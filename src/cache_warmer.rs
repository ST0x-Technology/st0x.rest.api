use crate::raindex::{BlockNumberCache, SharedRaindexProvider};
use crate::routes::orders::{
    process_get_orders_by_token, LimitOrderRatioCache, OrdersByTokenCache,
    RaindexOrdersListDataSource, StalePriceSkipCache, MAX_PAGE_SIZE,
};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};

const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// Snapshot of the cache warmer's most recent activity, exposed via
/// `/v1/health/detailed`. Mutated under a short-lived write lock at the end
/// of each cycle.
#[derive(Debug, Default, Clone)]
pub(crate) struct CacheWarmerStats {
    pub total_cycles: u64,
    pub last_cycle_ms: Option<u64>,
    pub last_tokens: Option<u32>,
    pub last_errors: Option<u32>,
    /// Unix timestamp (seconds) of the most recent cycle completion.
    pub last_complete_at_unix: Option<u64>,
}

pub(crate) type SharedCacheWarmerStats = Arc<RwLock<CacheWarmerStats>>;

pub(crate) fn shared_cache_warmer_stats() -> SharedCacheWarmerStats {
    Arc::new(RwLock::new(CacheWarmerStats::default()))
}

/// Background loop that keeps the orders-by-token cache warm.
///
/// For every token in the registry it calls `process_get_orders_by_token`
/// with `side=None, page=1, page_size=MAX_PAGE_SIZE` and inserts the result
/// into the shared cache. After each cycle the loop sleeps for
/// `REFRESH_INTERVAL` regardless of how long the cycle took, guaranteeing a
/// fixed idle gap between cycles so an over-running cycle never causes the
/// next one to start back-to-back (the pathology that produced the original
/// 429 storm).
pub(crate) async fn run_orders_by_token_warmer(
    cache: OrdersByTokenCache,
    shared_raindex: SharedRaindexProvider,
    block_number_cache: BlockNumberCache,
    limit_ratio_cache: LimitOrderRatioCache,
    stale_price_skip_cache: StalePriceSkipCache,
    stats: SharedCacheWarmerStats,
) {
    loop {
        let start = Instant::now();

        // Collect token addresses from the registry under a short-lived read lock.
        let token_addresses: Vec<alloy::primitives::Address> = {
            let raindex = shared_raindex.read().await;
            match raindex.client().get_all_tokens() {
                Ok(tokens) => tokens.values().map(|t| t.address).collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "cache warmer: failed to get token list, skipping cycle");
                    sleep(REFRESH_INTERVAL).await;
                    continue;
                }
            }
        };

        if token_addresses.is_empty() {
            tracing::debug!("cache warmer: no tokens in registry, skipping cycle");
            sleep(REFRESH_INTERVAL).await;
            continue;
        }

        let page: u16 = 1;
        let page_size: u16 = MAX_PAGE_SIZE;
        let mut ok_count: usize = 0;
        let mut err_count: usize = 0;

        for addr in &token_addresses {
            // Acquire the read lock per-token so other writers (e.g. admin
            // registry reload) are not blocked for the entire cycle.
            let result = {
                let raindex = shared_raindex.read().await;
                let ds = RaindexOrdersListDataSource {
                    client: raindex.client(),
                    block_number_cache: &block_number_cache,
                    limit_ratio_cache: &limit_ratio_cache,
                    stale_price_skip_cache: &stale_price_skip_cache,
                };
                process_get_orders_by_token(&ds, *addr, None, Some(page), Some(page_size)).await
            };

            match result {
                Ok(response) => {
                    let cache_key = (*addr, None, page, page_size);
                    cache.insert(cache_key, response).await;
                    ok_count += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        token = ?addr,
                        error = ?e,
                        "cache warmer: failed to refresh orders for token"
                    );
                    err_count += 1;
                }
            }
        }

        let cycle_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            tokens = token_addresses.len(),
            ok = ok_count,
            errors = err_count,
            duration_ms = cycle_ms,
            "cache warmer: orders-by-token refresh complete"
        );

        {
            let mut s = stats.write().await;
            s.total_cycles += 1;
            s.last_cycle_ms = Some(cycle_ms);
            s.last_tokens = Some(token_addresses.len() as u32);
            s.last_errors = Some(err_count as u32);
            s.last_complete_at_unix = Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
        }

        sleep(REFRESH_INTERVAL).await;
    }
}
