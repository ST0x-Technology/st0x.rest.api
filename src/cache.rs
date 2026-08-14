use moka::future::Cache;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use rain_orderbook_common::raindex_client::order_quotes::RaindexOrderQuote;

use crate::routes::swap::SwapCandidateBuild;
use crate::types::orders::OrdersListResponse;
use crate::types::trades::{TradesByAddressResponse, TradesQueryResponse};

enum ConditionalCacheError<V, E> {
    NotCacheable(V),
    Fetch(E),
}

pub(crate) struct AppCache<K, V>(pub(crate) Cache<K, V>)
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static;

impl<K, V> AppCache<K, V>
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub(crate) fn new(max_capacity: u64, ttl: Duration) -> Self {
        Self(
            Cache::builder()
                .max_capacity(max_capacity)
                .time_to_live(ttl)
                .build(),
        )
    }

    pub(crate) fn new_weighted<F>(max_capacity: u64, ttl: Duration, weigher: F) -> Self
    where
        F: Fn(&K, &V) -> u32 + Send + Sync + 'static,
    {
        Self(
            Cache::builder()
                .max_capacity(max_capacity)
                .weigher(weigher)
                .time_to_live(ttl)
                .build(),
        )
    }

    pub(crate) async fn get(&self, key: &K) -> Option<V> {
        self.0.get(key).await
    }

    pub(crate) async fn insert(&self, key: K, value: V) {
        self.0.insert(key, value).await
    }

    pub(crate) async fn get_or_try_insert<F, Fut, E>(&self, key: K, fetch: F) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V, E>>,
        E: Send + Sync + 'static,
    {
        self.0.try_get_with(key, async move { fetch().await }).await
    }

    pub(crate) async fn get_or_try_insert_if<F, Fut, E, P>(
        &self,
        key: K,
        fetch: F,
        should_cache: P,
    ) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V, E>>,
        E: Clone + Send + Sync + 'static,
        P: FnOnce(&V) -> bool,
    {
        let result = self
            .0
            .try_get_with(key, async move {
                let value = fetch().await.map_err(ConditionalCacheError::Fetch)?;
                if should_cache(&value) {
                    Ok(value)
                } else {
                    Err(ConditionalCacheError::NotCacheable(value))
                }
            })
            .await;

        match result {
            Ok(value) => Ok(value),
            Err(error) => match error.as_ref() {
                ConditionalCacheError::NotCacheable(value) => Ok(value.clone()),
                ConditionalCacheError::Fetch(error) => Err(Arc::new(error.clone())),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn invalidate_all(&self) {
        self.0.invalidate_all()
    }
}

pub(crate) struct RouteResponseCaches {
    enabled: bool,
    trades_query_enabled: bool,
    pub order_quotes: AppCache<String, Vec<RaindexOrderQuote>>,
    pub orders_by_token: AppCache<String, OrdersListResponse>,
    pub orders_query: AppCache<String, OrdersListResponse>,
    pub swap_candidates: AppCache<String, SwapCandidateBuild>,
    pub trades_by_token: AppCache<String, TradesByAddressResponse>,
    pub trades_by_taker: AppCache<String, TradesByAddressResponse>,
    pub trades_query: AppCache<String, TradesQueryResponse>,
    group: CacheGroup,
}

fn trade_row_weight(counts: impl IntoIterator<Item = usize>) -> u32 {
    counts
        .into_iter()
        .try_fold(0_u32, |total, count| {
            u32::try_from(count)
                .ok()
                .and_then(|count| total.checked_add(count))
        })
        .unwrap_or(u32::MAX)
        .max(1)
}

fn trades_query_weight(response: &TradesQueryResponse) -> u32 {
    match response {
        TradesQueryResponse::ByOrderHashes(grouped) => trade_row_weight(
            grouped
                .trades_by_order_hash
                .iter()
                .map(|entry| entry.trades.len()),
        ),
        TradesQueryResponse::ByTokens(page) => trade_row_weight([page.trades.len()]),
    }
}

impl RouteResponseCaches {
    pub(crate) fn new(max_capacity: u64, ttl: Duration) -> Self {
        Self::new_with_trade_weight(max_capacity, max_capacity, ttl)
    }

    pub(crate) fn new_with_trade_weight(
        max_capacity: u64,
        max_trade_weight: u64,
        ttl: Duration,
    ) -> Self {
        let enabled = max_capacity > 0 && !ttl.is_zero();
        let trades_query_enabled = enabled && max_trade_weight > 0;
        let max_capacity = max_capacity.max(1);
        let max_trade_weight = max_trade_weight.max(1);
        let ttl = if ttl.is_zero() {
            Duration::from_nanos(1)
        } else {
            ttl
        };
        let order_quotes = AppCache::new(max_capacity, ttl);
        let orders_by_token = AppCache::new(max_capacity, ttl);
        let orders_query = AppCache::new(max_capacity, ttl);
        let swap_candidates = AppCache::new(max_capacity, ttl);
        let trades_by_token = AppCache::new(max_capacity, ttl);
        let trades_by_taker = AppCache::new(max_capacity, ttl);
        let trades_query = AppCache::new_weighted(max_trade_weight, ttl, |_, response| {
            trades_query_weight(response)
        });

        let mut group = CacheGroup::new();
        group.register(&order_quotes);
        group.register(&orders_by_token);
        group.register(&orders_query);
        group.register(&swap_candidates);
        group.register(&trades_by_token);
        group.register(&trades_by_taker);
        group.register(&trades_query);

        Self {
            enabled,
            trades_query_enabled,
            order_quotes,
            orders_by_token,
            orders_query,
            swap_candidates,
            trades_by_token,
            trades_by_taker,
            trades_query,
            group,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn trades_query_is_enabled(&self) -> bool {
        self.trades_query_enabled
    }

    pub(crate) fn invalidate_all(&self) {
        self.group.invalidate_all();
    }
}

trait Invalidatable: Send + Sync {
    fn invalidate_all(&self);
}

impl<K, V> Invalidatable for Cache<K, V>
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn invalidate_all(&self) {
        Cache::invalidate_all(self)
    }
}

pub(crate) struct CacheGroup {
    caches: Vec<Arc<dyn Invalidatable>>,
}

impl CacheGroup {
    pub(crate) fn new() -> Self {
        Self { caches: Vec::new() }
    }

    pub(crate) fn register<K, V>(&mut self, cache: &AppCache<K, V>)
    where
        K: std::hash::Hash + Eq + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        self.caches.push(Arc::new(cache.0.clone()));
    }

    pub(crate) fn invalidate_all(&self) {
        for cache in &self.caches {
            cache.invalidate_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::common::TokenRef;
    use crate::types::trades::{
        TradeByAddress, TradesByOrderHashEntry, TradesByOrderHashesResponse, TradesPagination,
    };
    use alloy::primitives::{Address, B256};

    fn mock_trade() -> TradeByAddress {
        TradeByAddress {
            tx_hash: B256::ZERO,
            input_amount: "1".into(),
            output_amount: "1".into(),
            input_token: TokenRef {
                address: Address::ZERO,
                symbol: "IN".into(),
                decimals: 18,
            },
            output_token: TokenRef {
                address: Address::ZERO,
                symbol: "OUT".into(),
                decimals: 18,
            },
            order_hash: Some(B256::ZERO),
            timestamp: 1,
            block_number: 1,
        }
    }

    #[rocket::async_test]
    async fn test_app_cache_insert_and_get() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        cache.insert("key", 42).await;
        assert_eq!(cache.get(&"key").await, Some(42));
    }

    #[rocket::async_test]
    async fn test_app_cache_get_returns_none_for_missing_key() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        assert!(cache.get(&"missing").await.is_none());
    }

    #[rocket::async_test]
    async fn test_weighted_cache_rejects_entry_over_capacity() {
        let cache: AppCache<&str, u32> =
            AppCache::new_weighted(3, Duration::from_secs(60), |_, weight| *weight);
        cache.insert("heavy", 4).await;
        cache.0.run_pending_tasks().await;
        assert!(cache.get(&"heavy").await.is_none());
    }

    #[test]
    fn trades_query_weight_counts_token_mode_rows() {
        let response = TradesQueryResponse::ByTokens(TradesByAddressResponse {
            trades: vec![mock_trade(), mock_trade(), mock_trade()],
            pagination: TradesPagination {
                page: 1,
                page_size: 3,
                total_trades: 3,
                total_pages: 1,
                has_more: false,
            },
        });

        assert_eq!(trades_query_weight(&response), 3);
    }

    #[test]
    fn trades_query_weight_sums_grouped_rows() {
        let response = TradesQueryResponse::ByOrderHashes(TradesByOrderHashesResponse {
            trades_by_order_hash: vec![
                TradesByOrderHashEntry {
                    order_hash: B256::ZERO,
                    trades: vec![mock_trade(), mock_trade()],
                },
                TradesByOrderHashEntry {
                    order_hash: B256::with_last_byte(1),
                    trades: vec![mock_trade()],
                },
            ],
            total_count: 3,
        });

        assert_eq!(trades_query_weight(&response), 3);
    }

    #[test]
    fn trade_row_weight_saturates_on_overflow() {
        assert_eq!(trade_row_weight([u32::MAX as usize, 1]), u32::MAX);
    }

    #[test]
    fn zero_trade_row_budget_disables_only_trades_query_cache() {
        let caches = RouteResponseCaches::new_with_trade_weight(100, 0, Duration::from_secs(5));

        assert!(caches.is_enabled());
        assert!(!caches.trades_query_is_enabled());
    }

    #[rocket::async_test]
    async fn test_app_cache_invalidate_all_clears_entries() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        cache.insert("a", 1).await;
        cache.insert("b", 2).await;
        cache.invalidate_all();
        tokio::task::yield_now().await;
        assert!(cache.get(&"a").await.is_none());
        assert!(cache.get(&"b").await.is_none());
    }

    #[rocket::async_test]
    async fn test_get_or_try_insert_calls_fetch_on_miss() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        let result: Result<u32, Arc<String>> =
            cache.get_or_try_insert("key", || async { Ok(42) }).await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(cache.get(&"key").await, Some(42));
    }

    #[rocket::async_test]
    async fn test_get_or_try_insert_returns_cached_on_hit() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        cache.insert("key", 42).await;
        let result: Result<u32, Arc<String>> = cache
            .get_or_try_insert("key", || async { panic!("fetch should not be called") })
            .await;
        assert_eq!(result.unwrap(), 42);
    }

    #[rocket::async_test]
    async fn test_get_or_try_insert_does_not_cache_errors() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        let result: Result<u32, Arc<String>> = cache
            .get_or_try_insert("key", || async { Err("fail".to_string()) })
            .await;
        assert!(result.is_err());
        assert!(cache.get(&"key").await.is_none());
    }

    #[rocket::async_test]
    async fn test_get_or_try_insert_if_skips_rejected_values() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));

        let first: Result<u32, Arc<String>> = cache
            .get_or_try_insert_if("key", || async { Ok(1) }, |value| value.is_multiple_of(2))
            .await;
        assert_eq!(first.unwrap(), 1);
        assert!(cache.get(&"key").await.is_none());

        let second: Result<u32, Arc<String>> = cache
            .get_or_try_insert_if("key", || async { Ok(2) }, |value| value.is_multiple_of(2))
            .await;
        assert_eq!(second.unwrap(), 2);
        assert_eq!(cache.get(&"key").await, Some(2));
    }

    #[rocket::async_test]
    async fn test_get_or_try_insert_coalesces_concurrent_misses() {
        let cache: Arc<AppCache<String, u32>> =
            Arc::new(AppCache::new(10, Duration::from_secs(60)));
        let fetch_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut tasks = tokio::task::JoinSet::new();

        for _ in 0..10 {
            let cache = cache.clone();
            let fetch_count = fetch_count.clone();
            tasks.spawn(async move {
                cache
                    .get_or_try_insert("key".to_string(), || async move {
                        fetch_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        Ok::<_, String>(42)
                    })
                    .await
            });
        }

        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap().unwrap(), 42);
        }

        assert_eq!(fetch_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(cache.get(&"key".to_string()).await, Some(42));
    }

    #[rocket::async_test]
    async fn test_cache_group_invalidate_all_clears_registered_caches() {
        let cache_a: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        let cache_b: AppCache<u32, String> = AppCache::new(10, Duration::from_secs(60));
        cache_a.insert("x", 10).await;
        cache_b.insert(1, "hello".into()).await;

        let mut group = CacheGroup::new();
        group.register(&cache_a);
        group.register(&cache_b);
        group.invalidate_all();

        tokio::task::yield_now().await;
        assert!(cache_a.get(&"x").await.is_none());
        assert!(cache_b.get(&1).await.is_none());
    }
}
