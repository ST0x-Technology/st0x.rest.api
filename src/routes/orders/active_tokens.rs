use crate::cache::AppCache;
use alloy::primitives::Address;
use std::time::Duration;

const ACTIVE_TOKEN_TTL: Duration = Duration::from_secs(10 * 60);
const ACTIVE_TOKEN_CAPACITY: u64 = 1_000;

/// Set of token addresses that have received a real `/v1/orders/token/...`
/// request in the last `ACTIVE_TOKEN_TTL`. Consulted by the cache warmer to
/// skip refreshing tokens nobody is looking at, removing the fixed RPC floor
/// from the warmer's all-tokens-every-cycle pass. Entries are inserted only
/// from the user-facing route handler so the warmer never refreshes its own
/// reason to keep refreshing.
pub(crate) type ActiveTokenCache = AppCache<Address, ()>;

pub(crate) fn active_token_cache() -> ActiveTokenCache {
    AppCache::new(ACTIVE_TOKEN_CAPACITY, ACTIVE_TOKEN_TTL)
}
