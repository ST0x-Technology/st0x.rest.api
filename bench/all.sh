#!/usr/bin/env bash
# Full local cutover comparison: discover, bench both hosts, compare.
# Usage: bench/all.sh
# Output: bench/results/<ts>-compare.md

set -euo pipefail
cd "$(dirname "$0")/.."

# shellcheck source=bench/lib.sh
. bench/lib.sh
load_env

echo "=== Discovering fixtures from prod ==="
bench/discover.sh prod

echo "=== Benching prod ==="
prod_json="$(bench/run.sh prod)"

echo "=== Benching preview ==="
preview_json="$(bench/run.sh preview)"

# Use prod result's timestamp prefix so report and source JSONs share a stem.
prod_ts="$(basename "$prod_json" | sed 's/-prod\.json$//')"
report="bench/results/${prod_ts}-compare.md"

echo "=== Comparing ==="
bench/compare.sh "$prod_json" "$preview_json" "$report"

echo
echo "Report: $report"
