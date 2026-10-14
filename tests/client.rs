//! The async client against a fake collector: payloads, batching, retries, failures, disabled.

mod common;

use std::time::{Duration, Instant};

use common::{Collector, TOKEN, UA, config, unreachable_url};
use runomine::{Client, Config, PageView, RequestInfo, Track, visitor_hash};
use serde_json::json;

#[tokio::test]
async fn payloads_match_the_contract() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    rom.pageview(PageView {
        url: "https://example.com/pricing?utm_source=newsletter&utm_campaign=october".into(),
        ip: Some("203.0.113.9".into()),
        user_agent: Some(UA.into()),
        referrer: Some("https://www.google.com/".into()),
        country: Some("ES".into()),
        user: Some("42".into()),
        ..Default::default()
    });
    rom.track(
        "purchase",
        Track::new()
            .revenue(4.99, "EUR")
            .user(42)
            .prop("plan", "rent")
            .prop("film", "the-third-man")
            .request(RequestInfo {
                url: Some("https://example.com/checkout/success".into()),
                ip: Some("203.0.113.9".into()),
                user_agent: Some(UA.into()),
                ..Default::default()
            }),
    );
    // A webhook: no request behind it.
    rom.track("purchase", Track::new().revenue(10.0, "EUR").user("42"));
    rom.flush().await;

    let hits = collector.hits();
    assert_eq!(hits.len(), 1, "one batch");
    assert_eq!(hits[0].authorization, format!("Bearer {TOKEN}"));
    assert_eq!(hits[0].content_type, "application/json");
    let events = hits[0].events();
    assert_eq!(events.len(), 3);
    let today = runomine::today();
    let visitor = visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", &today);

    let pv = &events[0];
    assert_eq!(pv["type"], "pageview");
    assert_eq!(
        pv["url"],
        "https://example.com/pricing?utm_source=newsletter&utm_campaign=october"
    );
    assert_eq!(pv["referrer"], "https://www.google.com/");
    assert_eq!(pv["user_agent"], UA);
    assert_eq!(pv["visitor"], visitor.as_str());
    assert_eq!(pv["country"], "ES");
    assert_eq!(pv["user"], "42");
    assert!(pv.get("name").is_none());
    let ts = pv["ts"].as_str().unwrap();
    assert!(
        ts.starts_with(&today) && ts.ends_with('Z') && ts.len() == 24,
        "{ts}"
    );
    let id = pv["id"].as_str().unwrap();
    assert_eq!((id.len(), &id[14..15]), (36, "7"), "UUID v7: {id}");

    let purchase = &events[1];
    assert_eq!(purchase["type"], "event");
    assert_eq!(purchase["name"], "purchase");
    assert_eq!(purchase["url"], "https://example.com/checkout/success");
    assert_eq!(purchase["visitor"], visitor.as_str());
    assert_eq!(
        purchase["revenue"],
        json!({"amount": 4.99, "currency": "EUR"})
    );
    assert_eq!(
        purchase["props"],
        json!({"plan": "rent", "film": "the-third-man"})
    );

    let webhook = events[2].as_object().unwrap();
    assert!(
        !webhook.contains_key("visitor"),
        "no request, no visitor: {webhook:?}"
    );
    assert!(!webhook.contains_key("url"));
    assert_eq!(webhook["user"], "42");

    assert!(
        !hits[0].body.to_string().contains("203.0.113.9"),
        "the IP never leaves the server"
    );
}

#[tokio::test]
async fn batches_hold_at_most_500_events() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    for i in 0..1201 {
        rom.track("signup", Track::new().user(i));
    }
    rom.flush().await;
    let sizes: Vec<usize> = collector.hits().iter().map(|h| h.events().len()).collect();
    assert_eq!(sizes, [500, 500, 201]);
}

#[tokio::test]
async fn a_full_batch_goes_without_waiting_for_the_timer() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url)); // one-hour interval
    for _ in 0..500 {
        rom.track("signup", Track::new());
    }
    let events = collector
        .wait_for_events_async(500, Duration::from_secs(5))
        .await;
    assert_eq!(events.len(), 500);
}

#[tokio::test]
async fn the_timer_flushes() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(Config {
        flush_interval: Duration::from_millis(50),
        ..config(&collector.url)
    });
    rom.track("signup", Track::new());
    let events = collector
        .wait_for_events_async(1, Duration::from_secs(5))
        .await;
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn retries_5xx_and_429_with_the_same_ids() {
    let collector = Collector::start(&[503, 429]).await;
    let rom = Client::new(config(&collector.url));
    rom.track("signup", Track::new());
    rom.flush().await;
    let hits = collector.hits();
    let statuses: Vec<u16> = hits.iter().map(|h| h.status).collect();
    assert_eq!(statuses, [503, 429, 202]);
    let ids: Vec<_> = hits.iter().map(|h| h.events()[0]["id"].clone()).collect();
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "a retried batch keeps its ids"
    );
}

