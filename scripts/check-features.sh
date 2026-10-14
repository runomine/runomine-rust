#!/usr/bin/env bash
# Checks (and lints) the library with each feature combination a user may pick.
set -euo pipefail
cd "$(dirname "$0")/.."

combos=(
  ""
  "tokio,rustls"
  "tokio,native-tls"
  "blocking,rustls"
  "blocking,native-tls"
  "blocking"
  "tower,rustls"
  "axum,rustls"
  "actix-web,rustls"
  "axum,actix-web,blocking,rustls,native-tls"
)

for features in "${combos[@]}"; do
  echo "== --no-default-features --features '${features}'"
  cargo clippy --quiet --lib --no-default-features --features "${features}" -- -D warnings
done
echo "== default"
cargo clippy --quiet --lib -- -D warnings
echo "== all features, all targets"
cargo clippy --quiet --all-features --all-targets -- -D warnings
echo "all feature combinations are clean"
