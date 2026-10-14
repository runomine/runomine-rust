//! actix-web: pageviews with the middleware, events with the `Rom` extractor, a flush when the server stops.
//!
//! ```sh
//! RUNOMINE_TOKEN=rm_live_… cargo run --example actix --features actix-web
//! curl -A 'Mozilla/5.0' http://127.0.0.1:8080/
//! curl -A 'Mozilla/5.0' -X POST http://127.0.0.1:8080/signup
//! ```

use std::time::Duration;

use actix_web::{App, HttpResponse, HttpServer, get, post};
use runomine::{Client, Rom, Track, actix::Runomine};

#[get("/")]
async fn home() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body("<h1>Welcome</h1>")
}

#[post("/signup")]
async fn signup(rom: Rom) -> HttpResponse {
    let user_id = 42; // … create the account …
    rom.identify(user_id);
    rom.track("signup", Track::new().prop("plan", "free"));
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body("<p>Thanks for signing up</p>")
}

#[post("/purchase")]
async fn purchase(rom: Rom) -> HttpResponse {
    rom.track(
        "purchase",
        Track::new().revenue(4.99, "EUR").prop("plan", "rent"),
    );
    HttpResponse::Ok().json(serde_json::json!({ "ok": true }))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let rom = Client::from_env();
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    let app_rom = rom.clone();
    HttpServer::new(move || {
        App::new()
            .wrap(Runomine::new(app_rom.clone()).skip(["/api*"]))
            .service(home)
            .service(signup)
            .service(purchase)
    })
    .workers(2)
    .bind(("127.0.0.1", port))?
    .run() // stops gracefully on SIGINT/SIGTERM
    .await?;

    let _ = tokio::time::timeout(Duration::from_secs(5), rom.shutdown()).await;
    Ok(())
}
