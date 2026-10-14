//! Framework-agnostic request helpers: which requests are pageviews, and what to take from them.
//!
//! The axum, tower and actix-web integrations are built on these; use them directly for any other framework
//! (implement [`Headers`] for its header map, call [`is_pageview_request`] before the handler,
//! [`is_pageview_response`] after it, and [`request_info`] to build the pageview). No classification lives here:
//! sources, bots and devices are worked out by runomine.

use std::net::IpAddr;

use crate::event::RequestInfo;
use crate::util::{normalize_host, normalize_ip};

/// Read access to a request's headers. Names are lowercase.
pub trait Headers {
    /// The first value of the header, if it is valid text.
    fn get(&self, name: &str) -> Option<&str>;
    /// The last value of the header when it appears several times (default: the first).
    fn get_last(&self, name: &str) -> Option<&str> {
        self.get(name)
    }
}

impl<H: Headers + ?Sized> Headers for &H {
    fn get(&self, name: &str) -> Option<&str> {
        (**self).get(name)
    }
    fn get_last(&self, name: &str) -> Option<&str> {
        (**self).get_last(name)
    }
}

/// `(name, value)` pairs, e.g. `[("user-agent", "Mozilla/5.0 …")]`.
impl Headers for [(&str, &str)] {
    fn get(&self, name: &str) -> Option<&str> {
        self.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
    }
    fn get_last(&self, name: &str) -> Option<&str> {
        self.iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
    }
}

#[cfg(feature = "tower")]
impl Headers for http::HeaderMap {
    fn get(&self, name: &str) -> Option<&str> {
        http::HeaderMap::get(self, name).and_then(|v| v.to_str().ok())
    }
    fn get_last(&self, name: &str) -> Option<&str> {
        self.get_all(name)
            .iter()
            .next_back()
            .and_then(|v| v.to_str().ok())
    }
}

/// Where the visitor's IP comes from. It is only hashed, on your server, and never sent.
///
/// **Trust:** a header is only as trustworthy as the proxy that sets it. Pick a header only when every request
/// passes through a proxy that sets or overwrites it; otherwise anyone can send it. A forged IP only changes the
/// visitor hash (it can inflate your unique-visitor count), never anything else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ClientIp {
    /// The TCP peer (axum's `ConnectInfo<SocketAddr>`, actix-web's `peer_addr()`). Right when nothing sits in front
    /// of your app; behind a proxy every visitor would share the proxy's IP.
    #[default]
    Peer,
    /// The first entry of this header: `CF-Connecting-IP` behind Cloudflare, `X-Real-IP` behind nginx,
    /// `Fly-Client-IP` on Fly.io, `X-Forwarded-For` when your proxy overwrites it. Falls back to the peer.
    Header(String),
    /// The right-most `X-Forwarded-For` entry: the address your one proxy saw (Railway, Heroku, Render, a load
    /// balancer that appends). Earlier entries can be forged by the client. Falls back to the peer.
    RightmostForwardedFor,
}

impl ClientIp {
    /// [`ClientIp::Header`] with this header name.
    pub fn header(name: impl Into<String>) -> Self {
        ClientIp::Header(name.into().to_ascii_lowercase())
    }
}

/// Settings shared by the integrations.
#[cfg(any(feature = "tower", feature = "actix-web"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub client_ip: ClientIp,
    pub skip: Vec<String>,
    pub forwarded_host: bool,
}

#[cfg(any(feature = "tower", feature = "actix-web"))]
impl Default for Settings {
    fn default() -> Self {
        Settings {
            client_ip: ClientIp::Peer,
            skip: Vec::new(),
            forwarded_host: true,
        }
    }
}

/// The parts of a request [`request_info`] reads.
pub struct RequestParts<'a, H: Headers + ?Sized> {
    pub headers: &'a H,
    /// `"http"`/`"https"` when the server knows it (`X-Forwarded-Proto` wins when forwarded headers are trusted).
    pub scheme: Option<&'a str>,
    /// The URI's authority (`host[:port]`): HTTP/2 requests carry the host there instead of `Host`.
    pub authority: Option<&'a str>,
    /// Path and query, e.g. `"/pricing?utm_source=x"`.
    pub path_and_query: &'a str,
    /// The TCP peer.
    pub peer: Option<IpAddr>,
}

impl<H: Headers + ?Sized> Clone for RequestParts<'_, H> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<H: Headers + ?Sized> Copy for RequestParts<'_, H> {}

