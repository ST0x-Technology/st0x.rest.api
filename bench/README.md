# Endpoint Benchmark Harness

Local benchmark + reliability comparison between `api.preview.st0x.io` and
`api.st0x.io`. Used today to gate the preview-to-prod cutover; structured so
it can be wired into CI later.

## Prerequisites

- `oha` (`cargo install oha` or `nix develop`)
- `jq`
- `python3` (for parsing `endpoints.toml`)
- `curl`

The provided `nix develop` shell already has the first three.

## Setup

```bash
cp bench/.env.example bench/.env
# fill in BENCH_PROD_USER/PASS and BENCH_PREVIEW_USER/PASS
```

`bench/.env` is gitignored.

## Usage

Full comparison (discover → bench prod → bench preview → compare):

```bash
nix develop -c bench/all.sh
```

Or step by step:

```bash
nix develop -c bench/discover.sh prod
nix develop -c bench/run.sh prod
nix develop -c bench/run.sh preview
nix develop -c bench/compare.sh bench/results/<ts>-prod.json bench/results/<ts>-preview.json
```

Tunables (env vars):

- `BENCH_REQUESTS` — total requests per endpoint (default 50)
- `BENCH_CONCURRENCY` — concurrent workers (default 5)
- `BENCH_TIMEOUT_S` — per-request timeout (default 30)
- `BENCH_P95_THRESHOLD` — p95 regression threshold (default 0.25)
- `BENCH_SUCCESS_DROP` — success rate drop threshold (default 0.02)

## What's covered

15 idempotent read endpoints (see `endpoints.toml`). Mutating endpoints
(`POST /v1/order/*`, `POST /v1/order/cancel`, `PUT /admin/registry`) are
deliberately excluded.

## What this does NOT do (yet)

- Run in CI — deferred. Hooks for it: `bench/all.sh` exits non-zero only on
  hard failure; threshold crossings are advisory in the markdown report.
  Future CI work: parse `bench/results/*-compare.md` (or a future JSON
  variant) and fail the job on ⚠️ rows.
- Commit baselines to the repo — deferred. Future flow: merge-to-main runs
  `bench/run.sh prod` and commits the JSON to `bench/baseline.json`; PRs
  compare against that.

## Output

Each run produces:
- `bench/results/<ts>-<target>.json` — raw oha output + summary per endpoint
- `bench/results/<ts>-compare.md` — markdown report with ⚠️ flags

The `results/` directory is gitignored.
