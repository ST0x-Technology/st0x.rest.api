#!/usr/bin/env bash
# Run the benchmark suite against a single target host.
# Usage: bench/run.sh [prod|preview]
# Output: bench/results/<UTC-timestamp>-<target>.json (path printed on stdout)

set -euo pipefail
cd "$(dirname "$0")/.."

# shellcheck source=bench/lib.sh
. bench/lib.sh
require_cmd oha
require_cmd jq
require_cmd python3
load_env

target="${1:-}"
case "$target" in
  prod)    host="${BENCH_PROD_HOST:-}"; user="${BENCH_PROD_USER:-}"; pass="${BENCH_PROD_PASS:-}" ;;
  preview) host="${BENCH_PREVIEW_HOST:-}"; user="${BENCH_PREVIEW_USER:-}"; pass="${BENCH_PREVIEW_PASS:-}" ;;
  *) echo "usage: $0 [prod|preview]" >&2; exit 2 ;;
esac

if [ -z "${host:-}" ]; then
  echo "ERROR: missing $target host in bench/.env" >&2
  exit 1
fi

reqs="${BENCH_REQUESTS:-50}"
conc="${BENCH_CONCURRENCY:-5}"
timeout_s="${BENCH_TIMEOUT_S:-30}"

discovered="bench/results/.discovered.env"
if [ ! -f "$discovered" ]; then
  echo "ERROR: $discovered not found. Run bench/discover.sh first." >&2
  exit 1
fi

ts="$(date -u +%Y%m%dT%H%M%SZ)"
out="bench/results/${ts}-${target}.json"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Parse TOML → JSON via python's tomllib (3.11+) or tomli fallback.
endpoints_json="$(python3 - <<'PY'
import json, sys
try:
    import tomllib
except ImportError:
    import tomli as tomllib
with open("bench/endpoints.toml", "rb") as f:
    print(json.dumps(tomllib.load(f)))
PY
)"

b64=""
if [ -n "${user:-}" ] && [ -n "${pass:-}" ]; then
  b64="$(basic_b64 "$user" "$pass")"
fi

# Build the result skeleton.
jq -n \
  --arg target "$target" \
  --arg host "$host" \
  --arg started_at "$(date -u +%FT%TZ)" \
  --argjson reqs "$reqs" \
  --argjson conc "$conc" \
  '{target:$target, host:$host, started_at:$started_at,
    load:{requests:$reqs, concurrency:$conc},
    endpoints:[]}' > "$out"

n_endpoints="$(echo "$endpoints_json" | jq '.endpoint | length')"
echo "[run] target=$target host=$host endpoints=$n_endpoints requests=$reqs concurrency=$conc" >&2

for i in $(seq 0 $((n_endpoints - 1))); do
  ep="$(echo "$endpoints_json" | jq -c ".endpoint[$i]")"
  name="$(echo "$ep" | jq -r '.name')"
  method="$(echo "$ep" | jq -r '.method')"
  raw_path="$(echo "$ep" | jq -r '.path')"
  auth_kind="$(echo "$ep" | jq -r '.auth')"
  body_template="$(echo "$ep" | jq -r '.body // empty')"
  discover_keys="$(echo "$ep" | jq -r '.discover // [] | join(" ")')"

  # Resolve placeholders.
  path="$(resolve_placeholders "$raw_path" "$discovered")"
  body="$(resolve_placeholders "$body_template" "$discovered")"

  # Skip if any required placeholder still unresolved.
  skip_reason=""
  for key in $discover_keys; do
    if grep -q "{$key}" <<<"$path$body"; then
      skip_reason="missing fixture: $key"
      break
    fi
  done

  if [ -n "$skip_reason" ]; then
    echo "[run] SKIP $name — $skip_reason" >&2
    jq --arg name "$name" --arg method "$method" --arg path "$raw_path" \
       --arg reason "$skip_reason" \
       '.endpoints += [{name:$name, method:$method, path:$path,
         skipped:true, skip_reason:$reason, oha:null, summary:null}]' \
       "$out" > "$tmp/next" && mv "$tmp/next" "$out"
    continue
  fi

  url="$host$path"
  echo "[run] $method $url" >&2

  oha_args=( -n "$reqs" -c "$conc" -t "${timeout_s}s" --no-tui -j -m "$method" )
  if [ "$auth_kind" = "basic" ] && [ -n "$b64" ]; then
    oha_args+=( -H "Authorization: Basic $b64" )
  fi
  if [ -n "$body" ]; then
    oha_args+=( -H "Content-Type: application/json" -d "$body" )
  fi

  oha_out="$tmp/${name}.json"
  if ! oha "${oha_args[@]}" "$url" > "$oha_out" 2>"$tmp/${name}.err"; then
    echo "[run] WARN oha exited non-zero for $name (see $tmp/${name}.err)" >&2
    # Still capture output if any; mark skipped on parse failure below.
  fi

  if ! jq empty "$oha_out" 2>/dev/null; then
    echo "[run] ERROR $name produced unparseable output; recording as skipped" >&2
    jq --arg name "$name" --arg method "$method" --arg path "$raw_path" \
       --arg reason "oha output unparseable" \
       '.endpoints += [{name:$name, method:$method, path:$path,
         skipped:true, skip_reason:$reason, oha:null, summary:null}]' \
       "$out" > "$tmp/next" && mv "$tmp/next" "$out"
    continue
  fi

  # Build the summary block from oha's JSON. Field names match `oha -j` schema.
  summary="$(jq '{
    total_requests:        (.summary.requests          // .summary.total           // 0),
    success_count:         ((.statusCodeDistribution // {}) | to_entries
                              | map(select(.key|tonumber < 400)) | map(.value) | add // 0),
    success_rate:          (((.statusCodeDistribution // {}) | to_entries
                              | map(select(.key|tonumber < 400)) | map(.value) | add // 0)
                            / ((.summary.requests // .summary.total // 1) | if . == 0 then 1 else . end)),
    p50_ms:                ((.latencyPercentiles.p50  // .latency_percentiles.p50  // 0) * 1000),
    p95_ms:                ((.latencyPercentiles.p95  // .latency_percentiles.p95  // 0) * 1000),
    p99_ms:                ((.latencyPercentiles.p99  // .latency_percentiles.p99  // 0) * 1000),
    rps:                   (.summary.requestsPerSec   // .summary.rps              // 0),
    error_rate:            (1 - (((.statusCodeDistribution // {}) | to_entries
                              | map(select(.key|tonumber < 400)) | map(.value) | add // 0)
                            / ((.summary.requests // .summary.total // 1) | if . == 0 then 1 else . end)))
  }' "$oha_out")"

  jq --arg name "$name" --arg method "$method" --arg path "$raw_path" \
     --slurpfile oha "$oha_out" --argjson summary "$summary" \
     '.endpoints += [{name:$name, method:$method, path:$path,
       skipped:false, skip_reason:null, oha:$oha[0], summary:$summary}]' \
     "$out" > "$tmp/next" && mv "$tmp/next" "$out"
done

echo "$out"
