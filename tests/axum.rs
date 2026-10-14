//! The tower layer and the `Rom` extractor in a real axum server (with `ConnectInfo`), against a fake collector.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
};
use common::{Collector, TOKEN, UA, config};
use runomine::{Client, ClientIp, Rom, RunomineLayer, Track, visitor_hash};
use serde_json::{Value, json};

async fn page() -> Html<&'static str> {
    Html("<h1>hi</h1>")
}

async fn signup(rom: Rom) -> Html<&'static str> {
    rom.identify("u_42");
    rom.track("signup", Track::new().prop("plan", "free"));
    Html("<p>welcome</p>")
}

async fn checkout(rom: Rom) -> String {
    rom.visitor().unwrap_or_default()
}

async fn checkout_metadata(rom: Rom) -> Json<Value> {
    let anonymous = rom.checkout_metadata();
    rom.identify(42);
    Json(json!({"anonymous": anonymous, "identified": rom.checkout_metadata()}))
}

fn app(layer: RunomineLayer) -> Router {
    Router::new()
        .route("/", get(page).post(page))
        .route("/pricing", get(page))
        .route("/page.html", get(page))
        .route("/logo.png", get(page))
        .route("/admin/stats", get(page))
        .route("/json", get(|| async { Json(json!({"ok": true})) }))
        .route(
            "/missing",
            get(|| async { (StatusCode::NOT_FOUND, Html("<h1>404</h1>")) }),
        )
        .route(
            "/redirect",
            get(|| async { (StatusCode::SEE_OTHER, [("location", "/")]).into_response() }),
        )
        .route("/welcome", get(signup))
        .route("/signup", post(signup))
        .route("/checkout", get(checkout))
        .route("/checkout/metadata", post(checkout_metadata))
        .layer(layer)
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    base
}

fn http() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("user-agent", UA.parse().unwrap());
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap()
}

fn paths(events: &[Value], kind: &str) -> Vec<String> {
    events
        .iter()
        .filter(|e| e["type"] == kind)
        .map(|e| {
            let url = e["url"].as_str().unwrap_or("");
            url.splitn(4, '/')
                .nth(3)
                .map(|p| format!("/{p}"))
                .unwrap_or_default()
        })
        .collect()
}

