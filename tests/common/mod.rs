//! A fake runomine collector: `POST /api/v1/events` that records what it gets and answers with a scripted status.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};

pub const TOKEN: &str = "rm_live_test_0123456789abcdef";
pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15";

#[derive(Clone, Debug)]
pub struct Hit {
    pub authorization: String,
    pub content_type: String,
    pub body: Value,
    pub status: u16,
}

impl Hit {
    pub fn events(&self) -> Vec<Value> {
        self.body["events"].as_array().cloned().unwrap_or_default()
    }
}

#[derive(Clone, Default)]
struct Shared {
    hits: Arc<Mutex<Vec<Hit>>>,
    statuses: Arc<Mutex<VecDeque<u16>>>,
    delay: Arc<Mutex<Duration>>,
}

#[derive(Clone)]
pub struct Collector {
    pub url: String,
    shared: Shared,
}

async fn events(
    State(s): State<Shared>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, Json<Value>) {
    let delay = *s.delay.lock().unwrap();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let status = s.statuses.lock().unwrap().pop_front().unwrap_or(202);
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let n = body["events"].as_array().map_or(0, Vec::len);
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    s.hits.lock().unwrap().push(Hit {
        authorization: get("authorization"),
        content_type: get("content-type"),
        body,
        status,
    });
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({"accepted": n, "rejected": []})),
    )
}

impl Collector {
    /// Starts on the current tokio runtime. Answers the given statuses in order, then 202.
    pub async fn start(statuses: &[u16]) -> Collector {
        let shared = Shared::default();
        shared.statuses.lock().unwrap().extend(statuses);
        let app = Router::new()
            .route("/api/v1/events", post(events))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Collector { url, shared }
    }

    pub fn set_delay(&self, delay: Duration) {
        *self.shared.delay.lock().unwrap() = delay;
    }

    pub fn hits(&self) -> Vec<Hit> {
        self.shared.hits.lock().unwrap().clone()
    }

    /// Events of the accepted (2xx) requests.
    pub fn events(&self) -> Vec<Value> {
        self.hits()
            .iter()
            .filter(|h| (200..300).contains(&h.status))
            .flat_map(Hit::events)
            .collect()
    }

    /// Waits (polling, no runtime needed) until `n` events were accepted, or `timeout`.
    pub fn wait_for_events(&self, n: usize, timeout: Duration) -> Vec<Value> {
        let start = Instant::now();
        loop {
            let events = self.events();
            if events.len() >= n || start.elapsed() > timeout {
                return events;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub async fn wait_for_events_async(&self, n: usize, timeout: Duration) -> Vec<Value> {
        let start = Instant::now();
        loop {
            let events = self.events();
            if events.len() >= n || start.elapsed() > timeout {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// A test config: fast retries, a long interval (so only `flush` or a full batch sends).
pub fn config(url: &str) -> runomine::Config {
    runomine::Config {
        url: url.to_owned(),
        retry_delays: vec![Duration::from_millis(20), Duration::from_millis(20)],
        flush_interval: Duration::from_secs(3600),
        timeout: Duration::from_secs(2),
        ..runomine::Config::new(TOKEN)
    }
}

/// A local URL nothing listens on.
pub fn unreachable_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
}
