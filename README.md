# runomine (Rust)

Cookieless analytics that counts money, not just visits, sent **from your Rust backend** to
[runomine](https://runomine.com). Pageviews and business events (signups, purchases with revenue) go out in batches,
in the background. Your app is never blocked, and runomine never causes an error or a panic in it.

- Framework-agnostic core: `Client`, `visitor_hash`, `PageView` and `Track`.
- Optional integrations: axum 0.8, any tower/hyper stack, actix-web 4, plus helpers for other frameworks.
- Async on tokio by default, or a `blocking` client (std thread + ureq) with no async runtime.
- Rust 1.85+ (edition 2024).

## Install

From crates.io:

```toml
[dependencies]
runomine = { version = "0.1", features = ["axum"] }   # or "actix-web", "tower", "blocking"
```

### Before it's on crates.io

**From git** (this repo):

```toml
[dependencies]
runomine = { git = "https://github.com/runomine/runomine-rust", features = ["axum"] }
```

Or `cargo add runomine --git https://github.com/runomine/runomine-rust --features axum`. Pin a commit with
`rev = "…"` (or a `branch`/`tag`) for reproducible builds. `Cargo.lock` records the commit either way.

**From a path** (a clone of this repo):

```toml
[dependencies]
runomine = { path = "/path/to/runomine-rust", features = ["axum"] }
```

Then set the token of your site (runomine → Sites → your site):

```sh
RUNOMINE_TOKEN=rm_live_…
```

## Features

| Feature | Default | What it adds | Extra dependencies |
|---|---|---|---|
| `tokio` | yes | Async `Client`: a tokio task sending with reqwest | tokio (`rt`, `sync`, `time`), reqwest 0.13 |
| `rustls` | yes | TLS for the enabled sender (reqwest and/or ureq) | rustls |
| `native-tls` | | The platform TLS instead (use `default-features = false, features = ["tokio", "native-tls"]`) | native-tls |
| `blocking` | | `runomine::blocking::Client`: a std thread sending with ureq, no async runtime | ureq 3 |
| `tower` | | `RunomineLayer` for any `http` 1.x tower stack, and `Rom` | http, tower-layer, tower-service |
| `axum` | | `tower` plus axum's `ConnectInfo` for the IP, and `Rom` as an extractor | axum 0.8 (`tokio` feature only) |
| `actix-web` | | `actix::Runomine` middleware and `Rom` as an extractor | actix-web 4 (no default features) |

Without any feature you still get `visitor_hash`, the types and the [`web`](src/web.rs) helpers. A blocking-only
build is `default-features = false, features = ["blocking", "rustls"]`, with no tokio and no reqwest.

## axum

```rust
use std::net::SocketAddr;
use runomine::{Client, RunomineLayer};

let rom = Client::from_env();                                        // 1. client from RUNOMINE_*
let app = app.layer(RunomineLayer::new(rom.clone()));                // 2. pageviews + `Rom`
axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
    .with_graceful_shutdown(shutdown_signal())
    .await?;
rom.shutdown().await;                                                // 3. send what's queued
```

That's it: every `GET` answered `200` with `text/html` is recorded when the response is ready. The event goes on an
in-memory queue, and nothing waits for the network. Assets (any path with a file extension other than `.html`), prefetches
(`Purpose`/`Sec-Purpose: prefetch`) and skipped paths are left out. Add the layer on the outer `Router`, after your
routes, so it sees every request with its full path.

```rust
RunomineLayer::new(rom.clone())
    .skip(["/admin", "/api*", "/healthz"])         // "/admin" = itself and below, "/api*" = prefix
    .client_ip(runomine::ClientIp::RightmostForwardedFor) // behind a proxy, see below
    .user(|parts| parts.extensions.get::<CurrentUser>().map(|u| u.id.to_string()))
```

Handlers record events with the `Rom` extractor, which attaches the request's visitor hash, URL, user agent and
country:

```rust
use runomine::{Rom, Track};

async fn signup(rom: Rom, Form(form): Form<SignupForm>) -> impl IntoResponse {
    let user = create_user(&form).await?;
    rom.identify(user.id); // this request's pageview and later rom.track calls carry it
    rom.track("signup", Track::new().prop("plan", "free"));
    // …
}
```

A full example is in [`examples/axum.rs`](examples/axum.rs).

## actix-web

```rust
use runomine::{Client, Rom, Track, actix::Runomine};

let rom = Client::from_env();
let app_rom = rom.clone();
HttpServer::new(move || App::new().wrap(Runomine::new(app_rom.clone())).service(signup))
    .bind(("0.0.0.0", 8080))?
    .run()
    .await?;
rom.shutdown().await; // after the server stopped (SIGINT/SIGTERM)

#[post("/signup")]
async fn signup(rom: Rom) -> HttpResponse {
    rom.track("signup", Track::new().prop("plan", "free"));
    HttpResponse::Ok().finish()
}
```

`Runomine` has the same `skip`, `client_ip`, `trust_forwarded_host` and `user` options (`user` gets the
`ServiceRequest`). See [`examples/actix.rs`](examples/actix.rs).

## Any tower stack (hyper, tonic-web, …)

`RunomineLayer` works with any `Service<http::Request<B>, Response = http::Response<B2>>`:

```rust
let service = tower::ServiceBuilder::new()
    .layer(runomine::RunomineLayer::new(rom.clone()))
    .service(my_service);
```

For the IP, insert the peer's `SocketAddr` into the request extensions in your accept loop, or use a trusted header
with `client_ip`. Handlers get the request's `Rom` with `runomine::tower::rom_for(&parts)`.

## Plain tokio: workers, webhooks, jobs

```rust
use runomine::{Client, Track};

let rom = Client::from_env();

// A payment webhook: no visitor request behind it, so the event has no `visitor`,
// and runomine attributes it through `user`.
rom.track("purchase", Track::new()
    .revenue(4.99, "EUR")            // major units: 4.99, not 499
    .user(order.user_id)
    .prop("plan", "rent"));

rom.shutdown().await;                // before a short-lived process exits
```

To tie a webhook's purchase to the visit that started it, capture the visitor where the visitor is, keep it with
the order, and pass it on:

```rust
// at checkout, in the visitor's request (axum/actix-web)
order.runomine_visitor = rom.visitor();                       // Option<String>
// or anywhere, from what you know about the request
order.runomine_visitor = client.visitor(ip, user_agent, host);

// later, in the webhook
rom.track("purchase", Track::new().revenue(order.total, "EUR").user(order.user_id)
    .visitor(order.runomine_visitor.clone().unwrap_or_default()));
```

The library never invents a placeholder visitor. See [`examples/plain_tokio.rs`](examples/plain_tokio.rs).

## Blocking (no async runtime)

```rust
let rom = runomine::blocking::Client::from_env();
rom.track("export_finished", runomine::Track::new().user("42").prop("rows", 1200));
rom.shutdown(); // blocks until sent or given up on
```

Same API, with blocking `flush()` and `shutdown()`. Don't call them from an async runtime's worker thread. See
[`examples/blocking.rs`](examples/blocking.rs).

## Other frameworks

Build pageviews yourself with the `web` helpers, which apply the same rules as the integrations:

```rust
use runomine::{PageView, web};

if web::is_pageview_request(method, path, &headers, &["/admin"]) {
    // … run the handler …
    if web::is_pageview_response(status, content_type) {
        let info = web::request_info(web::RequestParts { headers: &headers, scheme, authority,
            path_and_query, peer }, &runomine::ClientIp::Peer, true);
        rom.pageview(PageView::from(info));
    }
}
```

Implement `web::Headers` for your framework's header map (it's two methods), or pass `&[(&str, &str)]`.

