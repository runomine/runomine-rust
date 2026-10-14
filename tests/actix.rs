//! The actix-web middleware and `Rom` extractor, against a fake collector.

mod common;

use actix_web::{App, HttpResponse, test, web};
use common::{Collector, TOKEN, UA, config};
use runomine::actix::Runomine;
use runomine::{Client, ClientIp, Rom, Track, visitor_hash};
use serde_json::{Value, json};

async fn page() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body("<h1>hi</h1>")
}

async fn signup(rom: Rom) -> HttpResponse {
    rom.identify(42);
    rom.track("signup", Track::new().prop("plan", "free"));
    HttpResponse::Ok()
        .content_type("text/html")
        .body("<p>welcome</p>")
}

async fn checkout(rom: Rom) -> String {
    rom.visitor().unwrap_or_default()
}

fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(page))
        .route("/", web::post().to(page))
        .route("/logo.png", web::get().to(page))
        .route("/admin", web::get().to(page))
        .route(
            "/json",
            web::get().to(|| async { HttpResponse::Ok().json(json!({"ok": true})) }),
        )
        .route(
            "/missing",
            web::get().to(|| async {
                HttpResponse::NotFound()
                    .content_type("text/html")
                    .body("404")
            }),
        )
        .route("/welcome", web::get().to(signup))
        .route("/signup", web::post().to(signup))
        .route("/checkout", web::get().to(checkout));
}

fn req(method: &str, path: &str) -> test::TestRequest {
    let r = match method {
        "POST" => test::TestRequest::post(),
        _ => test::TestRequest::get(),
    };
    r.uri(path)
        .insert_header(("host", "example.com"))
        .insert_header(("user-agent", UA))
        .peer_addr("203.0.113.9:50000".parse().unwrap())
}

#[actix_web::test]
async fn records_html_pageviews_and_events_with_the_visitor() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    let app = test::init_service(
        App::new()
            .wrap(Runomine::new(rom.clone()).skip(["/admin"]))
            .configure(routes),
    )
    .await;
    test::call_service(
        &app,
        req("GET", "/?utm_source=x")
            .insert_header(("referer", "https://www.google.com/"))
            .insert_header(("cf-ipcountry", "DE"))
            .to_request(),
    )
    .await;
    for (method, path) in [
        ("POST", "/"),
        ("GET", "/json"),
        ("GET", "/missing"),
        ("GET", "/logo.png"),
        ("GET", "/admin"),
    ] {
        test::call_service(&app, req(method, path).to_request()).await;
    }
    test::call_service(
        &app,
        req("GET", "/")
            .insert_header(("sec-purpose", "prefetch"))
            .to_request(),
    )
    .await;
    test::call_service(&app, req("POST", "/signup").to_request()).await;
    test::call_service(&app, req("GET", "/welcome").to_request()).await;
    let res = test::call_service(&app, req("GET", "/checkout").to_request()).await;
    let visitor = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    rom.flush().await;

    let expected = visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", &runomine::today());
    assert_eq!(visitor, expected);
    let events = collector.events();
    let pageviews: Vec<&Value> = events.iter().filter(|e| e["type"] == "pageview").collect();
    let urls: Vec<&str> = pageviews
        .iter()
        .map(|e| e["url"].as_str().unwrap())
        .collect();
    assert_eq!(
        urls,
        [
            "http://example.com/?utm_source=x",
            "http://example.com/welcome"
        ]
    );
    assert_eq!(pageviews[0]["visitor"], expected.as_str());
    assert_eq!(pageviews[0]["country"], "DE");
    assert_eq!(pageviews[0]["referrer"], "https://www.google.com/");
    assert_eq!(
        pageviews[1]["user"], "42",
        "Rom::identify reaches the pageview"
    );
    let signups: Vec<&Value> = events.iter().filter(|e| e["name"] == "signup").collect();
    assert_eq!(signups.len(), 2);
    assert_eq!(signups[0]["visitor"], expected.as_str());
    assert_eq!(signups[0]["url"], "http://example.com/signup");
    assert_eq!(signups[0]["user"], "42");
}

#[actix_web::test]
async fn trusted_header_and_disabled_client() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    let app = test::init_service(
        App::new()
            .wrap(Runomine::new(rom.clone()).client_ip(ClientIp::header("cf-connecting-ip")))
            .configure(routes),
    )
    .await;
    test::call_service(
        &app,
        req("GET", "/")
            .insert_header(("cf-connecting-ip", "198.51.100.7"))
            .to_request(),
    )
    .await;
    rom.flush().await;
    assert_eq!(
        collector.events()[0]["visitor"],
        visitor_hash(TOKEN, "198.51.100.7", UA, "example.com", &runomine::today()).as_str()
    );

    let off = Client::disabled();
    let app = test::init_service(
        App::new()
            .wrap(Runomine::new(off.clone()))
            .configure(routes),
    )
    .await;
    let res = test::call_service(&app, req("POST", "/signup").to_request()).await;
    assert!(res.status().is_success());
    off.flush().await;
    assert!(!off.worker_started());
    assert_eq!(collector.hits().len(), 1);
}

#[actix_web::test]
async fn rom_without_the_middleware_never_fails() {
    let app = test::init_service(App::new().configure(routes)).await;
    let res = test::call_service(&app, req("POST", "/signup").to_request()).await;
    assert!(res.status().is_success());
}
