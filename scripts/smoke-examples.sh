#!/usr/bin/env bash
# Runs every example against examples/fake_collector.rs and checks what reached the collector.
# The servers are stopped with SIGTERM well before the 2-second timer, so their events only arrive if the
# graceful-shutdown flush works. Needs curl and python3.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --quiet --all-features --examples
BIN=target/debug/examples
OUT=$(mktemp -d)
PIDS=()
cleanup() { for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT

export RUNOMINE_TOKEN=rm_live_test_0123456789abcdef
UA='Mozilla/5.0 (smoke test)'

wait_port() {
  for _ in $(seq 100); do
    # HEAD: not a pageview
    if curl -s -o /dev/null -I "http://127.0.0.1:$1/"; then return 0; fi
    sleep 0.1
  done
  echo "nothing listening on $1" >&2
  return 1
}

# start_collector NAME PORT: a fresh collector writing to $OUT/NAME.jsonl
start_collector() {
  PORT=$2 "$BIN/fake_collector" >"$OUT/$1.jsonl" 2>/dev/null &
  PIDS+=($!)
  wait_port "$2"
  export RUNOMINE_URL="http://127.0.0.1:$2"
}

stop() { kill -TERM "$1"; wait "$1" || true; }

echo "== axum"
start_collector axum 18701
PORT=18711 "$BIN/axum" 2>/dev/null &
APP=$!; PIDS+=($APP); wait_port 18711
curl -s -A "$UA" -e 'https://www.google.com/' 'http://127.0.0.1:18711/?utm_source=smoke' >/dev/null
curl -s -A "$UA" http://127.0.0.1:18711/api/health >/dev/null
curl -s -A "$UA" -H 'Sec-Purpose: prefetch' http://127.0.0.1:18711/ >/dev/null
curl -s -A "$UA" -X POST http://127.0.0.1:18711/signup >/dev/null
CHECKOUT_VISITOR=$(curl -s -A "$UA" -X POST http://127.0.0.1:18711/checkout | python3 -c 'import json,sys; print(json.load(sys.stdin)["runomine_visitor"])')
stop $APP

echo "== actix-web"
start_collector actix 18702
PORT=18712 "$BIN/actix" 2>/dev/null &
APP=$!; PIDS+=($APP); wait_port 18712
curl -s -A "$UA" 'http://127.0.0.1:18712/?utm_source=smoke' >/dev/null
curl -s -A "$UA" -X POST http://127.0.0.1:18712/signup >/dev/null
curl -s -A "$UA" -X POST http://127.0.0.1:18712/purchase >/dev/null
stop $APP

echo "== plain tokio"
start_collector plain 18703
"$BIN/plain_tokio"

echo "== blocking"
start_collector blocking 18704
"$BIN/blocking"

sleep 0.2
python3 -I - "$OUT" "$RUNOMINE_TOKEN" "$UA" "$CHECKOUT_VISITOR" <<'PY'
import hashlib, hmac, json, sys, datetime, pathlib

out, token, ua, checkout_visitor = sys.argv[1:5]
day = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")

def visitor(ip, agent, host):
    salt = hmac.new(token.encode(), f"runomine:{day}".encode(), hashlib.sha256).hexdigest()
    return hashlib.sha256(f"{salt}|{ip}|{agent}|{host}".encode()).hexdigest()[:16]

def events(name):
    lines = [json.loads(l) for l in pathlib.Path(out, f"{name}.jsonl").read_text().splitlines() if l.strip()]
    for batch in lines:
        assert batch["authorization"] == f"Bearer {token}", batch["authorization"]
    found = [e for batch in lines for e in batch["events"]]
    for e in found:
        assert len(e["id"]) == 36 and e["id"][14] == "7", e["id"]
        assert e["ts"].endswith("Z"), e["ts"]
        assert "ip" not in e and "127.0.0.1" not in json.dumps({k: v for k, v in e.items() if k != "url"})
    return found

def check(name, got, expected):
    assert got == expected, f"{name}: {got!r} != {expected!r}"

local = visitor("127.0.0.1", ua, "127.0.0.1")

ev = events("axum")
check("axum kinds", [(e["type"], e.get("name")) for e in ev],
      [("pageview", None), ("event", "signup")])
pv, signup = ev
check("axum url", pv["url"], "http://127.0.0.1:18711/?utm_source=smoke")
check("axum referrer", pv["referrer"], "https://www.google.com/")
check("axum pageview visitor", pv["visitor"], local)
check("axum signup visitor", signup["visitor"], local)
check("axum signup user", signup["user"], "42")
check("axum checkout visitor", checkout_visitor, local)
print("axum: pageview (prefetch and /api skipped), signup with visitor and user, flushed on SIGTERM")

ev = events("actix")
check("actix kinds", [(e["type"], e.get("name")) for e in ev],
      [("pageview", None), ("event", "signup"), ("event", "purchase")])
check("actix visitor", {e["visitor"] for e in ev}, {local})
check("actix revenue", ev[2]["revenue"], {"amount": 4.99, "currency": "EUR"})
print("actix-web: pageview, signup, purchase with revenue, flushed after the server stopped")

ev = events("plain")
check("plain names", [e["name"] for e in ev], ["purchase", "renewal"])
check("plain stored visitor", ev[0]["visitor"], visitor("203.0.113.9", "Mozilla/5.0", "example.com"))
assert "visitor" not in ev[1], ev[1]
check("plain user", ev[1]["user"], "42")
print("plain tokio: webhook purchase with the stored visitor, job renewal without a visitor")

ev = events("blocking")
check("blocking kinds", [(e["type"], e.get("name")) for e in ev],
      [("pageview", None), ("event", "export_finished")])
check("blocking visitor", ev[0]["visitor"], visitor("203.0.113.9", "Mozilla/5.0", "example.com"))
assert "visitor" not in ev[1], ev[1]
check("blocking props", ev[1]["props"], {"rows": 1200})
print("blocking: pageview and event from sync code, sent on shutdown()")
print("all examples OK")
PY
