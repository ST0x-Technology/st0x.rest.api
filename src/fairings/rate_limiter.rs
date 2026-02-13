use crate::error::ApiError;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::{Header, Status};
use rocket::request::{FromRequest, Outcome};
use rocket::{Request, Response};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WINDOW_DURATION: Duration = Duration::from_secs(60);
const PER_KEY_CLEANUP_EVERY: u64 = 1024;

pub struct GlobalRateLimit;

pub struct RateLimitInfo {
    pub limit: u64,
    pub remaining: u64,
    pub reset: u64,
}

pub struct CachedRateLimitInfo(pub Mutex<Option<RateLimitInfo>>);

pub struct RateLimitHeadersFairing;

pub struct RateLimiter {
    global_rpm: u64,
    per_key_rpm: u64,
    global_window: Mutex<VecDeque<Instant>>,
    per_key_windows: Mutex<HashMap<i64, VecDeque<Instant>>>,
    per_key_check_count: AtomicU64,
}

impl RateLimiter {
    pub fn new(global_rpm: u64, per_key_rpm: u64) -> Self {
        Self {
            global_rpm,
            per_key_rpm,
            global_window: Mutex::new(VecDeque::new()),
            per_key_windows: Mutex::new(HashMap::new()),
            per_key_check_count: AtomicU64::new(0),
        }
    }

    fn prune_window(window: &mut VecDeque<Instant>, cutoff: Instant) {
        while window.front().is_some_and(|t| *t < cutoff) {
            window.pop_front();
        }
    }

    fn compute_reset(window: &VecDeque<Instant>, now: Instant) -> u64 {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match window.front() {
            Some(&oldest) => {
                let delta = (oldest + WINDOW_DURATION)
                    .saturating_duration_since(now)
                    .as_secs();
                now_unix + delta
            }
            None => now_unix + WINDOW_DURATION.as_secs(),
        }
    }

    pub fn check_global(&self) -> Result<(bool, Option<RateLimitInfo>), ApiError> {
        if self.global_rpm == 0 {
            return Ok((true, None));
        }
        let mut window = match self.global_window.lock() {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "global rate limiter lock poisoned");
                return Err(ApiError::Internal("rate limiter unavailable".into()));
            }
        };
        let now = Instant::now();
        let cutoff = now - WINDOW_DURATION;
        Self::prune_window(&mut window, cutoff);
        if (window.len() as u64) < self.global_rpm {
            window.push_back(now);
            let remaining = self.global_rpm - window.len() as u64;
            let reset = Self::compute_reset(&window, now);
            Ok((
                true,
                Some(RateLimitInfo {
                    limit: self.global_rpm,
                    remaining,
                    reset,
                }),
            ))
        } else {
            let reset = Self::compute_reset(&window, now);
            Ok((
                false,
                Some(RateLimitInfo {
                    limit: self.global_rpm,
                    remaining: 0,
                    reset,
                }),
            ))
        }
    }

    pub fn check_per_key(&self, key_id: i64) -> Result<(bool, Option<RateLimitInfo>), ApiError> {
        if self.per_key_rpm == 0 {
            return Ok((true, None));
        }
        let mut windows = match self.per_key_windows.lock() {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "per-key rate limiter lock poisoned");
                return Err(ApiError::Internal("rate limiter unavailable".into()));
            }
        };

        let now = Instant::now();
        let cutoff = now - WINDOW_DURATION;
        let check_count = self.per_key_check_count.fetch_add(1, Ordering::Relaxed) + 1;

        if check_count % PER_KEY_CLEANUP_EVERY == 0 {
            windows.retain(|_, window| {
                Self::prune_window(window, cutoff);
                !window.is_empty()
            });
        }

        let window = windows.entry(key_id).or_default();
        Self::prune_window(window, cutoff);

        if (window.len() as u64) < self.per_key_rpm {
            window.push_back(now);
            let remaining = self.per_key_rpm - window.len() as u64;
            let reset = Self::compute_reset(window, now);
            Ok((
                true,
                Some(RateLimitInfo {
                    limit: self.per_key_rpm,
                    remaining,
                    reset,
                }),
            ))
        } else {
            let reset = Self::compute_reset(window, now);
            Ok((
                false,
                Some(RateLimitInfo {
                    limit: self.per_key_rpm,
                    remaining: 0,
                    reset,
                }),
            ))
        }
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for GlobalRateLimit {
    type Error = ApiError;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let rl = match req.rocket().state::<RateLimiter>() {
            Some(rl) => rl,
            None => {
                tracing::error!("RateLimiter not found in managed state");
                return Outcome::Error((
                    Status::InternalServerError,
                    ApiError::Internal("rate limiter unavailable".into()),
                ));
            }
        };

        match rl.check_global() {
            Ok((true, info)) => {
                if let Some(info) = info {
                    let cache = req.local_cache(|| CachedRateLimitInfo(Mutex::new(None)));
                    if let Ok(mut guard) = cache.0.lock() {
                        *guard = Some(info);
                    }
                }
                Outcome::Success(GlobalRateLimit)
            }
            Ok((false, info)) => {
                if let Some(info) = info {
                    let cache = req.local_cache(|| CachedRateLimitInfo(Mutex::new(None)));
                    if let Ok(mut guard) = cache.0.lock() {
                        *guard = Some(info);
                    }
                }
                tracing::warn!("global rate limit exceeded");
                Outcome::Error((
                    Status::TooManyRequests,
                    ApiError::RateLimited("Too many requests, please try again later".into()),
                ))
            }
            Err(e) => {
                tracing::error!(error = %e, "global rate limiter failed");
                Outcome::Error((Status::InternalServerError, e))
            }
        }
    }
}