#[tokio::test]
async fn gives_up_after_two_retries_and_carries_on() {
    let collector = Collector::start(&[500, 502, 503]).await;
    let rom = Client::new(config(&collector.url));
    rom.track("first", Track::new());
    rom.flush().await;
    assert_eq!(collector.hits().len(), 3, "one try and two retries");
    assert!(collector.events().is_empty(), "then dropped");
    rom.track("second", Track::new());
    rom.flush().await;
    let events = collector.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["name"], "second");
}

#[tokio::test]
async fn bad_token_and_bad_requests_are_not_retried() {
    for status in [401, 400, 422] {
        let collector = Collector::start(&[status]).await;
        let rom = Client::new(config(&collector.url));
        rom.track("signup", Track::new());
        rom.flush().await;
        assert_eq!(collector.hits().len(), 1, "status {status}");
    }
}

#[tokio::test]
async fn an_unreachable_server_is_never_an_error() {
    let rom = Client::new(config(&unreachable_url()));
    let start = Instant::now();
    rom.pageview(PageView {
        url: "https://example.com/".into(),
        ..Default::default()
    });
    rom.track("signup", Track::new());
    rom.flush().await;
    rom.track("after", Track::new());
    rom.shutdown().await;
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn shutdown_sends_what_is_queued_then_ignores_events() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    rom.track("before", Track::new());
    rom.shutdown().await;
    assert_eq!(collector.events().len(), 1);
    assert!(!rom.is_enabled());
    rom.track("after", Track::new());
    rom.flush().await;
    rom.shutdown().await;
    assert_eq!(collector.events().len(), 1);
}

#[tokio::test]
async fn a_full_queue_drops_new_events_without_blocking() {
    let collector = Collector::start(&[]).await;
    collector.set_delay(Duration::from_millis(300));
    let rom = Client::new(Config {
        batch_size: 1,
        max_queue: 1,
        ..config(&collector.url)
    });
    rom.track("e0", Track::new()); // the worker takes it and waits on the slow collector
    tokio::time::sleep(Duration::from_millis(100)).await;
    let start = Instant::now();
    for i in 1..=20 {
        rom.track(format!("e{i}"), Track::new()); // e1 waits in the queue; the rest find it full
    }
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "recording never blocks"
    );
    rom.flush().await;
    let names: Vec<_> = collector
        .events()
        .iter()
        .map(|e| e["name"].clone())
        .collect();
    assert_eq!(names, [json!("e0"), json!("e1")]);
}

#[tokio::test]
async fn a_disabled_client_does_nothing() {
    let collector = Collector::start(&[]).await;
    let off = |vars: Vec<(&str, String)>| {
        Client::new(Config::from_vars(move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.clone())
        }))
    };
    for rom in [
        Client::disabled(),
        off(vec![("RUNOMINE_URL", collector.url.clone())]), // no token
        Client::new(Config {
            enabled: false,
            ..config(&collector.url)
        }),
        off(vec![
            ("RUNOMINE_TOKEN", TOKEN.to_owned()),
            ("RUNOMINE_URL", collector.url.clone()),
            ("RUNOMINE_ENABLED", "false".to_owned()),
        ]),
    ] {
        assert!(!rom.is_enabled());
        rom.pageview(PageView {
            url: "https://example.com/".into(),
            ..Default::default()
        });
        rom.track("signup", Track::new());
        rom.flush().await;
        rom.shutdown().await;
        assert!(!rom.worker_started(), "no task is spawned");
    }
    assert!(collector.hits().is_empty());
}

#[test]
fn records_outside_a_tokio_runtime_too() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let collector = rt.block_on(Collector::start(&[]));
    let rom = Client::new(config(&collector.url));
    rom.track("signup", Track::new()); // no runtime on this thread
    rt.block_on(rom.flush());
    assert_eq!(collector.events().len(), 1);
}

#[test]
fn the_sender_outlives_the_runtime_that_recorded() {
    // Like actix-web: each worker's runtime stops before `main` calls `shutdown`.
    let collector_rt = tokio::runtime::Runtime::new().unwrap();
    let collector = collector_rt.block_on(Collector::start(&[]));
    let rom = Client::new(config(&collector.url));
    {
        let worker_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        worker_rt.block_on(async { rom.track("from_a_worker", Track::new()) });
    }
    collector_rt.block_on(rom.shutdown());
    assert_eq!(collector.events().len(), 1);
}

#[test]
fn visitor_helper_uses_the_token_and_today() {
    let rom = Client::new(Config::new(TOKEN));
    assert_eq!(
        rom.visitor("::ffff:203.0.113.9", UA, "Example.com:443")
            .unwrap(),
        visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", &runomine::today())
    );
}