## Revenue from your payment provider

runomine takes revenue from your payment provider, not from prices in your code: connect Stripe, Paddle, Lemon
Squeezy, Shopify or PayPal in the site's settings → **Integrations**, and runomine reads every sale, renewal and
refund from it. To attribute a sale to the visit and the person behind it, pass two identifiers when you create the
checkout; runomine reads them back from the provider.

```rust
async fn checkout(rom: Rom, user: CurrentUser) -> impl IntoResponse {
    rom.identify(user.id);
    let metadata = rom.checkout_metadata(); // BTreeMap: runomine_visitor, runomine_user
    // …
}

// Without the integrations: client.checkout_metadata(&request_info), user from request_info.user
```

`runomine_visitor` is the request's visitor hash and `runomine_user` the user set with `rom.identify` (else the
layer's `.user(…)`), as a string. Only keys with a value are returned: without a token or the request's IP there is
no visitor (never a made-up one).

Once a provider is connected, **don't also send `revenue`** with `track("purchase", …)`: it would be counted twice.
Send the event without revenue, or not at all. (The webhook example above is for payments that don't go through a
connected provider.)

Put the map where each provider keeps your data:

- **Stripe**: Checkout Session `metadata`, plus `client_reference_id` = your user id (runomine uses it as the user
  when `runomine_user` is missing); for subscriptions also `subscription_data.metadata`, so renewals carry the user.
  Without Checkout, the PaymentIntent's `metadata`.
