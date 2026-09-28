#!/usr/bin/env bash
# Compare two bench result JSONs and emit a markdown report.
# Usage: bench/compare.sh <baseline.json> <candidate.json> [out.md]
#   baseline = the "expected" / older / prod result
#   candidate = the "new" / preview / PR result

set -euo pipefail
cd "$(dirname "$0")/.."

# shellcheck source=bench/lib.sh
. bench/lib.sh
require_cmd jq

baseline="${1:?baseline JSON path required}"
candidate="${2:?candidate JSON path required}"
out="${3:-}"

[ -f "$baseline" ]  || { echo "missing: $baseline" >&2; exit 1; }
[ -f "$candidate" ] || { echo "missing: $candidate" >&2; exit 1; }

# p95 increase threshold (fraction): 0.25 = +25%
P95_THRESHOLD="${BENCH_P95_THRESHOLD:-0.25}"
# success-rate drop threshold (absolute, fraction): 0.02 = 2 percentage points
SUCCESS_DROP_THRESHOLD="${BENCH_SUCCESS_DROP:-0.02}"

render() {
  jq -rn \
    --slurpfile b "$baseline" \
    --slurpfile c "$candidate" \
    --argjson p95t "$P95_THRESHOLD" \
    --argjson sdt "$SUCCESS_DROP_THRESHOLD" '
    def f3: . * 1000 | round / 1000;
    def pct: . * 100 | round / 100;
    def fmt_ms($x):
      if ($x|type) == "number" then "\($x | f3) ms" else "—" end;
    def fmt_pct($x):
      if ($x|type) == "number" then "\($x*100 | pct)%" else "—" end;
    def delta($a;$b):
      if ($a|type) == "number" and ($b|type) == "number" and $a > 0
      then (($b - $a) / $a * 100 | pct | tostring) + "%"
      else "—"
      end;

    ($b[0]) as $base |
    ($c[0]) as $cand |
    (reduce ($base.endpoints + $cand.endpoints)[] as $e ({}; .[$e.name] = true)
     | keys) as $names |

    "# Bench Comparison\n\n" +
    "- **Baseline:** `\($base.target)` @ `\($base.host)` — \($base.started_at)\n" +
    "- **Candidate:** `\($cand.target)` @ `\($cand.host)` — \($cand.started_at)\n" +
    "- **Load:** \($base.load.requests) reqs × \($base.load.concurrency) concurrent (per endpoint)\n" +
    "- **Thresholds (advisory):** p95 +\($p95t*100|pct)% · success drop \($sdt*100|pct)pp\n\n" +

    "| Endpoint | Status | Base p95 | Cand p95 | Δ p95 | Base success | Cand success | Δ success | Notes |\n" +
    "|---|---|---:|---:|---:|---:|---:|---:|---|\n" +

    ([ $names[] as $n |
       (first($base.endpoints[]? | select(.name == $n)) // null) as $bb |
       (first($cand.endpoints[]? | select(.name == $n)) // null) as $cc |
       ($bb.summary.p95_ms       // null) as $bp |
       ($cc.summary.p95_ms       // null) as $cp |
       ($bb.summary.success_rate // null) as $bs |
       ($cc.summary.success_rate // null) as $cs |
       (
         if $bp == null or $cp == null then false
         elif $bp <= 0 then false
         else (($cp - $bp) / $bp) > $p95t end
       ) as $p95_bad |
       (
         if $bs == null or $cs == null then false
         else ($bs - $cs) > $sdt end
       ) as $succ_bad |
       (
         [
           if $bb == null then "missing in baseline" else empty end,
           if $cc == null then "missing in candidate" else empty end,
           if ($bb // {}).skipped == true then "baseline: \($bb.skip_reason // "skipped")" else empty end,
           if ($cc // {}).skipped == true then "candidate: \($cc.skip_reason // "skipped")" else empty end,
           if $p95_bad  then "p95 regression"        else empty end,
           if $succ_bad then "success-rate regression" else empty end
         ] | join("; ")
       ) as $notes |
       (if ($p95_bad or $succ_bad) then "⚠️"
        elif ($bb == null or $cc == null
              or ($bb.skipped // false) or ($cc.skipped // false)) then "—"
        else "✓" end) as $status |
       "| `\($n)` | \($status) | \(fmt_ms($bp)) | \(fmt_ms($cp)) | \(delta($bp;$cp)) | \(fmt_pct($bs)) | \(fmt_pct($cs)) | \(delta($bs;$cs)) | \($notes) |"
     ] | join("\n")) +

    "\n"
  '
}

if [ -n "$out" ]; then
  render | tee "$out"
else
  render
fi
