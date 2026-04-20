use moka::future::Cache;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

pub(crate) struct AppCache<K, V>(Cache<K, V>)
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
        self.0.try_get_with(key, fetch()).await
    }

    pub(crate) fn invalidate_all(&self) {
        self.0.invalidate_all();
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
        Cache::invalidate_all(self);
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

    #[rocket::async_test]
    async fn test_app_cache_insert_and_get() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        cache.insert("key", 42).await;
        assert_eq!(cache.get(&"key").await, Some(42));
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
    async fn test_get_or_try_insert_uses_single_flight() {
        let cache: AppCache<&str, u32> = AppCache::new(10, Duration::from_secs(60));
        let result: Result<u32, Arc<String>> =
            cache.get_or_try_insert("key", || async { Ok(42) }).await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(cache.get(&"key").await, Some(42));
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
