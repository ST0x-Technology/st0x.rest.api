#!/usr/bin/env bash
# Discover fixture params from a live host.
# Writes bench/results/.discovered-<target>.env so each host has its own
# fixtures (avoids cross-host data mismatches that produce 404s).
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

out="bench/results/.discovered-${target}.env"
: > "$out"

log() { echo "[discover] $*" >&2; }

log "Discovering fixtures from $target ($host)…"

# 1. tokens → list of candidate addresses
tokens_json="$(curl -fsS "${auth[@]}" "$host/v1/tokens")" || {
  log "WARN: /v1/tokens failed; token_in/token_out unavailable"; tokens_json="[]"; }

# Collect every token address; we'll walk them looking for one with trades.
# `mapfile` is bash 4+; macOS ships bash 3.2, so use the portable idiom.
token_addrs=()
while IFS= read -r addr; do
  [ -n "$addr" ] && token_addrs+=("$addr")
done < <(echo "$tokens_json" | jq -r '.[]?|.address // empty')

if [ "${#token_addrs[@]}" -eq 0 ]; then
  log "ERROR: no token addresses returned"
  log "Wrote $out"
  exit 0
fi

# 2. Walk tokens until we find one whose /v1/trades/token/{addr} returns a trade.
#    That gives us a token guaranteed to have orderHash + txHash.
token_in=""; token_out=""; order_hash=""; tx_hash=""
extract_first() { jq -r '(if type=="array" then .[0] else (.trades[0]? // .data[0]? // null) end) | '"$1"' // empty'; }

for addr in "${token_addrs[@]}"; do
  trades_json="$(curl -fsS "${auth[@]}" "$host/v1/trades/token/$addr?page=1&page_size=1")" || continue
  oh="$(echo "$trades_json" | extract_first '(.orderHash // .order_hash)')"
  th="$(echo "$trades_json" | extract_first '(.txHash // .tx_hash // .transactionHash)')"
  if [ -n "$oh" ] && [ -n "$th" ]; then
    token_in="$addr"
    order_hash="$oh"
    tx_hash="$th"
    break
  fi
done

if [ -z "$token_in" ]; then
  # No token had trades — fall back to the first token so partial fixtures still work.
  token_in="${token_addrs[0]}"
  log "WARN: no token has trades on this host; order_hash/tx_hash unavailable"
fi

# token_out: any other token in the list (different from token_in).
for addr in "${token_addrs[@]}"; do
  if [ "$addr" != "$token_in" ]; then token_out="$addr"; break; fi
done

[ -n "$token_in"   ] && echo "token_in=$token_in"     >> "$out"
[ -n "$token_out"  ] && echo "token_out=$token_out"   >> "$out"
[ -n "$order_hash" ] && echo "order_hash=$order_hash" >> "$out"
[ -n "$tx_hash"    ] && echo "tx_hash=$tx_hash"       >> "$out"
log "  token_in=${token_in:-<none>}"
log "  token_out=${token_out:-<none>}"
log "  order_hash=${order_hash:-<none>}"
log "  tx_hash=${tx_hash:-<none>}"

# 3. orders by token → owner.
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