#[rocket::async_trait]
impl Fairing for RateLimitHeadersFairing {
    fn info(&self) -> Info {
        Info {
            name: "Rate Limit Headers",
            kind: Kind::Response,
        }
    }

    async fn on_response<'r>(&self, req: &'r Request<'_>, res: &mut Response<'r>) {
        let cache = req.local_cache(|| CachedRateLimitInfo(Mutex::new(None)));
        if let Ok(guard) = cache.0.lock() {
            if let Some(ref info) = *guard {
                res.set_header(Header::new("X-RateLimit-Limit", info.limit.to_string()));
                res.set_header(Header::new(
                    "X-RateLimit-Remaining",
                    info.remaining.to_string(),
                ));
                res.set_header(Header::new("X-RateLimit-Reset", info.reset.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{basic_auth_header, client, seed_api_key};
    use rocket::http::{Header as HttpHeader, Status};
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn test_global_check_allows_under_limit() {
        let rl = RateLimiter::new(5, 5);
        for _ in 0..5 {
            assert!(matches!(rl.check_global(), Ok((true, _))));
        }
    }

    #[test]
    fn test_global_check_blocks_over_limit() {
        let rl = RateLimiter::new(3, 5);
        for _ in 0..3 {
            assert!(matches!(rl.check_global(), Ok((true, _))));
        }
        assert!(matches!(rl.check_global(), Ok((false, _))));
    }

    #[test]
    fn test_global_check_blocks_over_limit_with_concurrency() {
        let rl = Arc::new(RateLimiter::new(10, 1000));
        let workers = 64;
        let barrier = Arc::new(Barrier::new(workers));
        let mut handles = Vec::with_capacity(workers);

        for _ in 0..workers {
            let rl = Arc::clone(&rl);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                matches!(rl.check_global(), Ok((true, _)))
            }));
        }

        let mut allowed = 0usize;
        for handle in handles {
            if handle.join().expect("thread join") {
                allowed += 1;
            }
        }

        assert!(allowed <= 10);
    }

    #[test]
    fn test_per_key_check_allows_under_limit() {
        let rl = RateLimiter::new(100, 3);
        for _ in 0..3 {
            assert!(matches!(rl.check_per_key(1), Ok((true, _))));
        }
    }

    #[test]
    fn test_per_key_check_blocks_over_limit() {
        let rl = RateLimiter::new(100, 2);
        assert!(matches!(rl.check_per_key(1), Ok((true, _))));
        assert!(matches!(rl.check_per_key(1), Ok((true, _))));
        assert!(matches!(rl.check_per_key(1), Ok((false, _))));
    }

    #[test]
    fn test_per_key_check_blocks_over_limit_with_concurrency() {
        let rl = Arc::new(RateLimiter::new(1000, 7));
        let workers = 48;
        let barrier = Arc::new(Barrier::new(workers));
        let mut handles = Vec::with_capacity(workers);

        for _ in 0..workers {
            let rl = Arc::clone(&rl);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                matches!(rl.check_per_key(42), Ok((true, _)))
            }));
        }

        let mut allowed = 0usize;
        for handle in handles {
            if handle.join().expect("thread join") {
                allowed += 1;
            }
        }

        assert!(allowed <= 7);
    }

    #[test]
    fn test_per_key_limits_are_independent() {
        let rl = RateLimiter::new(100, 1);
        assert!(matches!(rl.check_per_key(1), Ok((true, _))));
        assert!(matches!(rl.check_per_key(1), Ok((false, _))));
        assert!(matches!(rl.check_per_key(2), Ok((true, _))));
        assert!(matches!(rl.check_per_key(2), Ok((false, _))));
    }

    #[test]
    fn test_per_key_limits_are_independent_with_concurrency() {
        let rl = Arc::new(RateLimiter::new(1000, 5));
        let workers_per_key = 20;
        let workers = workers_per_key * 2;
        let barrier = Arc::new(Barrier::new(workers));
        let mut handles = Vec::with_capacity(workers);

        for _ in 0..workers_per_key {
            let rl = Arc::clone(&rl);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                (1_i64, matches!(rl.check_per_key(1), Ok((true, _))))
            }));
        }
        for _ in 0..workers_per_key {
            let rl = Arc::clone(&rl);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                (2_i64, matches!(rl.check_per_key(2), Ok((true, _))))
            }));
        }

        let mut allowed_key_1 = 0usize;
        let mut allowed_key_2 = 0usize;
        for handle in handles {
            let (key, allowed) = handle.join().expect("thread join");
            if allowed {
                if key == 1 {
                    allowed_key_1 += 1;
                } else {
                    allowed_key_2 += 1;
                }
            }
        }

        assert_eq!(allowed_key_1, 5);
        assert_eq!(allowed_key_2, 5);
    }

    #[test]
    fn test_zero_rps_disables_limiting() {
        let rl = RateLimiter::new(0, 0);
        for _ in 0..1000 {
            assert!(matches!(rl.check_global(), Ok((true, _))));
            assert!(matches!(rl.check_per_key(1), Ok((true, _))));
        }
    }

    #[test]
    fn test_window_slides_after_expiry() {
        let rl = RateLimiter::new(2, 2);
        let stale = Instant::now() - Duration::from_secs(61);
        {
            let mut window = rl.global_window.lock().expect("lock");
            window.push_back(stale);
            window.push_back(Instant::now());
        }
        assert!(matches!(rl.check_global(), Ok((true, _))));
    }

    #[test]
    fn test_per_key_window_slides_after_expiry() {
        let rl = RateLimiter::new(100, 2);
        let stale = Instant::now() - Duration::from_secs(61);
        {
            let mut windows = rl.per_key_windows.lock().expect("lock");
            let window = windows.entry(7).or_default();
            window.push_back(stale);
            window.push_back(Instant::now());
        }
        assert!(matches!(rl.check_per_key(7), Ok((true, _))));
    }

    #[test]
    fn test_poisoned_global_lock_returns_error() {
        let rl = RateLimiter::new(2, 2);
        let _ = std::panic::catch_unwind(|| {
            let _guard = rl.global_window.lock().expect("lock");
            panic!("poison global lock");
        });

        assert!(matches!(rl.check_global(), Err(ApiError::Internal(_))));
    }

    #[test]
    fn test_poisoned_per_key_lock_returns_error() {
        let rl = RateLimiter::new(2, 2);
        let _ = std::panic::catch_unwind(|| {
            let _guard = rl.per_key_windows.lock().expect("lock");
            panic!("poison per-key lock");
        });

        assert!(matches!(rl.check_per_key(1), Err(ApiError::Internal(_))));
    }

    #[test]
    fn test_per_key_cleanup_removes_stale_entries() {
        let rl = RateLimiter::new(100, 1);
        let stale = Instant::now() - Duration::from_secs(61);

        {
            let mut windows = rl.per_key_windows.lock().expect("lock");
            for key in 1..=5 {
                windows.insert(key, VecDeque::from([stale]));
            }
        }

        for _ in 0..PER_KEY_CLEANUP_EVERY {
            assert!(rl.check_per_key(999).is_ok());
        }

        let windows = rl.per_key_windows.lock().expect("lock");
        assert_eq!(windows.len(), 1);
        assert!(windows.contains_key(&999));
    }

    #[rocket::async_test]
    async fn test_global_rate_limit_returns_429() {
        let rl = RateLimiter::new(2, 10000);
        let id = uuid::Uuid::new_v4();
        let pool = crate::db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("database init");
        let rocket = crate::rocket(pool, rl).expect("valid rocket instance");
        let client = rocket::local::asynchronous::Client::tracked(rocket)
            .await
            .expect("valid client");

        let r1 = client.get("/v1/tokens").dispatch().await;
        assert_eq!(r1.status(), Status::Unauthorized);

        let r2 = client.get("/v1/tokens").dispatch().await;
        assert_eq!(r2.status(), Status::Unauthorized);

        let r3 = client.get("/v1/tokens").dispatch().await;
        assert_eq!(r3.status(), Status::TooManyRequests);

        let retry_after = r3
            .headers()
            .get_one("Retry-After")
            .expect("Retry-After header");
        assert_eq!(retry_after, "60");

        let limit = r3
            .headers()
            .get_one("X-RateLimit-Limit")
            .expect("X-RateLimit-Limit header");
        assert_eq!(limit, "2");

        let remaining = r3
            .headers()
            .get_one("X-RateLimit-Remaining")
            .expect("X-RateLimit-Remaining header");
        assert_eq!(remaining, "0");

        assert!(r3.headers().get_one("X-RateLimit-Reset").is_some());

        let body = r3.into_string().await.expect("response body");
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(json["error"]["code"], "RATE_LIMITED");
        assert_eq!(
            json["error"]["message"],
            "Too many requests, please try again later"
        );
    }

    #[rocket::async_test]
    async fn test_per_key_rate_limit_returns_429() {
        let client = client().await;
        let (key_id, secret) = seed_api_key(&client).await;

        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool");
        let rl = client.rocket().state::<RateLimiter>().expect("rate limiter");

        let header_val = basic_auth_header(&key_id, &secret);

        let api_key: (i64,) = sqlx::query_as("SELECT id FROM api_keys WHERE key_id = ?")
            .bind(&key_id)
            .fetch_one(pool)
            .await
            .expect("query");

        {
            let mut windows = rl.per_key_windows.lock().expect("lock");
            let window = windows.entry(api_key.0).or_default();
            for _ in 0..10000 {
                window.push_back(Instant::now());
            }
        }

        let response = client
            .get("/v1/tokens")
            .header(HttpHeader::new("Authorization", header_val))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::TooManyRequests);

        let retry_after = response
            .headers()
            .get_one("Retry-After")
            .expect("Retry-After header");
        assert_eq!(retry_after, "60");

        let limit = response
            .headers()
            .get_one("X-RateLimit-Limit")
            .expect("X-RateLimit-Limit header");
        assert_eq!(limit, "10000");

        let remaining = response
            .headers()
            .get_one("X-RateLimit-Remaining")
            .expect("X-RateLimit-Remaining header");
        assert_eq!(remaining, "0");

        assert!(response.headers().get_one("X-RateLimit-Reset").is_some());

        let body = response.into_string().await.expect("response body");
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(json["error"]["code"], "RATE_LIMITED");
        assert_eq!(
            json["error"]["message"],
            "Too many requests, please try again later"
        );
    }

    #[rocket::async_test]
    async fn test_different_keys_have_separate_per_key_limits() {
        let client = client().await;
        let (key_id_a, secret_a) = seed_api_key(&client).await;
        let (key_id_b, secret_b) = seed_api_key(&client).await;

        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool");
        let rl = client.rocket().state::<RateLimiter>().expect("rate limiter");

        let api_key_a: (i64,) = sqlx::query_as("SELECT id FROM api_keys WHERE key_id = ?")
            .bind(&key_id_a)
            .fetch_one(pool)
            .await
            .expect("query");

        {
            let mut windows = rl.per_key_windows.lock().expect("lock");
            let window = windows.entry(api_key_a.0).or_default();
            for _ in 0..10000 {
                window.push_back(Instant::now());
            }
        }

        let header_a = basic_auth_header(&key_id_a, &secret_a);
        let response_a = client
            .get("/v1/tokens")
            .header(HttpHeader::new("Authorization", header_a))
            .dispatch()
            .await;
        assert_eq!(response_a.status(), Status::TooManyRequests);

        let header_b = basic_auth_header(&key_id_b, &secret_b);
        let response_b = client
            .get("/v1/tokens")
            .header(HttpHeader::new("Authorization", header_b))
            .dispatch()
            .await;
        assert_ne!(response_b.status(), Status::TooManyRequests);
    }

    #[rocket::async_test]
    async fn test_per_key_limit_is_hit_before_global_when_global_is_high() {
        let rl = RateLimiter::new(10000, 1);
        let id = uuid::Uuid::new_v4();
        let pool = crate::db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("database init");
        let rocket = crate::rocket(pool, rl).expect("valid rocket instance");
        let client = rocket::local::asynchronous::Client::tracked(rocket)
            .await
            .expect("valid client");

        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let first = client
            .get("/v1/tokens")
            .header(HttpHeader::new("Authorization", header.clone()))
            .dispatch()
            .await;
        assert_ne!(first.status(), Status::TooManyRequests);

        let first_limit = first
            .headers()
            .get_one("X-RateLimit-Limit")
            .expect("X-RateLimit-Limit header");
        assert_eq!(first_limit, "1");

        let first_remaining = first
            .headers()
            .get_one("X-RateLimit-Remaining")
            .expect("X-RateLimit-Remaining header");
        assert_eq!(first_remaining, "0");

        let second = client
            .get("/v1/tokens")
            .header(HttpHeader::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(second.status(), Status::TooManyRequests);

        let retry_after = second
            .headers()
            .get_one("Retry-After")
            .expect("Retry-After header");
        assert_eq!(retry_after, "60");

        let limit = second
            .headers()
            .get_one("X-RateLimit-Limit")
            .expect("X-RateLimit-Limit header");
        assert_eq!(limit, "1");

        let remaining = second
            .headers()
            .get_one("X-RateLimit-Remaining")
            .expect("X-RateLimit-Remaining header");
        assert_eq!(remaining, "0");

        let body = second.into_string().await.expect("response body");
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(json["error"]["code"], "RATE_LIMITED");
        assert_eq!(
            json["error"]["message"],
            "Too many requests, please try again later"
        );
    }

    #[rocket::async_test]
    async fn test_rate_limit_headers_on_successful_request() {
        let client = client().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header_val = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/v1/tokens")
            .header(HttpHeader::new("Authorization", header_val))
            .dispatch()
            .await;
        assert_ne!(response.status(), Status::TooManyRequests);

        let limit = response
            .headers()
            .get_one("X-RateLimit-Limit")
            .expect("X-RateLimit-Limit header");
        assert_eq!(limit, "10000");

        let remaining = response
            .headers()
            .get_one("X-RateLimit-Remaining")
            .expect("X-RateLimit-Remaining header");
        assert_eq!(remaining, "9999");

        assert!(response.headers().get_one("X-RateLimit-Reset").is_some());
    }
}
