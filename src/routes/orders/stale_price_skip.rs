use crate::cache::AppCache;
use alloy::primitives::B256;
use std::time::Duration;

const STALE_SKIP_TTL: Duration = Duration::from_secs(7 * 86_400);
const STALE_SKIP_CAPACITY: u64 = 10_000;

/// Set of order_hashes that have returned `StalePrice` from a quote.
///
/// The TTL is long (7 days) because the marker only becomes a no-op when
/// NYSE re-opens (callers consult `market_calendar::is_nyse_open` before
/// honoring it). The TTL exists purely as a safety valve so a stale-marker
/// for a permanently-removed order eventually falls out of memory.
pub(crate) type StalePriceSkipCache = AppCache<B256, ()>;

pub(crate) fn stale_price_skip_cache() -> StalePriceSkipCache {
    AppCache::new(STALE_SKIP_CAPACITY, STALE_SKIP_TTL)
}

/// True if the multicall error message indicates the order's price feed is
/// stale (Chainlink-style `StalePrice` revert from the order's strategy).
pub(crate) fn quote_indicates_stale_price(error_msg: &str) -> bool {
    error_msg.contains("StalePrice")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_indicates_stale_price_matches() {
        assert!(quote_indicates_stale_price(
            "Execution reverted with error: StalePrice\n"
        ));
        assert!(quote_indicates_stale_price(
            "Multicall failed: ... StalePrice ..."
        ));
    }

    #[test]
    fn test_quote_indicates_stale_price_does_not_match_other_errors() {
        assert!(!quote_indicates_stale_price(
            "Execution reverted with error: NotEnoughBalance"
        ));
        assert!(!quote_indicates_stale_price(""));
        assert!(!quote_indicates_stale_price(
            "rate-limited until QuantaInstant(...)"
        ));
    }

    #[rocket::async_test]
    async fn test_skip_cache_set_and_get() {
        let cache = stale_price_skip_cache();
        let key = B256::from([7u8; 32]);
        assert!(cache.get(&key).await.is_none());
        cache.insert(key, ()).await;
        assert!(cache.get(&key).await.is_some());
    }
}
