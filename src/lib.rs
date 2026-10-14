//! Cookieless analytics that counts money, not just visits: pageviews and business events (signups, purchases
//! with revenue) sent **from your Rust backend** to [runomine](https://runomine.com), in batches, in the
//! background. Your app is never blocked and never gets an error or a panic because of runomine.
//!
//! - `Client` (feature `tokio`, default): `pageview`, `track`, `flush().await`, `shutdown().await`.
//! - `blocking::Client` (feature `blocking`): the same without an async runtime.
//! - [`visitor_hash`]: the contract's visitor hash. The visitor's IP is hashed on your server and never sent.
//! - Integrations: `RunomineLayer` for tower/axum (features `tower`, `axum`), `actix::Runomine` for actix-web
//!   (feature `actix-web`), and [`web`] helpers for any other framework. `Rom` is the per-request handle.
//!
//! ```no_run
//! # #[cfg(feature = "tokio")]
//! # async fn demo() {
//! use runomine::{Client, Track};
//!
//! let rom = Client::from_env(); // RUNOMINE_TOKEN, RUNOMINE_URL, RUNOMINE_ENABLED
//! // A purchase from a payment webhook: no request, so no visitor; runomine attributes it through `user`.
//! rom.track("purchase", Track::new().revenue(4.99, "EUR").user(42).prop("plan", "rent"));
//! rom.shutdown().await; // before the process exits
//! # }
//! ```
//!
//! The library does no classification: sources, bots, devices and countries-from-IP are worked out by runomine.

mod config;
mod core;
mod event;
mod hash;
mod util;
pub mod web;

#[cfg(feature = "tokio")]
mod client;
#[cfg(any(feature = "tower", feature = "actix-web"))]
mod rom;

#[cfg(feature = "blocking")]
pub mod blocking;

#[cfg(feature = "tower")]
pub mod tower;

#[cfg(feature = "axum")]
pub mod axum;

#[cfg(feature = "actix-web")]
pub mod actix;

pub use config::{Config, DEFAULT_URL};
pub use event::{PageView, Prop, RequestInfo, Revenue, Track};
pub use hash::{today, visitor_hash};

#[cfg(feature = "tokio")]
pub use client::{Client, client, init};
#[cfg(any(feature = "tower", feature = "actix-web"))]
pub use rom::Rom;
#[cfg(feature = "tower")]
pub use tower::RunomineLayer;
pub use web::ClientIp;
