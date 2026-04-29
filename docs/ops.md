# Operations cheat sheet

Quick journalctl + curl recipes for the deployed `rest-api` service. SSH in with `nix develop -c remote` (or `ssh root@<host>` if your key is in `roles.ssh`).

## Service health

```bash
# Quick liveness probe (no auth)
curl -sS https://api.preview.st0x.io/health | jq

# Full status — includes db connectivity, raindex sync, cache_warmer
curl -sS https://api.preview.st0x.io/health/detailed | jq
```

Key fields in `/health/detailed.cache_warmer`:
- `running` — `false` until the warmer completes its first cycle (~15-30s after restart while caches are cold)
- `last_cycle_ms` — should track the steady-state cycle duration; sustained high values suggest upstream RPC slowness
- `seconds_since_last_complete` — should bounce between `0` and roughly the 5 minute refresh interval plus cycle duration; much higher means the warmer has frozen
- `last_errors` — per-token failures during the last cycle; non-zero is worth investigating

## Common journalctl queries

All queries run via `ssh root@api.preview.st0x.io '...'` or after `nix develop -c remote`.

### 429 rate

```bash
# Count in the last hour
journalctl -u rest-api --since '1 hour ago' --no-pager | grep -c 'error code 429'

# Per-RPC breakdown (when the backing RPC is identifiable from the error body)
journalctl -u rest-api --since '1 hour ago' --no-pager \
  | grep -oE 'error code -32016|error code 429|StalePrice' \
  | sort | uniq -c
```

### Cache warmer cycles

```bash
# Last 10 cycle durations + completion timestamps
journalctl -u rest-api --since '10 minutes ago' --no-pager \
  | grep 'cache warmer: orders-by-token refresh complete' \
  | sed -E 's/.*timestamp":"([^"]+)".*duration_ms":"?([0-9]+)"?.*/\1  cycle_ms=\2/' \
  | tail -10
```

### ERROR-level rate

```bash
journalctl -u rest-api --since '5 minutes ago' --no-pager \
  | grep -c 'level":"ERROR'
```

Most ERROR lines are benign (`No matching routes for HEAD /health` from external uptime checkers, or `task NNNN was cancelled` during graceful restart). Real signal:
- `failed to query orders` outside a deploy window
- `applied RPC override` should appear once on startup with the expected `url_count`

### Slow requests

```bash
# Requests > 5s in the last hour (raw rocket access logs)
journalctl -u rest-api --since '1 hour ago' --no-pager \
  | grep 'request completed' \
  | grep -oE 'duration_ms":[0-9]+\.[0-9]+' \
  | awk -F: '$2 > 5000 { print }' \
  | wc -l
```

## Smoke tests

```bash
# Run the smoke battery against the live preview
API_KEY=<id> API_SECRET=<secret> ./scripts/smoke.sh

# Override target
API_URL=https://api.st0x.io API_KEY=... API_SECRET=... ./scripts/smoke.sh
```

The script returns non-zero on any FAIL. Run post-deploy or wire into a cron + alert. SLOW (over `LATENCY_BUDGET_MS=3000`) is reported as a warning, not a failure.

## Suggested cron / external monitoring

A minimal external probe (run from any machine that can reach the public hostname):

```bash
# Run every 5 minutes; alert on non-zero exit or 502/503 in the body
*/5 * * * * cd /path/to/st0x.rest.api && \
  API_KEY=... API_SECRET=... ./scripts/smoke.sh > /tmp/smoke.last 2>&1 || \
  alert-channel "smoke failed: $(tail -5 /tmp/smoke.last)"
```

Higher-fidelity options (Prometheus + Grafana, Datadog, etc.) are deferred — the smoke + journalctl recipes cover most regressions for a single-instance preview.
