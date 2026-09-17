use crate::error::{ApiError, ApiErrorCode};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) struct SwapCapacity {
    global: Arc<Semaphore>,
    global_limit: usize,
    per_key_in_flight: Arc<Mutex<HashMap<i64, usize>>>,
    default_per_key_limit: usize,
    timeout: Duration,
}

#[derive(Debug)]
struct SwapPermit {
    _global: OwnedSemaphorePermit,
    _per_key: PerKeyPermit,
}

#[derive(Debug)]
struct PerKeyPermit {
    in_flight: Arc<Mutex<HashMap<i64, usize>>>,
    key_id: i64,
}

impl Drop for PerKeyPermit {
    fn drop(&mut self) {
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = in_flight.get_mut(&self.key_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&self.key_id);
            }
        }
    }
}

impl SwapCapacity {
    pub(crate) fn new(global_limit: usize, per_key_limit: usize, timeout: Duration) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global_limit)),
            global_limit,
            per_key_in_flight: Arc::new(Mutex::new(HashMap::new())),
            default_per_key_limit: per_key_limit,
            timeout,
        }
    }

    fn try_acquire_per_key(
        &self,
        key_id: i64,
        per_key_limit_override: Option<usize>,
    ) -> Result<PerKeyPermit, ApiError> {
        let limit = per_key_limit_override
            .unwrap_or(self.default_per_key_limit)
            .min(self.global_limit);
        let mut in_flight = self
            .per_key_in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let rejected_in_flight = {
            let count = in_flight.entry(key_id).or_default();
            if *count >= limit {
                Some(*count)
            } else {
                *count += 1;
                None
            }
        };
        drop(in_flight);

        if let Some(in_flight) = rejected_in_flight {
            crate::metrics::record_per_key_swap_capacity_rejection();
            tracing::warn!(
                key_id,
                in_flight,
                limit,
                "per-key concurrent swap limit exceeded"
            );
            return Err(ApiError::RateLimited(
                "too many concurrent swap requests".into(),
            ));
        }
        Ok(PerKeyPermit {
            in_flight: self.per_key_in_flight.clone(),
            key_id,
        })
    }

    fn try_acquire(
        &self,
        key_id: i64,
        per_key_limit_override: Option<usize>,
    ) -> Result<SwapPermit, ApiError> {
        let per_key = self.try_acquire_per_key(key_id, per_key_limit_override)?;

        let global = self.global.clone().try_acquire_owned().map_err(|_| {
            crate::metrics::record_global_swap_capacity_rejection();
            tracing::warn!(
                key_id,
                limit = self.global_limit,
                "global concurrent swap limit exceeded"
            );
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
        per_key_limit_override: Option<usize>,
        operation: impl Future<Output = Result<T, ApiError>>,
    ) -> Result<T, ApiError> {
        let _permit = self.try_acquire(key_id, per_key_limit_override)?;
        match tokio::time::timeout(self.timeout, operation).await {
            Ok(result) => result,
            Err(_) => {
                crate::metrics::record_swap_timeout();
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
        let held = capacity.try_acquire(7, None).expect("first permit");

        let error = capacity
            .try_acquire(7, None)
            .expect_err("per-key rejection");
        assert_eq!(error.code(), ApiErrorCode::RateLimited);

        drop(held);
        capacity.try_acquire(7, None).expect("released permit");
    }

    #[tokio::test]
    async fn per_key_override_does_not_change_other_keys() {
        let capacity = SwapCapacity::new(12, 4, Duration::from_secs(1));
        let overridden: Vec<_> = (0..8)
            .map(|_| capacity.try_acquire(7, Some(8)).expect("override permit"))
            .collect();
        let defaults: Vec<_> = (0..4)
            .map(|_| capacity.try_acquire(8, None).expect("default permit"))
            .collect();

        let override_error = capacity
            .try_acquire(7, Some(8))
            .expect_err("override limit");
        let default_error = capacity.try_acquire(8, None).expect_err("default limit");
        assert_eq!(override_error.code(), ApiErrorCode::RateLimited);
        assert_eq!(default_error.code(), ApiErrorCode::RateLimited);

        drop((overridden, defaults));
    }

    #[tokio::test]
    async fn override_changes_apply_while_requests_are_in_flight() {
        let capacity = SwapCapacity::new(12, 4, Duration::from_secs(1));
        let mut permits: Vec<_> = (0..4)
            .map(|_| capacity.try_acquire(7, None).expect("default permit"))
            .collect();
        permits.extend((0..4).map(|_| capacity.try_acquire(7, Some(8)).expect("override permit")));

        let reduced_limit_error = capacity
            .try_acquire(7, None)
            .expect_err("reduced limit applies immediately");
        assert_eq!(reduced_limit_error.code(), ApiErrorCode::RateLimited);

        drop(permits.drain(..5));
        capacity
            .try_acquire(7, None)
            .expect("default permit after in-flight count falls below four");
    }

    #[tokio::test]
    async fn rejects_work_when_global_limit_is_full() {
        let capacity = SwapCapacity::new(1, 1, Duration::from_secs(1));
        let _held = capacity.try_acquire(7, None).expect("first permit");

        let error = capacity.try_acquire(8, None).expect_err("global rejection");
        assert_eq!(error.code(), ApiErrorCode::RateLimited);
    }

    #[tokio::test]
    async fn override_is_bounded_by_global_limit() {
        let capacity = SwapCapacity::new(2, 1, Duration::from_secs(1));
        let _first = capacity.try_acquire(7, Some(8)).expect("first permit");
        let _second = capacity.try_acquire(7, Some(8)).expect("second permit");

        let error = capacity
            .try_acquire(7, Some(8))
            .expect_err("bounded override");
        assert_eq!(error.code(), ApiErrorCode::RateLimited);
    }

    #[tokio::test]
    async fn timeout_releases_capacity() {
        let capacity = SwapCapacity::new(1, 1, Duration::from_millis(1));
        let result: Result<Infallible, ApiError> = capacity
            .run(7, None, async {
                std::future::pending::<Result<Infallible, ApiError>>().await
            })
            .await;

        let error = result.expect_err("timeout");
        assert_eq!(error.code(), ApiErrorCode::SwapTimeout);
        capacity
            .try_acquire(7, None)
            .expect("permit released after timeout");
    }
}
