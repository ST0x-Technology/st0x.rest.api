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
    try:
        import tomli as tomllib
    except ImportError:
        sys.exit("ERROR: bench/run.sh requires Python 3.11+ (for tomllib) "
                 "or the 'tomli' package (pip install tomli).")
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
  if [ -n "$discover_keys" ]; then
    for key in $discover_keys; do
      if grep -q "{$key}" <<<"$path$body"; then
        skip_reason="missing fixture: $key"
        break
      fi
    done
  fi

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

  oha_args=( -n "$reqs" -c "$conc" -t "${timeout_s}s" --no-tui --output-format json -m "$method" )
  if [ "$auth_kind" = "basic" ] && [ -n "$b64" ]; then
    oha_args+=( -H "Authorization: Basic $b64" )
  fi
  if [ -n "$body" ]; then
    oha_args+=( -H "Content-Type: application/json" -d "$body" )
  fi

  oha_out="$tmp/${name}.json"
  oha_err="$tmp/${name}.err"
  if ! oha "${oha_args[@]}" "$url" > "$oha_out" 2>"$oha_err"; then
    err_persist="bench/results/.${name}.err"
    cp "$oha_err" "$err_persist" 2>/dev/null || true
    echo "[run] WARN oha exited non-zero for $name (err saved to $err_persist)" >&2
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

  # Build the summary block. Schema matches `oha --output-format json` (oha 1.14+).
  # NOTE: oha's own `summary.successRate` is "request completed without transport
  # error" — a 404/500 still counts. We must derive HTTP-level success ourselves
  # from statusCodeDistribution (status < 400 = success).
  summary="$(jq '
    ((.statusCodeDistribution // {})
       | (if type == "object" then to_entries else [] end)) as $codes |
    ($codes | map(.value) | add // 0) as $total |
    ($codes | map(select(.key|tonumber < 400)) | map(.value) | add // 0) as $ok |
    ($total | if . == 0 then 1 else . end) as $denom |
    {
      total_requests: $total,
      success_count:  $ok,
      success_rate:   ($ok / $denom),
      error_rate:     (1 - ($ok / $denom)),
      p50_ms: (.latencyPercentiles.p50 | if . == null then null else . * 1000 end),
      p95_ms: (.latencyPercentiles.p95 | if . == null then null else . * 1000 end),
      p99_ms: (.latencyPercentiles.p99 | if . == null then null else . * 1000 end),
      rps:    (.summary.requestsPerSec // 0),
      status_codes: ($codes | map({(.key): .value}) | add // {})
    }
  ' "$oha_out" 2>/dev/null)"

  if [ -z "$summary" ]; then
    echo "[run] ERROR $name summary computation failed; recording as skipped" >&2
    jq --arg name "$name" --arg method "$method" --arg path "$raw_path" \
       --arg reason "summary computation failed" \
       '.endpoints += [{name:$name, method:$method, path:$path,
         skipped:true, skip_reason:$reason, oha:null, summary:null}]' \
       "$out" > "$tmp/next" && mv "$tmp/next" "$out"
    continue
  fi

  jq --arg name "$name" --arg method "$method" --arg path "$raw_path" \
     --slurpfile oha "$oha_out" --argjson summary "$summary" \
     '.endpoints += [{name:$name, method:$method, path:$path,
       skipped:false, skip_reason:null, oha:$oha[0], summary:$summary}]' \
     "$out" > "$tmp/next" && mv "$tmp/next" "$out"
done

echo "$out"
