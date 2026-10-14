# Changelog

## Unreleased

Not on crates.io yet (the crate's trusted publisher isn't configured); install from git to use it. It will be 0.2.0.

- Page views take props, with the same rules as events (flat, up to 20 keys, string or number values):
  `PageView::prop` / `PageView::props` and the new `props` field, and `Rom::page_prop` for the pageview the axum,
  tower and actix-web middleware record. runomine stores them as the page view's dims. Code that builds a
  `PageView` with a struct literal needs `..Default::default()` (or `props: Default::default()`).
- Not yet here: samples (`POST /api/v1/samples`), which the PHP, Node and Python libraries send since their 0.2.0.

## 0.1.0

- First release: async `Client` on tokio and `blocking::Client`, pageviews and events with props and revenue,
  batching, retries, the contract's visitor hash, checkout metadata, and the tower/axum and actix-web integrations
  with the `Rom` extractor.
