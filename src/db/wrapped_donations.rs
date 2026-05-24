//! Persistence layer for `wrapped_donation_events` and the per-wrapper
//! `wrapped_donation_scan_state` cursor.
//!
//! A donation event is the OARV-side `Transfer(to=wrapper, value=X)` log
//! that does NOT have a matching `Deposit(...)` on the wrapper in the same
//! transaction. The scanner (`crate::wrapped_donations`) writes rows; the
//! `/v1/tokens/exchange-rates/history` route reads them via
//! [`list_for_wrapper`].

use super::DbPool;
use alloy::primitives::{Address, B256};

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct DonationEventRow {
    pub id: i64,
    #[allow(dead_code)]
    pub wrapper_address: String,
    pub donor_address: String,
    pub asset_amount: String,
    pub block_number: i64,
    pub block_timestamp: i64,
    pub tx_hash: String,
    /// `None` when the scanner could not read `convertToAssets` at the
    /// donation block (RPC pruned, transient failure). The route handler
    /// performs a bounded best-effort backfill on read; until that
    /// succeeds, callers see JSON `null` for this field.
    pub new_assets_per_share: Option<String>,
    #[allow(dead_code)]
    pub captured_at: String,
}

pub(crate) struct NewDonationEvent<'a> {
    pub wrapper_address: Address,
    pub donor_address: Address,
    pub asset_amount: &'a str,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub tx_hash: B256,
    /// `None` indicates the scanner couldn't read the rate at this block.
    /// The row is still persisted so the cursor can advance safely; the
    /// route layer attempts a lazy backfill on the next history request.
    pub new_assets_per_share: Option<&'a str>,
}

fn lowercase_addr(addr: Address) -> String {
    format!("{addr:#x}")
}

fn lowercase_b256(value: B256) -> String {
    format!("{value:#x}")
}

/// Insert a donation event. The widened `(wrapper_address, tx_hash,
/// donor_address, asset_amount, block_number)` unique constraint makes this
/// idempotent — replays of the scanner over the same block range silently
/// no-op via `INSERT OR IGNORE`.
///
/// `new_assets_per_share` may be `None` when the scanner couldn't read the
/// rate at the donation block; the route layer will lazily backfill on
/// the next history request.
#[allow(dead_code)]
pub(crate) async fn insert_event(
    pool: &DbPool,
    event: &NewDonationEvent<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO wrapped_donation_events \
         (wrapper_address, donor_address, asset_amount, block_number, \
          block_timestamp, tx_hash, new_assets_per_share) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(lowercase_addr(event.wrapper_address))
    .bind(lowercase_addr(event.donor_address))
    .bind(event.asset_amount)
    .bind(event.block_number as i64)
    .bind(event.block_timestamp as i64)
    .bind(lowercase_b256(event.tx_hash))
    .bind(event.new_assets_per_share)
    .execute(pool)
    .await?;
    Ok(())
}

/// Backfill the `new_assets_per_share` value for a row that was originally
/// stored with NULL because the scanner couldn't read the rate at the
/// donation block. Targeted by `id` so we only touch the exact row we just
/// re-fetched for.
#[allow(dead_code)]
pub(crate) async fn update_rate(pool: &DbPool, id: i64, new_rate: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE wrapped_donation_events SET new_assets_per_share = ? WHERE id = ?")
        .bind(new_rate)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Return every donation event for `wrapper` within the optional
/// `[from_block, to_block]` window (inclusive on both ends). Ordering is by
/// `block_number` ascending so the route can merge with snapshots in
/// chronological order before paginating.
pub(crate) async fn list_for_wrapper(
    pool: &DbPool,
    wrapper: Address,
    from_block: Option<u64>,
    to_block: Option<u64>,
) -> Result<Vec<DonationEventRow>, sqlx::Error> {
    let from = from_block.map(|b| b as i64).unwrap_or(0);
    let to = to_block.map(|b| b as i64).unwrap_or(i64::MAX);
    sqlx::query_as::<_, DonationEventRow>(
        "SELECT id, wrapper_address, donor_address, asset_amount, block_number, \
                block_timestamp, tx_hash, new_assets_per_share, captured_at \
         FROM wrapped_donation_events \
         WHERE wrapper_address = ? AND block_number BETWEEN ? AND ? \
         ORDER BY block_number ASC, id ASC",
    )
    .bind(lowercase_addr(wrapper))
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}

#[allow(dead_code)]
pub(crate) async fn get_scan_state(
    pool: &DbPool,
    wrapper: Address,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT last_scanned_block FROM wrapped_donation_scan_state WHERE wrapper_address = ?",
    )
    .bind(lowercase_addr(wrapper))
    .fetch_optional(pool)
    .await
}