- **Paddle Billing**: `customData` in `Paddle.Checkout.open({ … })` (render the values into the page), or
  `custom_data` when you create the transaction through the API.
- **Lemon Squeezy**: `checkout_data.custom` when you create the checkout through the API, or
  `checkout[custom][runomine_visitor]=…` URL parameters on a checkout link.
- **Shopify** (headless, Storefront API cart): cart `attributes`, `[{ "key": "runomine_visitor", "value": "…" }, …]`,
  which become the order's note attributes.
- **PayPal Orders v2**: `purchase_units[0].custom_id` = the map as a query string (127 characters at most), e.g.
  `url::form_urlencoded::Serializer::new(String::new()).extend_pairs(&metadata).finish()`.

```rust
// Stripe, with a plain form-encoded request (async-stripe and friends take the same map as `metadata`)
let mut form: Vec<(String, String)> = vec![
    ("mode".into(), "payment".into()),
    ("line_items[0][price]".into(), "price_123".into()),
    ("line_items[0][quantity]".into(), "1".into()),
    ("success_url".into(), "https://example.com/done".into()),
    ("client_reference_id".into(), user.id.to_string()),
];
form.extend(metadata.iter().map(|(k, v)| (format!("metadata[{k}]"), v.clone())));
http.post("https://api.stripe.com/v1/checkout/sessions").bearer_auth(&stripe_key).form(&form).send().await?;
```

## Behind a proxy: the visitor's IP

The IP only feeds the visitor hash. By default it's the TCP peer: axum's `ConnectInfo<SocketAddr>` (serve with
`into_make_service_with_connect_info::<SocketAddr>()`) or actix-web's `peer_addr()`. Behind a proxy or load
balancer, the peer is the proxy, so every visitor would share one hash. Use `client_ip` to read a header instead:

| Setup | `client_ip` |
|---|---|
| Nothing in front | `ClientIp::Peer` (default) |
| Cloudflare | `ClientIp::header("cf-connecting-ip")` |
| nginx with `proxy_set_header X-Real-IP $remote_addr` | `ClientIp::header("x-real-ip")` |
| Fly.io | `ClientIp::header("fly-client-ip")` |
| One proxy that appends to `X-Forwarded-For` (Railway, Heroku, Render, AWS ALB) | `ClientIp::RightmostForwardedFor` |

Only trust a header that your proxy sets or overwrites on every request. Otherwise a client can send any value. A
forged IP only changes that request's visitor hash (it can inflate your unique-visitor count), and nothing else.
The page URL's host and scheme come from `X-Forwarded-Host`/`X-Forwarded-Proto` when present, then `Host`. Turn
that off with `trust_forwarded_host(false)` if nothing in front of your app sets them.

## Privacy

- **The IP never leaves your server.** The library hashes it on your machine, with the user agent, the host and a
  salt derived from your token and today's date (UTC). Only the first 16 hex characters of that hash are sent.
- No cookies, no storage, no fingerprinting. The hash changes every day and differs per site, so a person can't be
  followed across days or sites.
- The user agent is sent so runomine can drop bots and group devices and browsers. runomine discards it after that.
- `user` is only sent when you set it.
- The library does no classification. Sources, bots, devices and countries-from-IP are all worked out by runomine.

The hash is exported and reproduces the contract's test vector:

```rust
assert_eq!(
    runomine::visitor_hash("rm_live_test_0123456789abcdef", "203.0.113.9",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Version/18.0 Safari/605.1.15",
        "example.com", "2026-10-13"),
    "5f9e1bebe31e8bf5",
);
```

## Configuration

`Client::from_env()` reads the environment. `Client::new(Config { … })` sets things in code:

```rust
let rom = runomine::Client::new(runomine::Config { debug: true, ..runomine::Config::new(token) });
```

