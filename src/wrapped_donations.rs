//! Detection and persistence of ERC4626 "donation" events on wrapped tStock
//! tokens.
//!
//! A donation is a direct asset transfer to the wrapper that increases
//! `assetsPerShare` without a corresponding `Deposit` (i.e. without minting
//! new wrapper shares). The detection algorithm is:
//!
//! 1. For a wrapper, find its OARV asset address (from the latest snapshot).
//! 2. From `last_scanned_block + 1` up to the chain head, fetch
//!    `Transfer(*, wrapper, *)` logs on the OARV contract (chunked by 5000
//!    blocks per `eth_getLogs` call to stay under provider limits).
//! 3. For each Transfer, look at the same tx's `Deposit(sender, owner,
//!    assets, shares)` events on the wrapper. If the Transfer's `value`
//!    matches the Deposit's `assets`, it's a regular deposit — skip.
//!    Otherwise, classify as a donation.
//! 4. For each donation, read `convertToAssets(10^decimals)` at the donation
//!    block via [`crate::wrapped_rates::IERC4626Rate`] to compute the new
//!    `assetsPerShare`.
//! 5. Persist via [`crate::db::wrapped_donations::insert_event`] and bump
//!    [`crate::db::wrapped_donations::update_scan_state`].
//!
//! ## Current status
//!
//! The scanner is **stubbed**. The DB layer
//! ([`crate::db::wrapped_donations`]) and the
//! `/v1/tokens/exchange-rates/history` endpoint are wired up and return any
//! donation rows that happen to be present, but no code populates them yet.
//! Today's wrapped tokens have static 1:1 rates so the missing history is
//! harmless; the moment a wrapper starts receiving real donations the
//! scanner needs to land.
//!
//! See the module-level doc-comment for the implementation sketch — the
//! types below (`DonationEvent`, `scan_for_wrapper`) intentionally exist as
//! the future entry points so route/test wiring doesn't churn when the real
//! scanner arrives.

use crate::db::DbPool;
use alloy::primitives::{Address, B256};

/// Donation event as observed by the scanner. Mirrors the DB row shape but
/// uses strong alloy types so callers can hand it straight to the DB layer
/// without parsing strings again.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct DonationEvent {
    pub wrapper_address: Address,
    pub donor_address: Address,
    pub asset_amount: String,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub tx_hash: B256,
    pub new_assets_per_share: String,
}

/// Stubbed scanner entry point. The real implementation will:
///
/// 1. Look up `wrapper_address`'s OARV asset and the current
///    `last_scanned_block`.
/// 2. Walk OARV `Transfer` logs in 5_000-block chunks (configurable via
///    [`SCAN_CHUNK_BLOCKS`]) up to the chain head.
/// 3. Diff against wrapper `Deposit` logs in the same blocks; anything left
///    over is a donation.
/// 4. Insert via [`crate::db::wrapped_donations::insert_event`] and bump the
///    scan cursor.
///
/// Today this is a no-op so the endpoint can ship while the scanner is
/// being built out. Returning `Ok(0)` mirrors "we walked 0 new blocks", and
/// keeps the call site shape stable.
#[allow(dead_code)]
pub(crate) async fn scan_for_wrapper(
    _pool: &DbPool,
    _wrapper_address: Address,
) -> Result<u64, ScanError> {
    // Intentionally a no-op until the RPC log-walking implementation lands.
    Ok(0)
}

/// Stay below most Base RPC providers' 10_000-block `eth_getLogs` cap with
/// some headroom for retries.
#[allow(dead_code)]
pub(crate) const SCAN_CHUNK_BLOCKS: u64 = 5_000;

/// Hard cap on how far the inline scan walks per request before bailing
/// out. With Base at ~2s block time this is ~28h of activity — past that
/// we'd rather return whatever's stored and warn than block the endpoint.
#[allow(dead_code)]
pub(crate) const MAX_INLINE_SCAN_BLOCKS: u64 = 50_000;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub(crate) enum ScanError {
    #[error("rpc unavailable: {0}")]
    Rpc(String),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("scanner not yet implemented")]
    NotImplemented,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn fresh_pool() -> DbPool {
        let id = uuid::Uuid::new_v4();
        db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("init db")
    }

    #[rocket::async_test]
    async fn test_scan_is_currently_a_noop() {
        let pool = fresh_pool().await;
        let wrapper: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        // Stubbed scanner returns Ok(0) and writes nothing.
        let walked = scan_for_wrapper(&pool, wrapper).await.unwrap();
        assert_eq!(walked, 0);
    }
}
