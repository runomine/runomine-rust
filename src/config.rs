use std::fmt;
use std::time::Duration;

use crate::util::parse_bool;

/// Where events go when `RUNOMINE_URL` is not set.
pub const DEFAULT_URL: &str = "https://runomine.com";

/// Client settings. Build it with [`Config::from_env`] or [`Config::new`], and adjust fields with struct update
/// syntax: `Config { debug: true, ..Config::new(token) }`.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    /// Server token, `rm_live_…` (`RUNOMINE_TOKEN`). Without it the client does nothing.
    pub token: String,
    /// Base URL (`RUNOMINE_URL`, default `https://runomine.com`). Events go to `{url}/api/v1/events`.
    pub url: String,
    /// `RUNOMINE_ENABLED`, default true. A client without a token is always disabled.
    pub enabled: bool,
    /// Log dropped batches and rejected events to stderr (`RUNOMINE_DEBUG`, default false).
    pub debug: bool,
    /// Time between automatic flushes. Default 2 s.
    pub flush_interval: Duration,
    /// Events per request, 1..=500. Default 500 (the API's maximum).
    pub batch_size: usize,
    /// Events waiting in memory. When full, new events are dropped (and logged with `debug`). Default 10 000.
    pub max_queue: usize,
    /// Waits before each retry on 5xx, 429 or a network error. Default `[1 s, 3 s]`: two retries, then the batch
    /// is dropped quietly. A `Retry-After` in seconds (capped at 10 s) replaces the wait.
    pub retry_delays: Vec<Duration>,
    /// Per-request timeout. Default 5 s.
    pub timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            token: String::new(),
            url: DEFAULT_URL.to_owned(),
            enabled: true,
            debug: false,
            flush_interval: Duration::from_secs(2),
            batch_size: 500,
            max_queue: 10_000,
            retry_delays: vec![Duration::from_secs(1), Duration::from_secs(3)],
            timeout: Duration::from_secs(5),
        }
    }
}

impl Config {
    /// Defaults with this token.
    pub fn new(token: impl Into<String>) -> Self {
        Config {
            token: token.into(),
            ..Config::default()
        }
    }

    /// From `RUNOMINE_TOKEN`, `RUNOMINE_URL`, `RUNOMINE_ENABLED` and `RUNOMINE_DEBUG`.
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// Like [`Config::from_env`], reading variables through `var` (for tests, or another source of settings).
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let get = |name: &str| {
            var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let mut config = Config::new(get("RUNOMINE_TOKEN").unwrap_or_default());
        if let Some(url) = get("RUNOMINE_URL") {
            config.url = url;
        }
        if let Some(enabled) = get("RUNOMINE_ENABLED").and_then(|v| parse_bool(&v)) {
            config.enabled = enabled;
        }
        if let Some(debug) = get("RUNOMINE_DEBUG").and_then(|v| parse_bool(&v)) {
            config.debug = debug;
        }
        config
    }

    /// Whether a client with this config sends anything: enabled, with a token and an http(s) URL.
    pub fn is_active(&self) -> bool {
        self.enabled
            && !self.token.trim().is_empty()
            && (self.url.starts_with("https://") || self.url.starts_with("http://"))
    }

    pub(crate) fn endpoint(&self) -> String {
        format!("{}/api/v1/events", self.url.trim_end_matches('/'))
    }
}

/// The token is never printed.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let token = if self.token.is_empty() { "" } else { "rm_…" };
        f.debug_struct("Config")
            .field("token", &token)
            .field("url", &self.url)
            .field("enabled", &self.enabled)
            .field("debug", &self.debug)
            .field("flush_interval", &self.flush_interval)
            .field("batch_size", &self.batch_size)
            .field("max_queue", &self.max_queue)
            .field("retry_delays", &self.retry_delays)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn reads_the_environment() {
        let c = Config::from_vars(vars(&[
            ("RUNOMINE_TOKEN", "rm_live_x"),
            ("RUNOMINE_URL", "http://localhost:8000/"),
        ]));
        assert_eq!(c.token, "rm_live_x");
        assert!(c.is_active());
        assert_eq!(c.endpoint(), "http://localhost:8000/api/v1/events");
        assert!(!format!("{c:?}").contains("rm_live_x"));
    }

    #[test]
    fn defaults_and_switches() {
        let c = Config::from_vars(vars(&[]));
        assert_eq!(c.url, DEFAULT_URL);
        assert!(!c.is_active(), "no token, no client");
        let c = Config::from_vars(vars(&[
            ("RUNOMINE_TOKEN", "rm_live_x"),
            ("RUNOMINE_ENABLED", "false"),
        ]));
        assert!(!c.is_active());
        let c = Config::from_vars(vars(&[
            ("RUNOMINE_TOKEN", "rm_live_x"),
            ("RUNOMINE_DEBUG", "1"),
        ]));
        assert!(c.debug && c.is_active());
        assert_eq!(
            Config::new("t").endpoint(),
            "https://runomine.com/api/v1/events"
        );
    }
}