#[allow(dead_code)]
pub(crate) async fn update_scan_state(
    pool: &DbPool,
    wrapper: Address,
    last_scanned_block: u64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO wrapped_donation_scan_state (wrapper_address, last_scanned_block, updated_at) \
         VALUES (?, ?, datetime('now')) \
         ON CONFLICT (wrapper_address) DO UPDATE SET \
            last_scanned_block = excluded.last_scanned_block, \
            updated_at = excluded.updated_at",
    )
    .bind(lowercase_addr(wrapper))
    .bind(last_scanned_block as i64)
    .execute(pool)
    .await?;
    Ok(())
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

    fn addr(byte: u8) -> Address {
        let mut buf = [0u8; 20];
        buf[19] = byte;
        Address::from(buf)
    }

    fn tx(byte: u8) -> B256 {
        let mut buf = [0u8; 32];
        buf[31] = byte;
        B256::from(buf)
    }

    #[rocket::async_test]
    async fn test_insert_and_list_donation_events() {
        let pool = fresh_pool().await;
        let wrapper = addr(1);
        let donor = addr(2);

        insert_event(
            &pool,
            &NewDonationEvent {
                wrapper_address: wrapper,
                donor_address: donor,
                asset_amount: "100.0",
                block_number: 100,
                block_timestamp: 1_700_000_000,
                tx_hash: tx(1),
                new_assets_per_share: Some("1.05"),
            },
        )
        .await
        .unwrap();
        insert_event(
            &pool,
            &NewDonationEvent {
                wrapper_address: wrapper,
                donor_address: donor,
                asset_amount: "50.0",
                block_number: 200,
                block_timestamp: 1_700_001_000,
                tx_hash: tx(2),
                new_assets_per_share: Some("1.10"),
            },
        )
        .await
        .unwrap();

        let events = list_for_wrapper(&pool, wrapper, None, None).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].block_number, 100);
        assert_eq!(events[1].block_number, 200);
    }

    #[rocket::async_test]
    async fn test_insert_event_is_idempotent() {
        let pool = fresh_pool().await;
        let wrapper = addr(1);
        let donor = addr(2);
        let event = NewDonationEvent {
            wrapper_address: wrapper,
            donor_address: donor,
            asset_amount: "100.0",
            block_number: 100,
            block_timestamp: 1_700_000_000,
            tx_hash: tx(1),
            new_assets_per_share: Some("1.05"),
        };
        insert_event(&pool, &event).await.unwrap();
        // Replay — should be silently ignored.
        insert_event(&pool, &event).await.unwrap();

        let events = list_for_wrapper(&pool, wrapper, None, None).await.unwrap();
        assert_eq!(events.len(), 1);
    }

    #[rocket::async_test]
    async fn test_list_for_wrapper_respects_block_range() {
        let pool = fresh_pool().await;
        let wrapper = addr(1);
        let donor = addr(2);
        for (i, bn) in [(1u8, 100u64), (2, 200), (3, 300)] {
            insert_event(
                &pool,
                &NewDonationEvent {
                    wrapper_address: wrapper,
                    donor_address: donor,
                    asset_amount: "10",
                    block_number: bn,
                    block_timestamp: bn * 10,
                    tx_hash: tx(i),
                    new_assets_per_share: Some("1.0"),
                },
            )
            .await
            .unwrap();
        }
        let events = list_for_wrapper(&pool, wrapper, Some(150), Some(250))
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].block_number, 200);
    }

    #[rocket::async_test]
    async fn test_insert_event_persists_null_rate() {
        let pool = fresh_pool().await;
        let wrapper = addr(1);
        let donor = addr(2);
        insert_event(
            &pool,
            &NewDonationEvent {
                wrapper_address: wrapper,
                donor_address: donor,
                asset_amount: "5.0",
                block_number: 500,
                block_timestamp: 1_700_005_000,
                tx_hash: tx(9),
                new_assets_per_share: None,
            },
        )
        .await
        .unwrap();

        let events = list_for_wrapper(&pool, wrapper, None, None).await.unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].new_assets_per_share.is_none());
    }

    #[rocket::async_test]
    async fn test_update_rate_backfills_null_row() {
        let pool = fresh_pool().await;
        let wrapper = addr(1);
        let donor = addr(2);
        insert_event(
            &pool,
            &NewDonationEvent {
                wrapper_address: wrapper,
                donor_address: donor,
                asset_amount: "5.0",
                block_number: 500,
                block_timestamp: 1_700_005_000,
                tx_hash: tx(9),
                new_assets_per_share: None,
            },
        )
        .await
        .unwrap();

        let rows = list_for_wrapper(&pool, wrapper, None, None).await.unwrap();
        update_rate(&pool, rows[0].id, "1.07").await.unwrap();

        let rows = list_for_wrapper(&pool, wrapper, None, None).await.unwrap();
        assert_eq!(rows[0].new_assets_per_share.as_deref(), Some("1.07"));
    }

    #[rocket::async_test]
    async fn test_scan_state_upsert() {
        let pool = fresh_pool().await;
        let wrapper = addr(7);
        assert!(get_scan_state(&pool, wrapper).await.unwrap().is_none());

        update_scan_state(&pool, wrapper, 100).await.unwrap();
        assert_eq!(get_scan_state(&pool, wrapper).await.unwrap(), Some(100));

        // Update — should overwrite, not insert a second row.
        update_scan_state(&pool, wrapper, 200).await.unwrap();
        assert_eq!(get_scan_state(&pool, wrapper).await.unwrap(), Some(200));
    }
}
