#!/usr/bin/env bash
# smoke.sh — End-to-end correctness + latency smoke tests against a deployed
# st0x-rest-api instance. Designed to be run post-deploy or on a cron.
#
# Usage:
#   API_URL=https://api.preview.st0x.io \
#     API_KEY=<key-id> API_SECRET=<secret> \
#     ./scripts/smoke.sh
#
# Exits 0 if all checks pass, non-zero otherwise. Prints a summary with
# per-check status + latency. Uses only curl + jq.

set -uo pipefail

API_URL="${API_URL:-https://api.preview.st0x.io}"
API_KEY="${API_KEY:-}"
API_SECRET="${API_SECRET:-}"

# Tokens to probe. Override via env if the registry changes.
USDC_BASE="${SMOKE_USDC:-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913}"
SAMPLE_OWNER="${SMOKE_OWNER:-0x71b94911fd1ce621fc40970450004c544e5287a8}"

# Latency budget per endpoint, in ms. Failures over budget are warnings, not
# hard failures, so a flaky network doesn't sink CI; tune if real regressions
# slip through.
LATENCY_BUDGET_MS="${LATENCY_BUDGET_MS:-3000}"

PASS=0
FAIL=0
WARN=0

color() {
    case "$1" in
        green) printf '\033[32m%s\033[0m' "$2" ;;
        red)   printf '\033[31m%s\033[0m' "$2" ;;
        yellow) printf '\033[33m%s\033[0m' "$2" ;;
        *) printf '%s' "$2" ;;
    esac
}

# probe NAME METHOD PATH EXPECTED_STATUS [JQ_FILTER]
# The optional JQ_FILTER must produce a non-null, non-empty value for the
# check to pass — used to assert on response shape, not just status code.
probe() {
    local name="$1"
    local method="$2"
    local path="$3"
    local expected_status="$4"
    local jq_filter="${5:-}"
    local auth_header=""
    if [[ -n "$API_KEY" && -n "$API_SECRET" ]]; then
        auth_header="-u $API_KEY:$API_SECRET"
    fi

    local tmp
    tmp=$(mktemp)
    # shellcheck disable=SC2086
    local result
    result=$(curl -sS -X "$method" $auth_header \
        -o "$tmp" \
        -w '%{http_code} %{time_total}\n' \
        --max-time 30 \
        "$API_URL$path" 2>&1) || true

    local status time_s
    status=$(echo "$result" | awk '{print $1}')
    time_s=$(echo "$result" | awk '{print $2}')
    local time_ms
    time_ms=$(awk -v t="$time_s" 'BEGIN { printf "%d", t * 1000 }')

    local check_status="FAIL"
    local detail=""

    if [[ "$status" == "$expected_status" ]]; then
        if [[ -n "$jq_filter" ]]; then
            if jq -e "$jq_filter" >/dev/null 2>&1 < "$tmp"; then
                check_status="PASS"
            else
                check_status="FAIL"
                detail="(shape mismatch)"
            fi
        else
            check_status="PASS"
        fi
    else
        body=$(head -c 200 "$tmp")
        detail="(got $status, body: $body)"
    fi

    rm -f "$tmp"

    local latency_marker=""
    if [[ "$check_status" == "PASS" && "$time_ms" -gt "$LATENCY_BUDGET_MS" ]]; then
        latency_marker=" $(color yellow SLOW)"
        WARN=$((WARN + 1))
    fi

    case "$check_status" in
        PASS)
            printf '  [%s] %-50s %4dms%s\n' "$(color green PASS)" "$name" "$time_ms" "$latency_marker"
            PASS=$((PASS + 1))
            ;;
        *)
            printf '  [%s] %-50s %4dms %s\n' "$(color red FAIL)" "$name" "$time_ms" "$detail"
            FAIL=$((FAIL + 1))
            ;;
    esac
}

echo "smoke tests against $API_URL"
echo "  budget per check: ${LATENCY_BUDGET_MS}ms"
echo

# 1. Public endpoints (no auth)
probe "GET /health"                          GET "/health" 200 '.status == "ok"'
probe "GET /health/detailed"                 GET "/health/detailed" 200 '.status'
probe "GET /health/detailed has cache_warmer" GET "/health/detailed" 200 '.cache_warmer'

# 2. Protected endpoints reject missing/invalid auth
SAVED_KEY="$API_KEY"; SAVED_SECRET="$API_SECRET"
API_KEY="" API_SECRET=""
probe "GET /v1/tokens (no auth)"             GET "/v1/tokens" 401
API_KEY="$SAVED_KEY"; API_SECRET="$SAVED_SECRET"

# 3. Authenticated endpoints — only run if creds are set
if [[ -n "$API_KEY" && -n "$API_SECRET" ]]; then
    probe "GET /v1/tokens"                       GET "/v1/tokens" 200 '.tokens | type == "array"'
    probe "GET /v1/orders/token/{usdc}"          GET "/v1/orders/token/$USDC_BASE" 200 '.orders | type == "array" and .pagination'
    probe "GET /v1/orders/owner/{owner}"         GET "/v1/orders/owner/$SAMPLE_OWNER" 200 '.orders | type == "array"'
    probe "GET /v1/trades/token/{usdc}"          GET "/v1/trades/token/$USDC_BASE?pageSize=10" 200 '.trades | type == "array"'
    probe "GET /v1/trades/{owner}"               GET "/v1/trades/$SAMPLE_OWNER?pageSize=10" 200 '.trades | type == "array"'
    # Path validation only kicks in after auth succeeds — Rocket auth fairing
    # runs first, so an invalid-address probe without auth would 401.
    probe "GET /v1/orders/token/<bad>"           GET "/v1/orders/token/not-an-address" 422
else
    echo "  (skipping authenticated checks; set API_KEY + API_SECRET to enable)"
fi

echo
echo "summary: $(color green "$PASS pass"), $(color red "$FAIL fail"), $(color yellow "$WARN slow")"

if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
exit 0
