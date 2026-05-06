#!/usr/bin/env bash
# Discover fixture params from a live host. Writes bench/results/.discovered.env.
# Usage: bench/discover.sh [prod|preview]   (default: prod)

set -euo pipefail
cd "$(dirname "$0")/.."

# shellcheck source=bench/lib.sh
. bench/lib.sh
require_cmd curl
require_cmd jq
load_env

target="${1:-prod}"
case "$target" in
  prod)    host="${BENCH_PROD_HOST:-}"; user="${BENCH_PROD_USER:-}"; pass="${BENCH_PROD_PASS:-}" ;;
  preview) host="${BENCH_PREVIEW_HOST:-}"; user="${BENCH_PREVIEW_USER:-}"; pass="${BENCH_PREVIEW_PASS:-}" ;;
  *) echo "usage: $0 [prod|preview]" >&2; exit 2 ;;
esac

if [ -z "${user:-}" ] || [ -z "${pass:-}" ]; then
  echo "ERROR: missing $target credentials in bench/.env" >&2
  exit 1
fi

b64="$(basic_b64 "$user" "$pass")"
auth=( -H "Authorization: Basic $b64" )

out="bench/results/.discovered.env"
: > "$out"

log() { echo "[discover] $*" >&2; }

log "Discovering fixtures from $target ($host)…"

# 1. tokens → token_in, token_out
tokens_json="$(curl -fsS "${auth[@]}" "$host/v1/tokens")" || {
  log "WARN: /v1/tokens failed; token_in/token_out unavailable"; tokens_json="[]"; }

token_in="$(echo "$tokens_json"  | jq -r 'first(.[]?|.address) // empty')"
token_out="$(echo "$tokens_json" | jq -r '[.[]?|.address]|.[1] // empty')"
[ -n "$token_in"  ] && echo "token_in=$token_in"   >> "$out"
[ -n "$token_out" ] && echo "token_out=$token_out" >> "$out"
log "  token_in=${token_in:-<none>}"
log "  token_out=${token_out:-<none>}"

# 2. orders → order_hash, owner
order_hash=""; owner=""
if [ -n "$token_in" ]; then
  orders_json="$(curl -fsS "${auth[@]}" "$host/v1/orders/token/$token_in?page=1&page_size=1")" || {
    log "WARN: /v1/orders/token/$token_in failed"; orders_json='{"orders":[]}'; }
  order_hash="$(echo "$orders_json" | jq -r '.orders[0].order_hash // .orders[0].hash // empty')"
  owner="$(echo "$orders_json"      | jq -r '.orders[0].owner // empty')"
  [ -n "$order_hash" ] && echo "order_hash=$order_hash" >> "$out"
  [ -n "$owner"      ] && echo "owner=$owner"           >> "$out"
fi
log "  order_hash=${order_hash:-<none>}"
log "  owner=${owner:-<none>}"

# 3. order detail → tx_hash
tx_hash=""
if [ -n "$order_hash" ]; then
  order_json="$(curl -fsS "${auth[@]}" "$host/v1/order/$order_hash")" || {
    log "WARN: /v1/order/$order_hash failed"; order_json='{}'; }
  tx_hash="$(echo "$order_json" | jq -r '
    .trades[0].tx_hash //
    .trades[0].transaction_hash //
    .order.transaction //
    .transaction //
    empty')"
  [ -n "$tx_hash" ] && echo "tx_hash=$tx_hash" >> "$out"
fi
log "  tx_hash=${tx_hash:-<none>}"

log "Wrote $out"
cat "$out" >&2
