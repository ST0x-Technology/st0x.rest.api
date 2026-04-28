#!/usr/bin/env bash
# uptimerobot-setup.sh — Creates the 3 baseline monitors against
# `${API_URL}` via UptimeRobot's REST API. Run once per environment
# (preview, prod). Re-running creates duplicates, so check the dashboard
# first if you're not sure whether they already exist.
#
# Usage:
#   UPTIMEROBOT_API_KEY=<your-main-api-key> ./scripts/uptimerobot-setup.sh
#
#   # Override target (default: https://api.preview.st0x.io)
#   API_URL=https://api.st0x.io \
#     UPTIMEROBOT_API_KEY=... ./scripts/uptimerobot-setup.sh
#
#   # Attach an existing alert contact (Telegram, email, etc.) at creation:
#   UPTIMEROBOT_API_KEY=... \
#     ALERT_CONTACT_ID=8253505 \
#     ./scripts/uptimerobot-setup.sh
#
# Get your API key from: https://uptimerobot.com/integrations/ → "Main API Key".

set -uo pipefail

: "${UPTIMEROBOT_API_KEY:?UPTIMEROBOT_API_KEY is required}"
API_URL="${API_URL:-https://api.preview.st0x.io}"
INTERVAL_SECONDS="${INTERVAL:-300}"  # 5 minutes (free-tier minimum)
# Optional: attach an existing alert contact (e.g. a Telegram integration).
# Discover candidate IDs with:
#   curl -sS -X POST -d api_key=$UPTIMEROBOT_API_KEY -d format=json \
#     https://api.uptimerobot.com/v2/getAlertContacts | jq '.alert_contacts'
ALERT_CONTACT_ID="${ALERT_CONTACT_ID:-}"

# Use the full hostname as the friendly-name prefix so alerts (especially
# Telegram pushes that show only the friendly_name) immediately identify
# which environment fired. Override with FRIENDLY_LABEL if you'd rather
# something shorter.
DEFAULT_LABEL=$(echo "$API_URL" | sed -E 's|https?://||; s|/.*$||')
LABEL="${FRIENDLY_LABEL:-$DEFAULT_LABEL}"

echo "Creating UptimeRobot monitors for $API_URL (label: $LABEL)"
echo

# create_monitor NAME URL TYPE THRESHOLD_MIN [KEYWORD]
# TYPE: 1 = HTTP(s) status, 2 = HTTP(s) keyword
# THRESHOLD_MIN: minutes of detected down before alerting (0 = immediate).
create_monitor() {
    local name="$1"
    local url="$2"
    local type="$3"
    local threshold="$4"
    local keyword="${5:-}"

    local args=(
        --data-urlencode "api_key=$UPTIMEROBOT_API_KEY"
        --data-urlencode "format=json"
        --data-urlencode "friendly_name=$name"
        --data-urlencode "url=$url"
        --data-urlencode "type=$type"
        --data-urlencode "interval=$INTERVAL_SECONDS"
    )

    if [[ -n "$ALERT_CONTACT_ID" ]]; then
        args+=(--data-urlencode "alert_contacts=${ALERT_CONTACT_ID}_${threshold}_0")
    fi

    if [[ "$type" == "2" && -n "$keyword" ]]; then
        # 1 = "Exists" — alert when keyword is NOT found in body.
        args+=(
            --data-urlencode "keyword_type=1"
            --data-urlencode "keyword_case_type=0"
            --data-urlencode "keyword_value=$keyword"
        )
    fi

    local resp
    resp=$(curl -sS -X POST "${args[@]}" \
        https://api.uptimerobot.com/v2/newMonitor)

    local stat id
    stat=$(echo "$resp" | jq -r '.stat // "unknown"')
    if [[ "$stat" == "ok" ]]; then
        id=$(echo "$resp" | jq -r '.monitor.id // "?"')
        echo "  [ok] $name (id=$id, threshold=${threshold}min)"
    else
        echo "  [fail] $name"
        echo "    response: $resp"
    fi
}

# Threshold 0 → page immediately on first detected failure (hard down).
# Threshold 5 → wait one extra check interval before paging (avoids
# flapping during deploy restarts and the post-restart cache-warmer
# transient).
create_monitor "$LABEL — liveness — /health" \
    "$API_URL/health" 1 0

create_monitor "$LABEL — component health — /health/detailed status=ok" \
    "$API_URL/health/detailed" 2 5 '"status":"ok"'

create_monitor "$LABEL — cache warmer — /health/detailed running=true" \
    "$API_URL/health/detailed" 2 5 '"running":true'

echo
if [[ -n "$ALERT_CONTACT_ID" ]]; then
    echo "Alert contact $ALERT_CONTACT_ID attached to all 3 monitors."
else
    echo "No ALERT_CONTACT_ID set — monitors won't page until you wire up alert"
    echo "contacts via the dashboard or re-run with ALERT_CONTACT_ID set."
fi