#[tokio::test]
async fn rom_checkout_metadata_carries_the_visitor_and_the_user() {
    let collector = Collector::start(&[]).await;
    let base = serve(app(RunomineLayer::new(Client::new(config(&collector.url))))).await;
    let body: Value = serde_json::from_str(
        &http()
            .post(format!("{base}/checkout/metadata"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    let visitor = visitor_hash(TOKEN, "127.0.0.1", UA, "127.0.0.1", &runomine::today());
    assert_eq!(body["anonymous"], json!({"runomine_visitor": visitor}));
    assert_eq!(
        body["identified"],
        json!({"runomine_visitor": visitor, "runomine_user": "42"})
    );

    // A disabled client: the user only, never a made-up visitor.
    let base = serve(app(RunomineLayer::new(Client::disabled()))).await;
    let body: Value = serde_json::from_str(
        &http()
            .post(format!("{base}/checkout/metadata"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["anonymous"], json!({}));
    assert_eq!(body["identified"], json!({"runomine_user": "42"}));
}

#[tokio::test]
async fn records_html_pageviews_only() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    let base = serve(app(RunomineLayer::new(rom.clone()).skip(["/admin"]))).await;
    let http = http();
    let get = |path: &str| http.get(format!("{base}{path}"));
    get("/pricing?utm_source=newsletter")
        .header("referer", "https://www.google.com/")
        .header("cf-ipcountry", "es")
        .send()
        .await
        .unwrap();
    get("/page.html").send().await.unwrap();
    // Not pageviews:
    http.post(format!("{base}/")).send().await.unwrap();
    get("/json").send().await.unwrap();
    get("/missing").send().await.unwrap();
    get("/logo.png").send().await.unwrap();
    get("/admin/stats").send().await.unwrap();
    get("/")
        .header("sec-purpose", "prefetch;prerender")
        .send()
        .await
        .unwrap();
    get("/").header("purpose", "prefetch").send().await.unwrap();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .get(format!("{base}/redirect"))
        .send()
        .await
        .unwrap();
    rom.flush().await;

    let events = collector.events();
    assert_eq!(
        paths(&events, "pageview"),
        ["/pricing?utm_source=newsletter", "/page.html"]
    );
    let pv = &events[0];
    let host = base.trim_start_matches("http://");
    assert_eq!(
        pv["url"],
        format!("http://{host}/pricing?utm_source=newsletter")
    );
    assert_eq!(pv["referrer"], "https://www.google.com/");
    assert_eq!(pv["country"], "ES");
    assert_eq!(pv["user_agent"], UA);
    assert_eq!(
        pv["visitor"],
        visitor_hash(TOKEN, "127.0.0.1", UA, "127.0.0.1", &runomine::today()).as_str(),
        "IP from ConnectInfo, host without port"
    );
}

#[tokio::test]
async fn rom_attaches_the_request_and_the_identified_user() {
    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    let base = serve(app(RunomineLayer::new(rom.clone()))).await;
    let http = http();
    http.post(format!("{base}/signup")).send().await.unwrap();
    http.get(format!("{base}/welcome")).send().await.unwrap();
    let visitor = http
        .get(format!("{base}/checkout"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    rom.flush().await;

    let expected = visitor_hash(TOKEN, "127.0.0.1", UA, "127.0.0.1", &runomine::today());
    assert_eq!(visitor, expected, "Rom::visitor, to keep with an order");
    let events = collector.events();
    let signup: Vec<&Value> = events.iter().filter(|e| e["name"] == "signup").collect();
    assert_eq!(signup.len(), 2);
    for e in &signup {
        assert_eq!(e["visitor"], expected.as_str());
        assert_eq!(e["user"], "u_42");
        assert_eq!(e["props"], json!({"plan": "free"}));
        assert_eq!(e["user_agent"], UA);
    }
    assert!(signup[0]["url"].as_str().unwrap().ends_with("/signup"));
    let welcome: Vec<&Value> = events.iter().filter(|e| e["type"] == "pageview").collect();
    assert_eq!(
        welcome.len(),
        1,
        "POST /signup is not a pageview, GET /welcome is"
    );
    assert_eq!(
        welcome[0]["user"], "u_42",
        "Rom::identify reaches the pageview"
    );
}

#[tokio::test]
async fn trusted_proxy_headers_and_user_resolver() {
    #[derive(Clone)]
    struct CurrentUser(&'static str);

    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    let layer = RunomineLayer::new(rom.clone())
        .client_ip(ClientIp::RightmostForwardedFor)
        .user(|parts| {
            parts
                .extensions
                .get::<CurrentUser>()
                .map(|u| u.0.to_owned())
        });
    let app = app(layer).layer(axum::Extension(CurrentUser("u_7")));
    let base = serve(app).await;
    http()
        .get(format!("{base}/"))
        .header("x-forwarded-for", "198.51.100.1, 203.0.113.9")
        .header("x-forwarded-host", "shop.example.com")
        .header("x-forwarded-proto", "https")
        .send()
        .await
        .unwrap();
    rom.flush().await;
    let events = collector.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["url"], "https://shop.example.com/");
    assert_eq!(
        events[0]["visitor"],
        visitor_hash(
            TOKEN,
            "203.0.113.9",
            UA,
            "shop.example.com",
            &runomine::today()
        )
        .as_str()
    );
    assert_eq!(events[0]["user"], "u_7");
}

#[tokio::test]
async fn a_disabled_client_leaves_requests_alone() {
    let collector = Collector::start(&[]).await;
    let rom = Client::disabled();
    let base = serve(app(RunomineLayer::new(rom.clone()))).await;
    let res = http().post(format!("{base}/signup")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        http()
            .get(format!("{base}/"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    rom.flush().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(collector.hits().is_empty());
    assert!(!rom.worker_started());
}

#[tokio::test]
async fn rom_without_the_layer_never_fails() {
    // No layer: Rom falls back to the process-wide client (from the environment: off in tests).
    let app = Router::new().route("/signup", post(signup));
    let base = serve(app).await;
    let res = http().post(format!("{base}/signup")).send().await.unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn the_layer_works_with_a_plain_tower_stack() {
    use tower::{Service, ServiceBuilder, ServiceExt};

    let collector = Collector::start(&[]).await;
    let rom = Client::new(config(&collector.url));
    let mut svc = ServiceBuilder::new()
        .layer(RunomineLayer::new(rom.clone()))
        .service_fn(|req: http::Request<String>| async move {
            let has_rom = req.extensions().get::<SocketAddr>().is_some();
            let mut headers = HeaderMap::new();
            headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
            let mut res = http::Response::new(format!("<p>{has_rom}</p>"));
            *res.headers_mut() = headers;
            Ok::<_, std::convert::Infallible>(res)
        });
    let mut req = http::Request::get("/hello")
        .header("host", "example.com")
        .header("user-agent", UA)
        .body(String::new())
        .unwrap();
    req.extensions_mut()
        .insert(SocketAddr::from(([203, 0, 113, 9], 50000)));
    svc.ready().await.unwrap().call(req).await.unwrap();
    rom.flush().await;
    let events = collector.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["url"], "http://example.com/hello");
    assert_eq!(
        events[0]["visitor"],
        visitor_hash(TOKEN, "203.0.113.9", UA, "example.com", &runomine::today()).as_str()
    );
}