| `Config` field | Environment | Default | |
|---|---|---|---|
| `token` | `RUNOMINE_TOKEN` | none | Server token, `rm_live_…`. Without it the client does nothing. |
| `url` | `RUNOMINE_URL` | `https://runomine.com` | Where events go (`{url}/api/v1/events`). |
| `enabled` | `RUNOMINE_ENABLED` | `true` | `false` turns the client into a no-op (tests, development). |
| `debug` | `RUNOMINE_DEBUG` | `false` | Log dropped batches and rejected events to stderr. |
| `flush_interval` | | 2 s | Longest wait before a queued event is sent. |
| `batch_size` | | 500 | Events per request (the API's maximum). |
| `max_queue` | | 10 000 | Events kept in memory. When full, **new** events are dropped. |
| `retry_delays` | | `[1 s, 3 s]` | Backoff before each retry on 5xx, 429 or a network error. Then the batch is dropped. |
| `timeout` | | 5 s | Per-request timeout. |

`runomine::client()` is a process-wide client from the environment, used by `RunomineLayer::default()`,
`Runomine::default()` and a `Rom` extracted without the middleware. Set it with `runomine::init(client)`.

## Delivery

- Recording (`pageview`, `track`, `Rom::track`) builds the event, hashes the IP and puts the event on a bounded
  queue. It never blocks, never awaits, never returns an error and never panics.
- One background task per client sends the queue. It runs on a small tokio runtime of its own, on a thread named
  `runomine`, so it doesn't depend on your runtimes: it keeps working when actix-web's worker runtimes stop, and
  it also sends events recorded outside any runtime. It starts with the first event. A disabled client starts
  nothing.
- A batch goes out 2 seconds after its first event, as soon as 500 are waiting, on `flush()`, and on `shutdown()`.
- `5xx`, `429` and network errors are retried twice with backoff (honouring `Retry-After`, capped at 10 s), then
  the batch is dropped quietly. `401` (a bad token) and other `4xx` are not retried. Every event has a UUID v7
  `id`, so a retried batch is stored once.
- When the queue is full (runomine unreachable for a long time), new events are dropped. The queue is bounded, so
  memory stays bounded.
- **Call `shutdown().await` before the process exits**, or up to 2 seconds of events are lost. With retries, a
  batch can take `timeout × 3 + 4 s` in the worst case, so wrap the call in `tokio::time::timeout` if your platform
  gives you less time to stop.

## API

| | |
|---|---|
| `Client::new(Config)` / `from_env()` / `disabled()` | A client. Cheap to clone. |
| `client.pageview(PageView { url, ip, user_agent, referrer, country, host, user, visitor, ts })` | Record a pageview. |
| `client.track(name, Track)` | Record an event. `Track::new().prop(k, v).props(…).revenue(amount, currency).user(id).visitor(v).url(u).ts(t).request(RequestInfo)` |
| `client.visitor(ip, user_agent, host)` | Today's visitor hash for this site. |
| `client.checkout_metadata(&RequestInfo)` | `runomine_visitor` / `runomine_user` for your payment provider's checkout. |
| `client.flush().await` / `client.shutdown().await` | Send now / send and stop. Never fail. |
| `runomine::client()` / `runomine::init(client)` | The process-wide client. |
| `RunomineLayer::new(client)` | tower/axum layer: `.skip(…)`, `.client_ip(…)`, `.trust_forwarded_host(…)`, `.user(…)`. |
| `actix::Runomine::new(client)` | actix-web middleware, same options. |
| `Rom` | Extractor (axum, actix-web): `track`, `visitor`, `checkout_metadata`, `identify`, `request`, `client`. |
| `tower::rom_for(&parts)` / `axum::visitor_for(&parts)` / `actix::visitor_for(&req)` | The `Rom` or visitor of a request, outside an extractor. |
| `visitor_hash(token, ip, user_agent, host, day)` / `today()` | The raw hash. |
| `web::*` | Pageview rules and request parsing for other frameworks. |
| `blocking::Client` | The same client without async. |

## Development

```sh
cargo test --all-features             # unit, integration (fake collector) and doc tests
./scripts/check-features.sh           # clippy -D warnings on every feature combination
./scripts/smoke-examples.sh           # runs every example against examples/fake_collector.rs and checks the payloads
```

Try an example against the fake collector:

```sh
cargo run --example fake_collector &
RUNOMINE_URL=http://127.0.0.1:8788 RUNOMINE_TOKEN=rm_live_test cargo run --example axum --features axum
curl -A 'Mozilla/5.0' http://127.0.0.1:3000/
```

## License

MIT. See [LICENSE](LICENSE).
