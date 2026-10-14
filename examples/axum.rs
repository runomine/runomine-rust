//! axum: pageviews with the layer, events with the `Rom` extractor, a flush on graceful shutdown.
//!
//! ```sh
//! RUNOMINE_TOKEN=rm_live_… cargo run --example axum --features axum
//! curl -A 'Mozilla/5.0' http://127.0.0.1:3000/            # a pageview
//! curl -A 'Mozilla/5.0' -X POST http://127.0.0.1:3000/signup  # an event with the visitor attached
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    Json, Router,
    response::Html,
    routing::{get, post},
};
use runomine::{Client, Rom, RunomineLayer, Track};

async fn home() -> Html<&'static str> {
    Html("<h1>Welcome</h1>")
}

async fn signup(rom: Rom) -> Html<&'static str> {
    let user_id = 42; // … create the account …
    rom.identify(user_id);
    rom.track("signup", Track::new().prop("plan", "free"));
    Html("<p>Thanks for signing up</p>")
}

async fn checkout(rom: Rom) -> Json<serde_json::Value> {
    // Keep the visitor with the order, for the payment webhook (see examples/plain_tokio.rs).
    Json(serde_json::json!({ "order": "o_1", "runomine_visitor": rom.visitor() }))
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // 1. A client from RUNOMINE_TOKEN / RUNOMINE_URL / RUNOMINE_ENABLED.
    let rom = Client::from_env();

    let app = Router::new()
        .route("/", get(home))
        .route("/signup", post(signup))
        .route("/checkout", post(checkout))
        .route("/api/health", get(|| async { "ok" }))
        // 2. The layer: pageviews for GET 200 text/html, and `Rom` for handlers.
        .layer(RunomineLayer::new(rom.clone()).skip(["/api*"]));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("listening on http://{}", listener.local_addr()?);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // 3. Send what is still queued before exiting (bounded, in case runomine is unreachable).
    let _ = tokio::time::timeout(Duration::from_secs(5), rom.shutdown()).await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
