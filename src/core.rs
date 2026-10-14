//! What the async and the blocking clients share: building events and deciding what to do with a response.

#![cfg_attr(not(any(feature = "tokio", feature = "blocking")), allow(dead_code))]

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::event::{Batch, PageView, RequestInfo, Track, WireEvent};
use crate::hash::visitor_hash;
use crate::util::{host_of_url, new_id, non_empty, normalize_ip, rfc3339, utc_day};

pub(crate) const USER_AGENT: &str = concat!("runomine-rust/", env!("CARGO_PKG_VERSION"));
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);

pub(crate) struct Core {
    pub token: String,
    pub active: bool,
    pub debug: bool,
    pub endpoint: String,
    pub flush_interval: Duration,
    pub batch_size: usize,
    pub max_queue: usize,
    pub retry_delays: Vec<Duration>,
    pub timeout: Duration,
}

/// What to do after an attempt to send a batch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Done,
    Retry(Duration),
    Drop,
}

impl Core {
    pub fn new(config: Config) -> Self {
        let batch_size = config.batch_size.clamp(1, 500);
        Core {
            active: config.is_active(),
            endpoint: config.endpoint(),
            token: config.token.trim().to_owned(),
            debug: config.debug,
            flush_interval: config.flush_interval.max(Duration::from_millis(1)),
            batch_size,
            max_queue: config.max_queue.max(batch_size),
            retry_delays: config.retry_delays,
            timeout: config.timeout,
        }
    }

    pub fn log(&self, message: impl fmt::Display) {
        if self.debug {
            eprintln!("[runomine] {message}");
        }
    }

    /// The visitor hash for this site's token, or `None` when the client has no token.
    pub fn visitor(&self, ip: &str, user_agent: &str, host: &str, day: &str) -> Option<String> {
        (!self.token.is_empty())
            .then(|| visitor_hash(&self.token, &normalize_ip(ip), user_agent, host, day))
    }

    /// `runomine_visitor` (when active and the request has an IP) and `runomine_user` (`user`, else the
    /// request's), only the keys with a value.
    pub fn checkout_metadata(
        &self,
        request: &RequestInfo,
        user: Option<String>,
        day: &str,
    ) -> BTreeMap<String, String> {
        let mut metadata = BTreeMap::new();
        let ip = request.ip.as_deref().map(str::trim).unwrap_or("");
        if self.active && !ip.is_empty() {
            let host = non_empty(request.host.clone())
                .unwrap_or_else(|| host_of_url(request.url.as_deref().unwrap_or("")));
            let user_agent = request.user_agent.as_deref().unwrap_or("");
            if let Some(visitor) = self.visitor(ip, user_agent, &host, day) {
                metadata.insert("runomine_visitor".to_owned(), visitor);
            }
        }
        let user = non_empty(user).or_else(|| non_empty(request.user.clone()));
        if let Some(user) = user {
            metadata.insert("runomine_user".to_owned(), user);
        }
        metadata
    }

    pub fn pageview(&self, pv: PageView) -> Option<WireEvent> {
        let url = pv.url.trim().to_owned();
        if url.is_empty() {
            self.log("pageview without a url, dropped");
            return None;
        }
        let ts = pv.ts.unwrap_or_else(SystemTime::now);
        let user_agent = pv.user_agent.unwrap_or_default();
        let visitor = match non_empty(pv.visitor) {
            Some(v) => v,
            None => {
                let host = non_empty(pv.host).unwrap_or_else(|| host_of_url(&url));
                self.visitor(
                    pv.ip.as_deref().unwrap_or(""),
                    &user_agent,
                    &host,
                    &utc_day(ts),
                )?
            }
        };
        Some(WireEvent {
            id: new_id(),
            kind: "pageview",
            name: None,
            ts: rfc3339(ts),
            url: Some(url),
            referrer: non_empty(pv.referrer),
            user_agent: Some(user_agent),
            visitor: Some(visitor),
            country: non_empty(pv.country),
            user: non_empty(pv.user),
            revenue: None,
            props: (!pv.props.is_empty()).then_some(pv.props),
        })
    }

    pub fn track(&self, name: &str, t: Track) -> Option<WireEvent> {
        let name = name.trim();
        if name.is_empty() {
            self.log("event without a name, dropped");
            return None;
        }
        let ts = t.ts.unwrap_or_else(SystemTime::now);
        let has_request = t.request.is_some();
        let request = t.request.unwrap_or_default();
        let url = t.url.or(non_empty(request.url));
        let user_agent = non_empty(request.user_agent);
        // No request, no visitor: never a placeholder (contract, API v1 `visitor`).
        let visitor = match t.visitor {
            Some(v) => Some(v),
            None if has_request => {
                let host = non_empty(request.host)
                    .unwrap_or_else(|| host_of_url(url.as_deref().unwrap_or("")));
                self.visitor(
                    request.ip.as_deref().unwrap_or(""),
                    user_agent.as_deref().unwrap_or(""),
                    &host,
                    &utc_day(ts),
                )
            }
            None => None,
        };
        Some(WireEvent {
            id: new_id(),
            kind: "event",
            name: Some(name.to_owned()),
            ts: rfc3339(ts),
            url,
            referrer: None,
            user_agent,
            visitor,
            country: non_empty(request.country),
            user: t.user.or(non_empty(request.user)),
            revenue: t.revenue,
            props: (!t.props.is_empty()).then_some(t.props),
        })
    }

