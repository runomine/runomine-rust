//! actix-web 4: the [`Runomine`] middleware records pageviews, and [`Rom`] is an extractor.
//!
//! ```no_run
//! use actix_web::{App, HttpServer, post};
//! use runomine::{Rom, Track, actix::Runomine};
//!
//! #[post("/signup")]
//! async fn signup(rom: Rom) -> &'static str {
//!     rom.identify("42");
//!     rom.track("signup", Track::new().prop("plan", "free"));
//!     "ok"
//! }
//!
//! #[actix_web::main]
//! async fn main() -> std::io::Result<()> {
//!     let rom = runomine::Client::from_env();
//!     let app_rom = rom.clone();
//!     HttpServer::new(move || App::new().wrap(Runomine::new(app_rom.clone())).service(signup))
//!         .bind(("0.0.0.0", 8080))?
//!         .run()
//!         .await?;
//!     rom.shutdown().await;
//!     Ok(())
//! }
//! ```

use std::fmt;
use std::future::{Future, Ready, ready};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use actix_web::dev::{Payload, Service, ServiceRequest, ServiceResponse, Transform, forward_ready};
use actix_web::http::header::{CONTENT_TYPE, HeaderMap};
use actix_web::{Error, FromRequest, HttpMessage, HttpRequest};

use crate::client::Client;
use crate::rom::{Rom, RomContext, RomExt};
use crate::web::{
    ClientIp, Headers, RequestParts, Settings, is_pageview_request, is_pageview_response,
    request_info,
};

type UserFn = Arc<dyn Fn(&ServiceRequest) -> Option<String> + Send + Sync>;

/// actix-web's header map, for [`crate::web`]'s helpers.
struct ActixHeaders<'a>(&'a HeaderMap);

impl Headers for ActixHeaders<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(|v| v.to_str().ok())
    }
    fn get_last(&self, name: &str) -> Option<&str> {
        self.0.get_all(name).last().and_then(|v| v.to_str().ok())
    }
}

/// Middleware for `App::wrap`: records a pageview when a `GET` (not a prefetch, not an asset, not skipped) is
/// answered `200` with `text/html`, and provides [`Rom`]. Nothing waits for the network.
#[derive(Clone)]
pub struct Runomine {
    shared: Arc<Shared>,
}

#[derive(Clone)]
struct Shared {
    client: Client,
    settings: Settings,
    user: Option<UserFn>,
}

impl fmt::Debug for Runomine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runomine")
            .field("client", &self.shared.client)
            .field("settings", &self.shared.settings)
            .finish_non_exhaustive()
    }
}

/// With the process-wide [`crate::client()`] (from the environment).
impl Default for Runomine {
    fn default() -> Self {
        Self::new(crate::client().clone())
    }
}

impl Runomine {
    pub fn new(client: Client) -> Self {
        Runomine {
            shared: Arc::new(Shared {
                client,
                settings: Settings::default(),
                user: None,
            }),
        }
    }

    /// Paths that are never pageviews: `"/admin"` (itself and below), `"/api*"` (prefix). Assets and prefetches
    /// are always skipped. Replaces the list (default: empty).
    pub fn skip<S: Into<String>>(mut self, paths: impl IntoIterator<Item = S>) -> Self {
        Arc::make_mut(&mut self.shared).settings.skip = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Where the visitor's IP comes from. Default [`ClientIp::Peer`] (`peer_addr()`); behind a proxy, see
    /// [`ClientIp`].
    pub fn client_ip(mut self, client_ip: ClientIp) -> Self {
        Arc::make_mut(&mut self.shared).settings.client_ip = client_ip;
        self
    }

    /// Whether `X-Forwarded-Host`/`X-Forwarded-Proto` give the page URL's host and scheme (default true).
    pub fn trust_forwarded_host(mut self, trust: bool) -> Self {
        Arc::make_mut(&mut self.shared).settings.forwarded_host = trust;
        self
    }

    /// Your user id for a request, from what earlier middleware put in it. Handlers can also call
    /// [`Rom::identify`].
    pub fn user<F>(mut self, user: F) -> Self
    where
        F: Fn(&ServiceRequest) -> Option<String> + Send + Sync + 'static,
    {
        Arc::make_mut(&mut self.shared).user = Some(Arc::new(user));
        self
    }
}

impl<S, B> Transform<S, ServiceRequest> for Runomine
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = RunomineMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(RunomineMiddleware {
            service: Rc::new(service),
            shared: Arc::clone(&self.shared),
        }))
    }
}

/// The service made by [`Runomine`].
pub struct RunomineMiddleware<S> {
    service: Rc<S>,
    shared: Arc<Shared>,
}

fn info(
    headers: &HeaderMap,
    uri: &actix_web::http::Uri,
    secure: bool,
    peer: Option<std::net::SocketAddr>,
    settings: &Settings,
) -> crate::RequestInfo {
    request_info(
        RequestParts {
            headers: &ActixHeaders(headers),
            scheme: Some(
                uri.scheme_str()
                    .unwrap_or(if secure { "https" } else { "http" }),
            ),
            authority: uri.authority().map(|a| a.as_str()),
            path_and_query: uri.path_and_query().map_or("/", |p| p.as_str()),
            peer: peer.map(|p| p.ip()),
        },
        &settings.client_ip,
        settings.forwarded_host,
    )
}

impl<S, B> Service<ServiceRequest> for RunomineMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let shared = &self.shared;
        let mut pending = None;
        if req.extensions().get::<RomExt>().is_none() {
            let enabled = shared.client.is_enabled();
            let mut info = if enabled {
                info(
                    req.headers(),
                    req.uri(),
                    req.app_config().secure(),
                    req.peer_addr(),
                    &shared.settings,
                )
            } else {
                Default::default()
            };
            if enabled {
                if let Some(user) = &shared.user {
                    info.user = catch_unwind(AssertUnwindSafe(|| user(&req))).ok().flatten();
                }
            }
            let pageview = enabled
                && is_pageview_request(
                    req.method().as_str(),
                    req.path(),
                    &ActixHeaders(req.headers()),
                    &shared.settings.skip,
                );
            let ext = RomExt {
                client: shared.client.clone(),
                ctx: Arc::new(RomContext::new(info)),
            };
            if pageview {
                pending = Some(ext.clone());
            }
            req.extensions_mut().insert(ext);
        }
        let fut = self.service.call(req);
        Box::pin(async move {
            let res = fut.await?;
            if let Some(ext) = pending {
                let content_type = res
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok());
                if is_pageview_response(res.status().as_u16(), content_type) {
                    ext.ctx.record_pageview(&ext.client);
                }
            }
            Ok(res)
        })
    }
}

/// The [`Rom`] of a request: the middleware's, or one with the process-wide client and the TCP peer.
pub fn rom_for(req: &HttpRequest) -> Rom {
    if let Some(ext) = req.extensions().get::<RomExt>() {
        return Rom::from_ext(ext);
    }
    let client = crate::client().clone();
    let info = if client.is_enabled() {
        info(
            req.headers(),
            req.uri(),
            req.app_config().secure(),
            req.peer_addr(),
            &Settings::default(),
        )
    } else {
        Default::default()
    };
    Rom {
        client,
        ctx: Arc::new(RomContext::new(info)),
    }
}

/// The visitor hash of a request (see [`Rom::visitor`]).
pub fn visitor_for(req: &HttpRequest) -> Option<String> {
    rom_for(req).visitor()
}

impl FromRequest for Rom {
    type Error = Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(Ok(rom_for(req)))
    }
}