/// Builds the [`RequestInfo`] of a request: URL (`X-Forwarded-Proto`/`X-Forwarded-Host` first when
/// `forwarded_host` is true, then the scheme, `Host` and the authority), IP per `client_ip`, user agent,
/// referrer and country (`CF-IPCountry`, `CloudFront-Viewer-Country`).
pub fn request_info<H: Headers + ?Sized>(
    parts: RequestParts<'_, H>,
    client_ip: &ClientIp,
    forwarded_host: bool,
) -> RequestInfo {
    let h = parts.headers;
    let forwarded = |name: &str| {
        if forwarded_host {
            first(h.get(name))
        } else {
            None
        }
    };
    let host = forwarded("x-forwarded-host")
        .or_else(|| h.get("host").map(str::trim).filter(|v| !v.is_empty()))
        .or(parts.authority);
    let scheme = forwarded("x-forwarded-proto")
        .or(parts.scheme)
        .unwrap_or("http")
        .to_ascii_lowercase();
    let path = if parts.path_and_query.starts_with('/') {
        parts.path_and_query.to_owned()
    } else {
        format!("/{}", parts.path_and_query)
    };
    let ip = match client_ip {
        ClientIp::Peer => None,
        ClientIp::Header(name) => first(h.get(name)).map(str::to_owned),
        ClientIp::RightmostForwardedFor => h
            .get_last("x-forwarded-for")
            .and_then(|v| v.rsplit(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned),
    }
    .or_else(|| parts.peer.map(|p| p.to_string()))
    .map(|ip| normalize_ip(&ip));
    RequestInfo {
        url: host.map(|host| format!("{scheme}://{host}{path}")),
        ip,
        user_agent: h.get("user-agent").map(str::to_owned),
        referrer: h
            .get("referer")
            .map(str::to_owned)
            .filter(|v| !v.is_empty()),
        country: country(h),
        host: host.map(normalize_host),
        user: None,
    }
}

fn first(value: Option<&str>) -> Option<&str> {
    value
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// ISO 3166-1 alpha-2 from `CF-IPCountry` or `CloudFront-Viewer-Country`. Cloudflare's `XX` (unknown) and `T1`
/// (Tor) are not countries.
pub fn country<H: Headers + ?Sized>(headers: &H) -> Option<String> {
    ["cf-ipcountry", "cloudfront-viewer-country"]
        .iter()
        .find_map(|name| {
            let value = headers.get(name)?.trim().to_ascii_uppercase();
            (value.len() == 2 && value.bytes().all(|b| b.is_ascii_uppercase()) && value != "XX")
                .then_some(value)
        })
}

/// A speculative load the user may never see: `Purpose`, `Sec-Purpose`, `X-Purpose` or `X-Moz` saying
/// `prefetch`, `prerender` or `preview`.
pub fn is_prefetch<H: Headers + ?Sized>(headers: &H) -> bool {
    ["sec-purpose", "purpose", "x-purpose", "x-moz"]
        .iter()
        .any(|name| {
            headers.get(name).is_some_and(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("prefetch") || v.contains("prerender") || v.contains("preview")
            })
        })
}

/// `/logo.png`, `/app.js`, `/robots.txt`: assets, not pages. `/page.html` and `/about` are pages.
pub fn is_asset_path(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or("");
    match last.rsplit_once('.') {
        Some((_, ext)) => {
            !ext.is_empty()
                && ext.len() <= 8
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                && !ext.eq_ignore_ascii_case("html")
                && !ext.eq_ignore_ascii_case("htm")
        }
        None => false,
    }
}

/// Whether `path` is skipped: `"/admin"` matches `/admin` and `/admin/…` (not `/administration`);
/// `"/api*"` matches anything starting with `/api`.
pub fn is_skipped<S: AsRef<str>>(path: &str, skip: &[S]) -> bool {
    skip.iter().any(|rule| {
        let rule = rule.as_ref();
        if let Some(prefix) = rule.strip_suffix('*') {
            return path.starts_with(prefix);
        }
        let base = rule.trim_end_matches('/');
        if base.is_empty() {
            return path == "/";
        }
        path == base
            || path
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The library's rule before the handler runs: a `GET`, not a prefetch, not an asset, not skipped.
pub fn is_pageview_request<H: Headers + ?Sized, S: AsRef<str>>(
    method: &str,
    path: &str,
    headers: &H,
    skip: &[S],
) -> bool {
    method.eq_ignore_ascii_case("GET")
        && !is_prefetch(headers)
        && !is_asset_path(path)
        && !is_skipped(path, skip)
}

/// The rule after the handler: answered `200` with `text/html`.
pub fn is_pageview_response(status: u16, content_type: Option<&str>) -> bool {
    status == 200 && content_type.is_some_and(|ct| ct.to_ascii_lowercase().contains("text/html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h<'a>(pairs: &'a [(&'a str, &'a str)]) -> &'a [(&'a str, &'a str)] {
        pairs
    }

    #[test]
    fn pageview_rules() {
        let none = h(&[]);
        let skip = ["/admin", "/api*"];
        assert!(is_pageview_request("GET", "/", none, &skip));
        assert!(is_pageview_request("get", "/pricing", none, &skip));
        assert!(is_pageview_request("GET", "/page.html", none, &skip));
        assert!(is_pageview_request("GET", "/administration", none, &skip));
        assert!(!is_pageview_request("POST", "/", none, &skip));
        assert!(!is_pageview_request("HEAD", "/", none, &skip));
        assert!(!is_pageview_request("GET", "/logo.png", none, &skip));
        assert!(!is_pageview_request("GET", "/static/app.js", none, &skip));
        assert!(!is_pageview_request("GET", "/robots.txt", none, &skip));
        assert!(!is_pageview_request("GET", "/admin", none, &skip));
        assert!(!is_pageview_request("GET", "/admin/stats", none, &skip));
        assert!(!is_pageview_request("GET", "/api/v1/x", none, &skip));
        assert!(!is_pageview_request(
            "GET",
            "/",
            h(&[("Sec-Purpose", "prefetch;prerender")]),
            &skip
        ));
        assert!(!is_pageview_request(
            "GET",
            "/",
            h(&[("purpose", "prefetch")]),
            &skip
        ));
        assert!(!is_pageview_request(
            "GET",
            "/",
            h(&[("x-moz", "prefetch")]),
            &skip
        ));

        assert!(is_pageview_response(200, Some("text/html; charset=utf-8")));
        assert!(is_pageview_response(200, Some("Text/HTML")));
        assert!(!is_pageview_response(200, Some("application/json")));
        assert!(!is_pageview_response(200, None));
        assert!(!is_pageview_response(404, Some("text/html")));
        assert!(!is_pageview_response(304, Some("text/html")));
    }

    #[test]
    fn request_info_from_headers() {
        let headers = h(&[
            ("host", "Example.com:8080"),
            ("user-agent", "UA"),
            ("referer", "https://www.google.com/"),
            ("cf-ipcountry", "es"),
            ("x-forwarded-for", "198.51.100.1, 203.0.113.9"),
        ]);
        let parts = RequestParts {
            headers,
            scheme: None,
            authority: None,
            path_and_query: "/pricing?utm_source=x",
            peer: Some("10.0.0.1".parse().unwrap()),
        };
        let info = request_info(parts, &ClientIp::Peer, true);
        assert_eq!(
            info.url.as_deref(),
            Some("http://Example.com:8080/pricing?utm_source=x")
        );
        assert_eq!(info.host.as_deref(), Some("example.com"));
        assert_eq!(info.ip.as_deref(), Some("10.0.0.1"));
        assert_eq!(info.country.as_deref(), Some("ES"));
        assert_eq!(info.referrer.as_deref(), Some("https://www.google.com/"));
        let info = request_info(parts, &ClientIp::RightmostForwardedFor, true);
        assert_eq!(info.ip.as_deref(), Some("203.0.113.9"));
        let info = request_info(parts, &ClientIp::header("X-Forwarded-For"), true);
        assert_eq!(info.ip.as_deref(), Some("198.51.100.1"));
        let info = request_info(parts, &ClientIp::header("cf-connecting-ip"), true);
        assert_eq!(
            info.ip.as_deref(),
            Some("10.0.0.1"),
            "falls back to the peer"
        );
    }

    #[test]
    fn forwarded_host_and_proto_and_http2_authority() {
        let headers = h(&[
            ("x-forwarded-host", "shop.example.com"),
            ("x-forwarded-proto", "https"),
            ("host", "app:3000"),
        ]);
        let parts = RequestParts {
            headers,
            scheme: Some("http"),
            authority: None,
            path_and_query: "/",
            peer: None,
        };
        let info = request_info(parts, &ClientIp::Peer, true);
        assert_eq!(info.url.as_deref(), Some("https://shop.example.com/"));
        assert_eq!(info.ip, None);
        let info = request_info(parts, &ClientIp::Peer, false);
        assert_eq!(info.url.as_deref(), Some("http://app:3000/"));
        let parts = RequestParts {
            headers: h(&[]),
            scheme: Some("https"),
            authority: Some("example.com"),
            path_and_query: "/a",
            peer: None,
        };
        assert_eq!(
            request_info(parts, &ClientIp::Peer, true).url.as_deref(),
            Some("https://example.com/a")
        );
    }

    #[test]
    fn countries() {
        assert_eq!(country(h(&[("cf-ipcountry", "XX")])), None);
        assert_eq!(country(h(&[("cf-ipcountry", "T1")])), None);
        assert_eq!(
            country(h(&[("cloudfront-viewer-country", "fr")])).as_deref(),
            Some("FR")
        );
    }
}
