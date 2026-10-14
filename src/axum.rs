//! axum 0.8: [`RunomineLayer`](crate::RunomineLayer) for pageviews (it reads `ConnectInfo<SocketAddr>` when you
//! serve with `into_make_service_with_connect_info::<SocketAddr>()`), and [`Rom`] as an extractor.
//!
//! ```no_run
//! use std::net::SocketAddr;
//! use axum::{Router, routing::post};
//! use runomine::{Rom, RunomineLayer, Track};
//!
//! async fn signup(rom: Rom) -> &'static str {
//!     // … create the account …
//!     rom.identify("42");
//!     rom.track("signup", Track::new().prop("plan", "free"));
//!     "ok"
//! }
//!
//! # async fn run() {
//! let rom = runomine::Client::from_env();
//! let app = Router::new()
//!     .route("/signup", post(signup))
//!     .layer(RunomineLayer::new(rom.clone()));
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
//! axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
//!     .with_graceful_shutdown(async { tokio::signal::ctrl_c().await.ok(); })
//!     .await
//!     .unwrap();
//! rom.shutdown().await;
//! # }
//! ```

use std::convert::Infallible;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::rom::Rom;
pub use crate::tower::{rom_for, visitor_for};

impl<S: Send + Sync> FromRequestParts<S> for Rom {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(rom_for(parts))
    }
}
