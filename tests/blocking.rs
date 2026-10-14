//! The blocking client (no async runtime in the caller) against a fake collector.

mod common;

use std::time::{Duration, Instant};

use common::{Collector, TOKEN, UA, config, unreachable_url};
use runomine::blocking::Client;
use runomine::{Config, PageView, Track, visitor_hash};

/// The collector runs on its own runtime threads; the test itself is plain sync code.
fn collector(statuses: &[u16]) -> (tokio::runtime::Runtime, Collector) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let collector = rt.block_on(Collector::start(statuses));
    (rt, collector)
}

#[test]
fn sends_pageviews_and_events_from_sync_code() {
    let (_rt, collector) = collector(&[]);
    let rom = Client::new(config(&collector.url));
    rom.pageview(PageView {
        url: "https://example.com/".into(),
        ip: Some("203.0.113.9".into()),
        user_agent: Some(UA.into()),
        ..Default::default()
    });
    rom.track(
        "export_finished",
        Track::new().user("42").prop("rows", 1200),
    );
    rom.flush();
    let hits = collector.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].authorization, format!("Bearer {TOKEN}"));
    let events = hits[0].events();
    assert_eq!(
        events[0]["visitor"],
        visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", &runomine::today()).as_str()
    );
    assert_eq!(events[1]["props"]["rows"], 1200);
    assert!(events[1].get("visitor").is_none(), "no request, no visitor");
}

#[test]
fn batches_retries_and_shutdown() {
    let (_rt, collector) = collector(&[503]);
    let rom = Client::new(config(&collector.url));
    for _ in 0..501 {
        rom.track("signup", Track::new());
    }
    rom.shutdown();
    let statuses: Vec<u16> = collector.hits().iter().map(|h| h.status).collect();
    assert_eq!(statuses, [503, 202, 202], "the first batch retried once");
    let sizes: Vec<usize> = collector.hits().iter().map(|h| h.events().len()).collect();
    assert_eq!(sizes, [500, 500, 1]);
    rom.track("after", Track::new());
    rom.flush();
    assert_eq!(collector.events().len(), 501);
}

#[test]
fn the_timer_flushes() {
    let (_rt, collector) = collector(&[]);
    let rom = Client::new(Config {
        flush_interval: Duration::from_millis(50),
        ..config(&collector.url)
    });
    rom.track("signup", Track::new());
    assert_eq!(
        collector.wait_for_events(1, Duration::from_secs(5)).len(),
        1
    );
}

#[test]
fn an_unreachable_server_is_never_an_error() {
    let rom = Client::new(config(&unreachable_url()));
    let start = Instant::now();
    rom.track("signup", Track::new());
    rom.shutdown();
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_disabled_client_starts_no_thread() {
    let rom = Client::new(Config::default());
    assert!(!rom.is_enabled());
    rom.track("signup", Track::new());
    rom.flush();
    rom.shutdown();
    assert!(!rom.worker_started());
}
