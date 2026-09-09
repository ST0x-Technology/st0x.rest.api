use crate::error::{ApiError, ApiErrorCode};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct SwapCapacity {
    global: Arc<Semaphore>,
    per_key: Mutex<HashMap<i64, Weak<Semaphore>>>,
    per_key_limit: usize,
    timeout: Duration,
}

#[derive(Debug)]
struct SwapPermit {
    _global: OwnedSemaphorePermit,
    _per_key: OwnedSemaphorePermit,
}

impl SwapCapacity {
    pub(crate) fn new(global_limit: usize, per_key_limit: usize, timeout: Duration) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global_limit)),
            per_key: Mutex::new(HashMap::new()),
            per_key_limit,
            timeout,
        }
    }

    fn key_semaphore(&self, key_id: i64) -> Arc<Semaphore> {
        let mut semaphores = self
            .per_key
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        semaphores.retain(|_, semaphore| semaphore.strong_count() > 0);

        semaphores
            .get(&key_id)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let semaphore = Arc::new(Semaphore::new(self.per_key_limit));
                semaphores.insert(key_id, Arc::downgrade(&semaphore));
                semaphore
            })
    }

    fn try_acquire(&self, key_id: i64) -> Result<SwapPermit, ApiError> {
        let per_key = self
            .key_semaphore(key_id)
            .try_acquire_owned()
            .map_err(|_| {
                tracing::warn!(key_id, "per-key concurrent swap limit exceeded");
                ApiError::RateLimited("too many concurrent swap requests".into())
            })?;

        let global = self.global.clone().try_acquire_owned().map_err(|_| {
            tracing::warn!(key_id, "global concurrent swap limit exceeded");
            ApiError::RateLimited("swap service is at capacity".into())
        })?;

        Ok(SwapPermit {
            _global: global,
            _per_key: per_key,
        })
    }

    pub(crate) async fn run<T>(
        &self,
        key_id: i64,
        operation: impl Future<Output = Result<T, ApiError>>,
    ) -> Result<T, ApiError> {
        let _permit = self.try_acquire(key_id)?;
        match tokio::time::timeout(self.timeout, operation).await {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    key_id,
                    timeout_ms = self.timeout.as_millis(),
                    "swap request timed out"
                );
                Err(ApiError::coded(
                    ApiErrorCode::SwapTimeout,
                    "the swap request timed out",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    #[tokio::test]
    async fn rejects_work_when_per_key_limit_is_full() {
        let capacity = SwapCapacity::new(2, 1, Duration::from_secs(1));
        let held = capacity.try_acquire(7).expect("first permit");

        let error = capacity.try_acquire(7).expect_err("per-key rejection");
        assert_eq!(error.code(), ApiErrorCode::RateLimited);

        drop(held);
        capacity.try_acquire(7).expect("released permit");
    }

    #[tokio::test]
    async fn rejects_work_when_global_limit_is_full() {
        let capacity = SwapCapacity::new(1, 1, Duration::from_secs(1));
        let _held = capacity.try_acquire(7).expect("first permit");

        let error = capacity.try_acquire(8).expect_err("global rejection");
        assert_eq!(error.code(), ApiErrorCode::RateLimited);
    }

    #[tokio::test]
    async fn timeout_releases_capacity() {
        let capacity = SwapCapacity::new(1, 1, Duration::from_millis(1));
        let result: Result<Infallible, ApiError> = capacity
            .run(7, async {
                std::future::pending::<Result<Infallible, ApiError>>().await
            })
            .await;

        let error = result.expect_err("timeout");
        assert_eq!(error.code(), ApiErrorCode::SwapTimeout);
        capacity
            .try_acquire(7)
            .expect("permit released after timeout");
    }
}