    pub fn body(&self, events: &[WireEvent]) -> Option<Vec<u8>> {
        match serde_json::to_vec(&Batch { events }) {
            Ok(body) => Some(body),
            Err(err) => {
                self.log(format_args!(
                    "could not encode {} event(s), dropped: {err}",
                    events.len()
                ));
                None
            }
        }
    }

    /// Decides after attempt number `attempt` (0-based): `Ok(status)` with the `Retry-After` header and body, or
    /// `Err(network error)`.
    pub fn judge(
        &self,
        result: Result<(u16, Option<String>, String), String>,
        attempt: usize,
        batch: usize,
    ) -> Step {
        let (status, retry_after) = match result {
            Ok((status, _, body)) if (200..300).contains(&status) => {
                if self.debug && body.contains("\"rejected\"") {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        if let Some(rejected) = v.get("rejected").and_then(|r| r.as_array()) {
                            if !rejected.is_empty() {
                                self.log(format_args!(
                                    "{} event(s) rejected: {}",
                                    rejected.len(),
                                    serde_json::Value::Array(rejected.clone())
                                ));
                            }
                        }
                    }
                }
                return Step::Done;
            }
            Ok((status, retry_after, _)) => (Some(status), retry_after),
            Err(err) => {
                self.log(format_args!("request failed: {err}"));
                (None, None)
            }
        };
        let retryable = match status {
            None => true,
            Some(s) => s == 429 || s >= 500,
        };
        if !retryable || attempt >= self.retry_delays.len() {
            let why = status.map_or_else(|| "network error".to_owned(), |s| format!("status {s}"));
            let hint = if status == Some(401) {
                ", check RUNOMINE_TOKEN"
            } else {
                ""
            };
            self.log(format_args!("batch of {batch} dropped ({why}{hint})"));
            return Step::Drop;
        }
        let wait = retry_after
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|s| Duration::from_secs(s).min(MAX_RETRY_AFTER))
            .unwrap_or(self.retry_delays[attempt]);
        Step::Retry(wait)
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::event::RequestInfo;

    const TOKEN: &str = "rm_live_test_0123456789abcdef";
    const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15";

    fn core() -> Core {
        Core::new(Config::new(TOKEN))
    }

    /// 2026-10-13T18:04:05.123Z
    fn ts() -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(1_791_914_645_123)
    }

    #[test]
    fn pageview_matches_the_contract() {
        let event = core()
            .pageview(PageView {
                url: "https://example.com/pricing?utm_source=newsletter&utm_campaign=october"
                    .into(),
                ip: Some("::ffff:203.0.113.9".into()),
                user_agent: Some(UA.into()),
                referrer: Some("https://www.google.com/".into()),
                country: Some("ES".into()),
                user: Some("42".into()),
                ts: Some(ts()),
                ..Default::default()
            })
            .unwrap();
        let json = serde_json::to_value(&event).unwrap();
        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "country",
                "id",
                "referrer",
                "ts",
                "type",
                "url",
                "user",
                "user_agent",
                "visitor"
            ]
        );
        assert_eq!(json["type"], "pageview");
        assert_eq!(json["ts"], "2026-10-13T18:04:05.123Z");
        assert_eq!(
            json["visitor"], "5f9e1bebe31e8bf5",
            "the contract vector, IPv4-mapped IP normalized"
        );
        assert_eq!(json["user_agent"], UA);
        assert!(
            !json.to_string().contains("203.0.113.9"),
            "the IP is never sent"
        );
    }

    #[test]
    fn event_matches_the_contract() {
        let event = core()
            .track(
                "purchase",
                Track::new()
                    .prop("plan", "rent")
                    .prop("film", "the-third-man")
                    .prop("seats", 2)
                    .revenue(4.99, "eur")
                    .user(42)
                    .ts(ts())
                    .request(RequestInfo {
                        url: Some("https://example.com/checkout/success".into()),
                        ip: Some("203.0.113.9".into()),
                        user_agent: Some(UA.into()),
                        ..Default::default()
                    }),
            )
            .unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "event");
        assert_eq!(json["name"], "purchase");
        assert_eq!(json["url"], "https://example.com/checkout/success");
        assert_eq!(json["visitor"], "5f9e1bebe31e8bf5");
        assert_eq!(json["user"], "42");
        assert_eq!(
            json["revenue"],
            serde_json::json!({"amount": 4.99, "currency": "EUR"})
        );
        assert_eq!(
            json["props"],
            serde_json::json!({"plan": "rent", "film": "the-third-man", "seats": 2})
        );
        assert!(json.get("referrer").is_none());
    }

    #[test]
    fn pageview_props_are_sent_like_event_props() {
        let page = || PageView {
            url: "https://example.com/news".into(),
            ip: Some("203.0.113.9".into()),
            user_agent: Some(UA.into()),
            ..Default::default()
        };
        let event = core()
            .pageview(
                page()
                    .prop("section", "news")
                    .props([("team", 7), ("seats", 2)]),
            )
            .unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json["props"],
            serde_json::json!({"section": "news", "team": 7, "seats": 2})
        );
        let json = serde_json::to_value(core().pageview(page()).unwrap()).unwrap();
        assert!(json.get("props").is_none());
    }

    #[test]
    fn event_without_a_request_has_no_visitor() {
        let event = core()
            .track("purchase", Track::new().user("u_1").revenue(10.0, "EUR"))
            .unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("visitor").is_none());
        assert!(json.get("url").is_none());
        assert!(json.get("user_agent").is_none());
        assert!(json.get("props").is_none());
        // A URL alone is not a request either.
        let event = core()
            .track("signup", Track::new().url("https://example.com/"))
            .unwrap();
        assert!(event.visitor.is_none());
        // A visitor captured earlier is kept as is.
        let event = core()
            .track("purchase", Track::new().visitor("5f9e1bebe31e8bf5"))
            .unwrap();
        assert_eq!(event.visitor.as_deref(), Some("5f9e1bebe31e8bf5"));
    }

    #[test]
    fn checkout_metadata_matches_the_contract_vector() {
        let request = RequestInfo {
            url: Some("https://Example.com:8443/checkout".into()),
            ip: Some("203.0.113.9".into()),
            user_agent: Some(UA.into()),
            ..Default::default()
        };
        let meta = core().checkout_metadata(&request, None, "2026-10-13");
        assert_eq!(meta.len(), 1);
        assert_eq!(meta["runomine_visitor"], "5f9e1bebe31e8bf5");

        let meta = core().checkout_metadata(&request, Some(42.to_string()), "2026-10-13");
        assert_eq!(meta["runomine_visitor"], "5f9e1bebe31e8bf5");
        assert_eq!(meta["runomine_user"], "42");

        // The request's user, unless one is given.
        let with_user = RequestInfo {
            user: Some("acct_9".into()),
            ..request.clone()
        };
        let meta = core().checkout_metadata(&with_user, None, "2026-10-13");
        assert_eq!(meta["runomine_user"], "acct_9");
        let meta = core().checkout_metadata(&with_user, Some("7".into()), "2026-10-13");
        assert_eq!(meta["runomine_user"], "7");
    }

    #[test]
    fn checkout_metadata_leaves_out_what_is_unknown() {
        // No IP (a job, a webhook): no visitor, never a made-up one.
        let no_ip = RequestInfo {
            url: Some("https://example.com/".into()),
            user_agent: Some(UA.into()),
            ..Default::default()
        };
        let meta = core().checkout_metadata(&no_ip, Some("42".into()), "2026-10-13");
        assert_eq!(
            meta.into_iter().collect::<Vec<_>>(),
            [("runomine_user".to_owned(), "42".to_owned())]
        );
        assert!(
            core()
                .checkout_metadata(&RequestInfo::default(), Some(String::new()), "2026-10-13")
                .is_empty()
        );

        // Disabled: the user still goes, the visitor doesn't.
        let request = RequestInfo {
            ip: Some("203.0.113.9".into()),
            host: Some("example.com".into()),
            ..Default::default()
        };
        let disabled = Core::new(Config {
            enabled: false,
            ..Config::new(TOKEN)
        });
        assert!(
            disabled
                .checkout_metadata(&request, None, "2026-10-13")
                .is_empty()
        );
        let no_token = Core::new(Config::new(""));
        let meta = no_token.checkout_metadata(&request, Some("u1".into()), "2026-10-13");
        assert_eq!(meta.len(), 1);
        assert_eq!(meta["runomine_user"], "u1");
    }

    #[test]
    fn bad_input_is_dropped_not_sent() {
        assert!(core().pageview(PageView::default()).is_none());
        assert!(core().track("  ", Track::new()).is_none());
        let t = Track::new().revenue(f64::NAN, "EUR").revenue(1.0, "");
        assert!(t.revenue.is_none());
    }

    #[test]
    fn retries_5xx_429_and_network_errors_then_drops() {
        let c = core();
        let ok = |s: u16| Ok((s, None, String::new()));
        assert_eq!(c.judge(ok(202), 0, 1), Step::Done);
        assert_eq!(c.judge(ok(503), 0, 1), Step::Retry(Duration::from_secs(1)));
        assert_eq!(c.judge(ok(429), 1, 1), Step::Retry(Duration::from_secs(3)));
        assert_eq!(c.judge(ok(500), 2, 1), Step::Drop);
        assert_eq!(
            c.judge(Err("refused".into()), 0, 1),
            Step::Retry(Duration::from_secs(1))
        );
        assert_eq!(c.judge(ok(401), 0, 1), Step::Drop);
        assert_eq!(c.judge(ok(400), 0, 1), Step::Drop);
        assert_eq!(
            c.judge(Ok((429, Some("60".into()), String::new())), 0, 1),
            Step::Retry(Duration::from_secs(10))
        );
    }
}
