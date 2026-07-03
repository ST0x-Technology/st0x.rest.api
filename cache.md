# Caching Strategy

This page documents the current ST0x API caching model and the intended policy
for each class of data. The API currently runs as a single process with local
memory caches plus SQLite-backed local/indexer state. There is no shared cache
layer such as Redis, and the API does not currently emit `Cache-Control` or
`ETag` headers for client-side HTTP caching.

## Current Cache Inventory

| Cache                          | Location                                                                                   | Contents                                                                                                                                            | TTL / retention                                                                                                                                                                                                             | Invalidation                                                                                                                                                        |
| ------------------------------ | ------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Route response caches          | `src/cache.rs` via `ApplicationState.response_caches`                                      | Order quotes, orders by token, swap candidates, trades by token, trades by taker                                                                    | Configured by `response_cache_ttl_seconds`; production and preview use 5 seconds. Disabled when `response_cache_max_entries = 0` or TTL is 0, as in dev. Each cache has its own `response_cache_max_entries` Moka capacity. | Time-based expiry; process restart; `/admin/reload-registry` invalidates all route response caches. Errors are not cached. Concurrent misses are coalesced by Moka. |
| Token details aggregate cache  | `src/routes/token_details.rs`                                                              | Holder count, transfer count, deposit volume, and withdraw volume for SFT-backed token details, keyed by SFT subgraph URL and wrapped token address | Fixed 5 minutes, max 512 aggregate entries                                                                                                                                                                                  | Time-based expiry; process restart. Tests can clear it directly.                                                                                                    |
| Token details list cache       | `src/routes/token_details.rs`                                                              | Batch `/v1/tokens/details` responses, keyed by the sorted requested token set and SFT subgraph URLs                                                 | Fixed 5 minutes, max 64 list responses                                                                                                                                                                                      | Time-based expiry; process restart. Partial responses are not cached.                                                                                               |
| Raindex local DB               | configured by `local_db_path` and owned by `rain_orderbook_common`                         | Local index of configured raindex orderbook state used by list/detail/trade queries                                                                 | Persistent SQLite state. Freshness is controlled by the raindex sync scheduler, not an API TTL.                                                                                                                             | Updated by the raindex local DB sync process. Health is exposed in `/health/detailed` with per-network and per-orderbook sync state.                                |
| Wrapped exchange rate history  | app SQLite table `wrapped_exchange_rate_snapshots`                                         | Best-effort snapshots of ERC4626 wrapped-token ratios captured while serving wrap-ratio, order, trade, and swap requests                            | Persistent history; no TTL or pruning policy today                                                                                                                                                                          | `INSERT OR IGNORE` on observed `(share_token_address, block_number)`-style snapshots. New reads append data opportunistically.                                      |
| Registry / token configuration | `RaindexProvider` loaded from `registry_url`                                               | Curated tokens, networks, deployments, orderbook settings                                                                                           | Held in memory for the lifetime of the provider                                                                                                                                                                             | `/admin/reload-registry` reloads the provider from the configured registry source and invalidates route response caches.                                            |
| Request-local caches           | Rocket `Request::local_cache` in auth, request logging, usage logging, rate-limit handling | Per-request metadata such as auth key ID, tracing metadata, start time, and cached rate-limit info                                                  | Request lifetime only                                                                                                                                                                                                       | Dropped at the end of each request.                                                                                                                                 |

## Route Response Cache Details

`RouteResponseCaches` wraps five independent Moka caches:

- `order_quotes`: expensive quote results for order summaries/details.
- `orders_by_token`: paginated `/v1/orders/token/{address}` responses keyed by
  normalized address, state, side, page, page size, and denomination.
- `swap_candidates`: computed take-order candidates for a token pair and the
  exact active order set used to build the route.
- `trades_by_token`: paginated `/v1/trades/token/{address}` responses keyed by
  normalized address, pagination, time filters, and denomination.
- `trades_by_taker`: paginated `/v1/trades/taker/{address}` responses keyed the
  same way as token trade responses.

These caches are intentionally short-lived because they sit in front of data
that can change quickly with new blocks, vault balance changes, order removals,
and trades. They are latency and upstream-load caches, not correctness sources.
On a cache miss, the API asks raindex/RPC/subgraphs for current data and stores
only successful responses.

