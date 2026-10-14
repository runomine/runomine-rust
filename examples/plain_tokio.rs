//! Plain tokio, no web framework: events from a worker or a payment webhook, where there is no visitor request.
//!
//! ```sh
//! RUNOMINE_TOKEN=rm_live_… cargo run --example plain_tokio
//! ```

use std::time::Duration;

use runomine::{Client, Track};

struct Order {
    id: &'static str,
    user_id: u64,
    total: f64,
    plan: &'static str,
    /// Captured at checkout with `rom.visitor()` (axum/actix-web) or `client.visitor(ip, ua, host)`.
    runomine_visitor: Option<String>,
}

#[tokio::main]
async fn main() {
    let rom = Client::from_env();

    // At checkout, in the visitor's request, the app stored the visitor hash with the order.
    let paid = Order {
        id: "o_1",
        user_id: 42,
        total: 4.99,
        plan: "rent",
        runomine_visitor: rom.visitor("203.0.113.9", "Mozilla/5.0", "example.com"),
    };

    // Later, the payment webhook: the purchase joins the visit that started it.
    let mut details = Track::new()
        .revenue(paid.total, "EUR")
        .user(paid.user_id)
        .prop("plan", paid.plan)
        .prop("order", paid.id);
    if let Some(visitor) = &paid.runomine_visitor {
        details = details.visitor(visitor.clone());
    }
    rom.track("purchase", details);

    // A renewal from a scheduled job: no request and no stored visitor, so the event carries no `visitor`;
    // runomine attributes it through `user`.
    rom.track(
        "renewal",
        Track::new()
            .revenue(9.99, "EUR")
            .user(paid.user_id)
            .prop("plan", "monthly"),
    );

    // Short-lived process: send before exiting.
    let _ = tokio::time::timeout(Duration::from_secs(10), rom.shutdown()).await;
}
