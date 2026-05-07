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

# 2. trades by token → orderHash + txHash from a trade that actually exists.
#    The orders-by-token endpoint can return orders with zero trades, so we
#    can't derive tx_hash from there reliably. Trades-by-token gives both.
order_hash=""; tx_hash=""
if [ -n "$token_in" ]; then
  trades_json="$(curl -fsS "${auth[@]}" "$host/v1/trades/token/$token_in?page=1&page_size=1")" || {
    log "WARN: /v1/trades/token/$token_in failed"; trades_json='[]'; }
  # Response can be either a bare array or wrapped {trades:[…]}. Handle both.
  order_hash="$(echo "$trades_json" | jq -r '
    (if type=="array" then .[0] else (.trades[0]? // .data[0]? // null) end)
    | (.orderHash // .order_hash // empty)')"
  tx_hash="$(echo "$trades_json" | jq -r '
    (if type=="array" then .[0] else (.trades[0]? // .data[0]? // null) end)
    | (.txHash // .tx_hash // .transactionHash // empty)')"
  [ -n "$order_hash" ] && echo "order_hash=$order_hash" >> "$out"
  [ -n "$tx_hash"    ] && echo "tx_hash=$tx_hash"       >> "$out"
fi
log "  order_hash=${order_hash:-<none>}"
log "  tx_hash=${tx_hash:-<none>}"

# 3. orders by token → owner (one of the orders that owns the token).
owner=""
if [ -n "$token_in" ]; then
  orders_json="$(curl -fsS "${auth[@]}" "$host/v1/orders/token/$token_in?page=1&page_size=1")" || {
    log "WARN: /v1/orders/token/$token_in failed"; orders_json='{"orders":[]}'; }
  owner="$(echo "$orders_json" | jq -r '.orders[0].owner // empty')"
  [ -n "$owner" ] && echo "owner=$owner" >> "$out"
fi
log "  owner=${owner:-<none>}"

log "Wrote $out"
cat "$out" >&2
