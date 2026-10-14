//! A tower [`Layer`] for any `http` 1.x stack (axum, hyper + tower, tonic-web, …).
//!
//! It records a pageview when a `GET` (not a prefetch, not an asset, not skipped) is answered `200` with
//! `text/html`, as soon as the response is produced: the event goes on the client's queue, nothing waits for the
//! network. It also puts the request's [`Rom`] handle in the request extensions.

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::{Request, Response, header};
use tower_layer::Layer;
use tower_service::Service;

use crate::client::Client;
use crate::rom::{Rom, RomContext, RomExt};
use crate::web::{
    ClientIp, RequestParts, Settings, is_pageview_request, is_pageview_response, request_info,
};

type UserFn = Arc<dyn Fn(&http::request::Parts) -> Option<String> + Send + Sync>;

/// Records pageviews and provides [`Rom`]. See the [module docs](self).
///
/// ```no_run
/// # #[cfg(feature = "axum")] {
/// use runomine::{Client, RunomineLayer};
///
/// let rom = Client::from_env();
/// let app: axum::Router = axum::Router::new()
///     // .route(…)
///     .layer(RunomineLayer::new(rom.clone()).skip(["/admin", "/api*"]));
/// # }
/// ```
#[derive(Clone)]
pub struct RunomineLayer {
    shared: Arc<Shared>,
}

#[derive(Clone)]
struct Shared {
    client: Client,
    settings: Settings,
    user: Option<UserFn>,
}

impl fmt::Debug for RunomineLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunomineLayer")
            .field("client", &self.shared.client)
            .field("settings", &self.shared.settings)
            .finish_non_exhaustive()
    }
}

/// With the process-wide [`crate::client()`] (from the environment).
impl Default for RunomineLayer {
    fn default() -> Self {
        Self::new(crate::client().clone())
    }
}

impl RunomineLayer {
    pub fn new(client: Client) -> Self {
        RunomineLayer {
            shared: Arc::new(Shared {
                client,
                settings: Settings::default(),
                user: None,
            }),
        }
    }

    /// Paths that are never pageviews: `"/admin"` (itself and below), `"/api*"` (prefix). Assets (any path with a
    /// file extension other than `.html`) and prefetches are always skipped. Replaces the list (default: empty).
    pub fn skip<S: Into<String>>(mut self, paths: impl IntoIterator<Item = S>) -> Self {
        Arc::make_mut(&mut self.shared).settings.skip = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Where the visitor's IP comes from. Default [`ClientIp::Peer`]; behind a proxy, see [`ClientIp`] for which
    /// header to trust.
    pub fn client_ip(mut self, client_ip: ClientIp) -> Self {
        Arc::make_mut(&mut self.shared).settings.client_ip = client_ip;
        self
    }

    /// Whether `X-Forwarded-Host`/`X-Forwarded-Proto` give the page URL's host and scheme (default true, so the
    /// URL is the public one behind a proxy). Turn it off if nothing in front of your app sets them.
    pub fn trust_forwarded_host(mut self, trust: bool) -> Self {
        Arc::make_mut(&mut self.shared).settings.forwarded_host = trust;
        self
    }

    /// Your user id for a request, from what earlier middleware put in it (e.g. an auth extension). Handlers can
    /// also call [`Rom::identify`].
    pub fn user<F>(mut self, user: F) -> Self
    where
        F: Fn(&http::request::Parts) -> Option<String> + Send + Sync + 'static,
    {
        Arc::make_mut(&mut self.shared).user = Some(Arc::new(user));
        self
    }
}

impl<S> Layer<S> for RunomineLayer {
    type Service = RunomineService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RunomineService {
            inner,
            shared: Arc::clone(&self.shared),
        }
    }
}

/// The service made by [`RunomineLayer`].
#[derive(Clone)]
pub struct RunomineService<S> {
    inner: S,
    shared: Arc<Shared>,
}

impl<S: fmt::Debug> fmt::Debug for RunomineService<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunomineService")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for RunomineService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        // Registered twice (or nested): the outer one does the work.
        if req.extensions().get::<RomExt>().is_some() {
            return ResponseFuture {
                inner: self.inner.call(req),
                pending: None,
            };
        }
        let (mut parts, body) = req.into_parts();
        let shared = &self.shared;
        let enabled = shared.client.is_enabled();
        let mut info = if enabled {
            info_from_parts(&parts, &shared.settings)
        } else {
            Default::default()
        };
        if enabled {
            if let Some(user) = &shared.user {
                info.user = catch_unwind(AssertUnwindSafe(|| user(&parts)))
                    .ok()
                    .flatten();
            }
        }
        let pageview = enabled
            && is_pageview_request(
                parts.method.as_str(),
                parts.uri.path(),
                &parts.headers,
                &shared.settings.skip,
            );
        let ext = RomExt {
            client: shared.client.clone(),
            ctx: Arc::new(RomContext::new(info)),
        };
        let pending = pageview.then(|| ext.clone());
        parts.extensions.insert(ext);
        ResponseFuture {
            inner: self.inner.call(Request::from_parts(parts, body)),
            pending,
        }
    }
}

pin_project_lite::pin_project! {
    /// Response future of [`RunomineService`].
    pub struct ResponseFuture<F> {
        #[pin]
        inner: F,
        pending: Option<RomExt>,
    }
}

impl<F, ResBody, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<Response<ResBody>, E>>,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let result = match this.inner.poll(cx) {
            Poll::Ready(result) => result,
            Poll::Pending => return Poll::Pending,
        };
        if let (Ok(res), Some(ext)) = (&result, this.pending.take()) {
            let content_type = res
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok());
            if is_pageview_response(res.status().as_u16(), content_type) {
                ext.ctx.record_pageview(&ext.client);
            }
        }
        Poll::Ready(result)
    }
}

/// The TCP peer: axum's `ConnectInfo<SocketAddr>`, or a plain `SocketAddr` extension (insert one in a hyper stack).
fn peer(extensions: &http::Extensions) -> Option<IpAddr> {
    #[cfg(feature = "axum")]
    if let Some(axum::extract::ConnectInfo(addr)) =
        extensions.get::<axum::extract::ConnectInfo<SocketAddr>>()
    {
        return Some(addr.ip());
    }
    extensions.get::<SocketAddr>().map(SocketAddr::ip)
}

pub(crate) fn info_from_parts(
    parts: &http::request::Parts,
    settings: &Settings,
) -> crate::RequestInfo {
    request_info(
        RequestParts {
            headers: &parts.headers,
            scheme: parts.uri.scheme_str(),
            authority: parts.uri.authority().map(|a| a.as_str()),
            path_and_query: parts.uri.path_and_query().map_or("/", |p| p.as_str()),
            peer: peer(&parts.extensions),
        },
        &settings.client_ip,
        settings.forwarded_host,
    )
}

/// The [`Rom`] of a request: the layer's, or one with the process-wide client and the TCP peer.
pub fn rom_for(parts: &http::request::Parts) -> Rom {
    match parts.extensions.get::<RomExt>() {
        Some(ext) => Rom::from_ext(ext),
        None => {
            let client = crate::client().clone();
            let info = if client.is_enabled() {
                info_from_parts(parts, &Settings::default())
            } else {
                Default::default()
            };
            Rom {
                client,
                ctx: Arc::new(RomContext::new(info)),
            }
        }
    }
}

/// The visitor hash of a request (see [`Rom::visitor`]).
pub fn visitor_for(parts: &http::request::Parts) -> Option<String> {
    rom_for(parts).visitor()
}
