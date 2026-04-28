use crate::cache::AppCache;
use alloy::primitives::B256;
use rain_orderbook_common::parsed_meta::ParsedMeta;
use rain_orderbook_common::raindex_client::orders::RaindexOrder;
use std::time::Duration;

const LIMIT_RATIO_CACHE_TTL: Duration = Duration::from_secs(86_400); // 24h
const LIMIT_RATIO_CACHE_CAPACITY: u64 = 10_000;

/// Cache of `formatted_ratio` keyed by order_hash for limit orders.
///
/// Limit orders by definition have a fixed io_ratio that doesn't depend on
/// block state, so once we've quoted one we can reuse the ratio without
/// burning RPC. The 24h TTL is a safety net so a poisoned cache entry
/// self-heals after a day.
pub(crate) type LimitOrderRatioCache = AppCache<B256, String>;

pub(crate) fn limit_order_ratio_cache() -> LimitOrderRatioCache {
    AppCache::new(LIMIT_RATIO_CACHE_CAPACITY, LIMIT_RATIO_CACHE_TTL)
}

/// True when the order's `selected_deployment` metadata identifies it as a
/// limit order (e.g. `fixed-limit`). Returns false for orders with no
/// dotrain GUI metadata, or with a non-limit deployment.
pub(crate) fn is_limit_order(order: &RaindexOrder) -> bool {
    for meta in order.parsed_meta() {
        if let ParsedMeta::DotrainGuiStateV1(gui_state) = meta {
            if gui_state
                .selected_deployment
                .to_lowercase()
                .contains("limit")
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::order_json;
    use serde_json::json;

    fn order_with_deployment(deployment: &str) -> RaindexOrder {
        let mut value = order_json();
        value["parsedMeta"] = json!([{
            "DotrainGuiStateV1": {
                "dotrain_hash": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "field_values": {},
                "deposits": {},
                "select_tokens": {},
                "vault_ids": {},
                "selected_deployment": deployment,
            }
        }]);
        serde_json::from_value(value).expect("deserialize order with parsedMeta")
    }

    #[test]
    fn test_is_limit_order_true_for_fixed_limit() {
        let order = order_with_deployment("fixed-limit-buy-base");
        assert!(is_limit_order(&order));
    }

    #[test]
    fn test_is_limit_order_true_for_uppercase_limit() {
        let order = order_with_deployment("Fixed-LIMIT-Sell");
        assert!(is_limit_order(&order));
    }

    #[test]
    fn test_is_limit_order_false_for_dca() {
        let order = order_with_deployment("auction-dca-base");
        assert!(!is_limit_order(&order));
    }

    #[test]
    fn test_is_limit_order_false_for_grid() {
        let order = order_with_deployment("grid-base");
        assert!(!is_limit_order(&order));
    }

    #[test]
    fn test_is_limit_order_false_for_no_metadata() {
        let order: RaindexOrder =
            serde_json::from_value(order_json()).expect("deserialize plain order");
        assert!(!is_limit_order(&order));
    }

    #[rocket::async_test]
    async fn test_cache_hit_and_miss() {
        let cache = limit_order_ratio_cache();
        let key = alloy::primitives::B256::from([1u8; 32]);
        assert!(cache.get(&key).await.is_none());
        cache.insert(key, "1.5".to_string()).await;
        assert_eq!(cache.get(&key).await.as_deref(), Some("1.5"));
    }
}
