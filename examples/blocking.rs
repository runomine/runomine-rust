//! Without an async runtime (a CLI, a sync worker): the `blocking` client sends from a background thread.
//!
//! ```sh
//! RUNOMINE_TOKEN=rm_live_… cargo run --example blocking --no-default-features --features blocking,rustls
//! ```

use runomine::blocking::Client;
use runomine::{PageView, Track};

fn main() {
    let rom = Client::from_env();

    // A page served by a sync framework (or a static-site generator's preview server).
    rom.pageview(PageView {
        url: "https://example.com/docs".into(),
        ip: Some("203.0.113.9".into()),
        user_agent: Some("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15".into()), // a bare "Mozilla/5.0" counts as a bot
        referrer: Some("https://www.google.com/".into()),
        ..Default::default()
    });

    // A business event from a batch job: no request, so no visitor.
    rom.track(
        "export_finished",
        Track::new().user("42").prop("rows", 1200),
    );

    // Blocks until sent (or given up on). Call it before the process exits.
    rom.shutdown();
}