## Data Class Policy

| Data class                          | Current policy                                                                                                                                                                                                                                                                        | Target policy                                                                                                                                                                                                                                                                                                     |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Quotes and swap candidates          | Cache briefly in process memory. Production/preview TTL is 5 seconds. Keys must include every input that changes semantics, including token pair, order set, pagination, filters, and denomination.                                                                                   | Keep TTL short, normally 5 seconds. Prefer coalesced in-memory cache misses before adding a shared cache because correctness depends on fast-moving chain/orderbook state. Add explicit metrics before increasing TTL.                                                                                            |
| Prices and wrapped ratios           | Current wrapped ratios are read live from ERC4626/RPC paths. Successful observations are persisted as history, but current ratio endpoints are not served from the history table. Token details aggregate volumes are cached for 5 minutes because they are expensive subgraph scans. | Current price/ratio reads should remain live or very short-lived unless the response includes freshness metadata. Historical ratios can be read from SQLite because they are immutable once captured. If RPC load becomes a problem, add a small in-memory ratio cache keyed by token and block/freshness window. |
| Token list and registry config      | Token list reads come from the in-memory raindex provider loaded from the registry. The provider is refreshed by admin registry reload, not by TTL.                                                                                                                                   | Treat registry data as configuration. Refresh explicitly via `/admin/reload-registry`; do not add time-based token-list caching unless registry reload semantics change.                                                                                                                                          |
| Token details                       | Aggregate SFT details and complete batch responses are cached for 5 minutes. Partial batch responses are not cached.                                                                                                                                                                  | Keep 5 minutes while aggregate scans require full subgraph pagination. If subgraphs provide indexed aggregate entities later, reduce or remove this cache.                                                                                                                                                        |
| Orders, trades, vault/indexer state | Raindex local DB is the durable read model. API response caches are short-lived overlays for high-fanout endpoints.                                                                                                                                                                   | Keep local DB as the source of truth for indexed state. Surface freshness through `/health/detailed`, and keep API response TTLs short enough that stale reads are bounded even if the local DB is healthy but slightly behind.                                                                                   |
| Admin and write-adjacent responses  | Admin registry reload invalidates route response caches. Write-adjacent calldata/cancel/deploy flows should not rely on long-lived cached state.                                                                                                                                      | Any new mutation or registry-changing path must invalidate affected route caches or bypass caching.                                                                                                                                                                                                               |

## Invalidation Rules

Use these rules when adding or changing cached behavior:

- Include all request parameters and implicit defaults in cache keys. Normalize
  addresses to lowercase and normalize default pagination/filter values.
- Do not cache errors or partial responses unless the caller contract explicitly
  says a partial response is a stable result.
- Keep volatile orderbook, quote, swap, and trade data on a short TTL. The
  current production default is 5 seconds.
- Invalidate route response caches when registry configuration changes. Today
  `/admin/reload-registry` calls `response_caches.invalidate_all()`.
- Prefer durable SQLite tables for historical facts, such as wrapped exchange
  rate snapshots, instead of expanding process memory caches.
- Treat the raindex local DB sync state as freshness metadata. If
  `/health/detailed` reports syncing or failure, cached API responses may still
  be fast but should not be treated as fresh.

## Gaps and Opportunities

- Add cache observability for route response caches: hit/miss counters, insert
  counts, eviction counts, and per-cache latency impact. This should come before
  TTL changes.
- Add explicit freshness fields or headers for cached endpoints so clients can
  distinguish fast cached responses from freshly computed responses.
- Consider a very short-lived wrapped-ratio cache for repeated unwrapped
  denomination requests. This could reduce repeated ERC4626/RPC reads during
  bursts while keeping staleness bounded.
- Consider targeted response caching for token list output if `/v1/tokens`
  becomes a hot endpoint. The source data is already in memory, so this is only
  useful if response mapping/serialization becomes measurable.
- Review whether orders-by-owner and order detail endpoints need the same
  response-cache treatment as orders-by-token if they become high-traffic paths.
- Add a documented pruning/retention policy for
  `wrapped_exchange_rate_snapshots` if the history table grows materially in
  production.
- Align operations documentation with the current `/health/detailed` schema. The
  current health endpoint exposes raindex local DB sync state rather than a
  first-class cache warmer object.
