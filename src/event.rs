//! What you record ([`PageView`], [`Track`]) and what is sent (the API v1 event).

use std::collections::BTreeMap;
use std::fmt;
use std::time::SystemTime;

use serde::Serialize;

/// The facts the library needs from a web request. Every field is optional; build it yourself for frameworks
/// without an integration, or get it from the axum/actix-web integrations.
///
/// The `ip` is only used for the visitor hash, on your server. It is never sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestInfo {
    /// Full URL of the page, e.g. `"https://example.com/pricing?utm_source=x"`.
    pub url: Option<String>,
    /// The visitor's IP. Hashed here, never sent.
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    /// ISO 3166-1 alpha-2 from a CDN header (`CF-IPCountry`, `CloudFront-Viewer-Country`).
    pub country: Option<String>,
    /// Hostname for the hash; defaults to the URL's.
    pub host: Option<String>,
    /// Your own user id, when known.
    pub user: Option<String>,
}

/// A pageview. Only `url` is required; the visitor hash is computed from `ip`, `user_agent` and `host` (or the
/// URL's host) unless you pass `visitor`.
///
/// ```
/// let pageview = runomine::PageView {
///     url: "https://example.com/pricing?utm_source=newsletter".into(),
///     ip: Some("203.0.113.9".into()),
///     user_agent: Some("Mozilla/5.0 …".into()),
///     referrer: Some("https://www.google.com/".into()),
///     ..Default::default()
/// };
/// # #[cfg(feature = "tokio")]
/// runomine::Client::from_env().pageview(pageview);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageView {
    pub url: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub country: Option<String>,
    pub host: Option<String>,
    pub user: Option<String>,
    /// A precomputed visitor hash; otherwise computed from `ip`, `user_agent` and `host`.
    pub visitor: Option<String>,
    /// Event time. Default: now.
    pub ts: Option<SystemTime>,
}

impl From<RequestInfo> for PageView {
    fn from(info: RequestInfo) -> Self {
        PageView {
            url: info.url.unwrap_or_default(),
            ip: info.ip,
            user_agent: info.user_agent,
            referrer: info.referrer,
            country: info.country,
            host: info.host,
            user: info.user,
            visitor: None,
            ts: None,
        }
    }
}

/// Revenue in major units (`4.99`, not `499`) and an ISO 4217 currency.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Revenue {
    pub amount: f64,
    pub currency: String,
}

/// A property value: text or a number (the contract allows nothing else).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Prop {
    Text(String),
    Number(serde_json::Number),
}

impl From<&str> for Prop {
    fn from(v: &str) -> Self {
        Prop::Text(v.to_owned())
    }
}
impl From<String> for Prop {
    fn from(v: String) -> Self {
        Prop::Text(v)
    }
}
impl From<&String> for Prop {
    fn from(v: &String) -> Self {
        Prop::Text(v.clone())
    }
}
impl From<bool> for Prop {
    fn from(v: bool) -> Self {
        Prop::Text(v.to_string())
    }
}
impl From<f64> for Prop {
    fn from(v: f64) -> Self {
        serde_json::Number::from_f64(v).map_or_else(|| Prop::Text(v.to_string()), Prop::Number)
    }
}
impl From<f32> for Prop {
    fn from(v: f32) -> Self {
        Prop::from(f64::from(v))
    }
}
macro_rules! int_props {
    ($($t:ty),*) => {$(
        impl From<$t> for Prop {
            fn from(v: $t) -> Self {
                Prop::Number(v.into())
            }
        }
    )*};
}
int_props!(i8, i16, i32, i64, u8, u16, u32, u64, isize, usize);

/// A business event's details, builder-style. Everything is optional.
///
/// ```
/// use runomine::Track;
/// let details = Track::new()
///     .prop("plan", "rent")
///     .prop("film", "the-third-man")
///     .revenue(4.99, "EUR")
///     .user(42);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub(crate) props: BTreeMap<String, Prop>,
    pub(crate) revenue: Option<Revenue>,
    pub(crate) user: Option<String>,
    pub(crate) visitor: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) ts: Option<SystemTime>,
    pub(crate) request: Option<RequestInfo>,
}

impl Track {
    pub fn new() -> Self {
        Self::default()
    }

    /// A property: a flat key with a string or number value (up to 20 per event; runomine stores them as dims).
    pub fn prop(mut self, key: impl Into<String>, value: impl Into<Prop>) -> Self {
        self.props.insert(key.into(), value.into());
        self
    }

    /// Several properties at once.
    pub fn props<K: Into<String>, V: Into<Prop>>(
        mut self,
        props: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        for (k, v) in props {
            self.props.insert(k.into(), v.into());
        }
        self
    }

    /// Revenue in major units (`4.99`, not `499`) and an ISO 4217 currency (`"EUR"`). A non-finite amount or an
    /// empty currency is left out.
    pub fn revenue(mut self, amount: f64, currency: impl Into<String>) -> Self {
        let currency: String = currency.into();
        self.revenue = (amount.is_finite() && !currency.trim().is_empty()).then(|| Revenue {
            amount,
            currency: currency.trim().to_ascii_uppercase(),
        });
        self
    }

    /// Your own user id (any `Display`: a number, a UUID, a string). It links events to the visits before login.
    pub fn user(mut self, user: impl fmt::Display) -> Self {
        self.user = Some(user.to_string()).filter(|u| !u.is_empty());
        self
    }

    /// A visitor hash captured earlier in the visitor's request (e.g. stored with the order at checkout), for an
    /// event recorded later without a request (a payment webhook). Takes precedence over [`Track::request`].
    pub fn visitor(mut self, visitor: impl Into<String>) -> Self {
        self.visitor = Some(visitor.into()).filter(|v| !v.is_empty());
        self
    }

    /// Overrides the URL taken from the request.
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into()).filter(|u| !u.is_empty());
        self
    }

    /// Event time. Default: now.
    pub fn ts(mut self, ts: SystemTime) -> Self {
        self.ts = Some(ts);
        self
    }

    /// The web request the event happened in: its visitor hash, URL, user agent and country are attached.
    /// Without a request (and without [`Track::visitor`]) the event carries no `visitor`; runomine attributes it
    /// through `user`.
    pub fn request(mut self, request: RequestInfo) -> Self {
        self.request = Some(request);
        self
    }
}

/// The event as sent to `POST /api/v1/events` (contract, "API v1"). Unknown fields are left out, never sent empty.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct WireEvent {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visitor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revenue: Option<Revenue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props: Option<BTreeMap<String, Prop>>,
}

#[derive(Serialize)]
pub(crate) struct Batch<'a> {
    pub events: &'a [WireEvent],
}
