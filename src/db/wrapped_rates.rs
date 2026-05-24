use super::DbPool;
use alloy::primitives::Address;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct WrappedRateSnapshot {
    pub token_address: String,
    pub block_number: i64,
    pub block_timestamp: i64,
    pub assets_per_share: String,
    pub asset_address: String,
    pub asset_symbol: String,
    pub asset_decimals: i64,
    pub captured_at: String,
}

pub(crate) struct NewWrappedRateSnapshot<'a> {
    pub token_address: Address,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub assets_per_share: &'a str,
    pub asset_address: Address,
    pub asset_symbol: &'a str,
    pub asset_decimals: u8,
}

fn lowercase_addr(addr: Address) -> String {
    format!("{addr:#x}")
}

pub(crate) async fn insert_snapshot(
    pool: &DbPool,
    s: &NewWrappedRateSnapshot<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO wrapped_exchange_rate_snapshots \
         (token_address, block_number, block_timestamp, assets_per_share, \
          asset_address, asset_symbol, asset_decimals) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(lowercase_addr(s.token_address))
    .bind(s.block_number as i64)
    .bind(s.block_timestamp as i64)
    .bind(s.assets_per_share)
    .bind(lowercase_addr(s.asset_address))
    .bind(s.asset_symbol)
    .bind(i64::from(s.asset_decimals))
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn get_latest_for_token(
    pool: &DbPool,
    token: Address,
) -> Result<Option<WrappedRateSnapshot>, sqlx::Error> {
    sqlx::query_as::<_, WrappedRateSnapshot>(
        "SELECT token_address, block_number, block_timestamp, assets_per_share, \
                asset_address, asset_symbol, asset_decimals, captured_at \
         FROM wrapped_exchange_rate_snapshots \
         WHERE token_address = ? \
         ORDER BY captured_at DESC, id DESC LIMIT 1",
    )
    .bind(lowercase_addr(token))
    .fetch_optional(pool)
    .await
}

pub(crate) async fn list_latest_per_token(
    pool: &DbPool,
) -> Result<Vec<WrappedRateSnapshot>, sqlx::Error> {
    sqlx::query_as::<_, WrappedRateSnapshot>(
        "SELECT s.token_address, s.block_number, s.block_timestamp, s.assets_per_share, \
                s.asset_address, s.asset_symbol, s.asset_decimals, s.captured_at \
         FROM wrapped_exchange_rate_snapshots s \
         INNER JOIN ( \
             SELECT token_address, MAX(id) AS max_id \
             FROM wrapped_exchange_rate_snapshots \
             GROUP BY token_address \
         ) latest ON s.id = latest.max_id \
         ORDER BY s.token_address",
    )
    .fetch_all(pool)
    .await
}

/// Return every snapshot row for `token` whose `block_number` falls in
/// `[from_block, to_block]` (inclusive on both ends). Used by the
/// exchange-rate history endpoint to merge snapshots with donation events
/// into a single chronological feed.
pub(crate) async fn list_for_token_in_range(
    pool: &DbPool,
    token: Address,
    from_block: Option<u64>,
    to_block: Option<u64>,
) -> Result<Vec<WrappedRateSnapshot>, sqlx::Error> {
    let from = from_block.map(|b| b as i64).unwrap_or(0);
    let to = to_block.map(|b| b as i64).unwrap_or(i64::MAX);
    sqlx::query_as::<_, WrappedRateSnapshot>(
        "SELECT token_address, block_number, block_timestamp, assets_per_share, \
                asset_address, asset_symbol, asset_decimals, captured_at \
         FROM wrapped_exchange_rate_snapshots \
         WHERE token_address = ? AND block_number BETWEEN ? AND ? \
         ORDER BY block_number ASC, id ASC",
    )
    .bind(lowercase_addr(token))
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
}

/// Earliest snapshot for `token`, ordered by `block_number ASC` (ties broken
/// by insert order via `id`). Returns `None` when no snapshot has been
/// recorded yet. Used by the donation scanner to seed `from_block` when no
/// scan-state cursor exists — we'd rather start from the first observation
/// than walk Base from genesis.
pub(crate) async fn get_earliest_for_token(
    pool: &DbPool,
    token: Address,
) -> Result<Option<WrappedRateSnapshot>, sqlx::Error> {
    sqlx::query_as::<_, WrappedRateSnapshot>(
        "SELECT token_address, block_number, block_timestamp, assets_per_share, \
                asset_address, asset_symbol, asset_decimals, captured_at \
         FROM wrapped_exchange_rate_snapshots \
         WHERE token_address = ? \
         ORDER BY block_number ASC, id ASC LIMIT 1",
    )
    .bind(lowercase_addr(token))
    .fetch_optional(pool)
    .await
}

pub(crate) async fn get_at_or_before_block(
    pool: &DbPool,
    token: Address,
    block_number: u64,
) -> Result<Option<WrappedRateSnapshot>, sqlx::Error> {
    sqlx::query_as::<_, WrappedRateSnapshot>(
        "SELECT token_address, block_number, block_timestamp, assets_per_share, \
                asset_address, asset_symbol, asset_decimals, captured_at \
         FROM wrapped_exchange_rate_snapshots \
         WHERE token_address = ? AND block_number <= ? \
         ORDER BY block_number DESC, id DESC LIMIT 1",
    )
    .bind(lowercase_addr(token))
    .bind(block_number as i64)
    .fetch_optional(pool)
    .await
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

    #[rocket::async_test]
    async fn test_insert_and_get_latest() {
        let pool = fresh_pool().await;
        let token = addr(1);
        let asset = addr(2);

        insert_snapshot(
            &pool,
            &NewWrappedRateSnapshot {
                token_address: token,
                block_number: 100,
                block_timestamp: 1_700_000_000,
                assets_per_share: "1.0",
                asset_address: asset,
                asset_symbol: "tABC",
                asset_decimals: 18,
            },
        )
        .await
        .unwrap();

        insert_snapshot(
            &pool,
            &NewWrappedRateSnapshot {
                token_address: token,
                block_number: 200,
                block_timestamp: 1_700_001_000,
                assets_per_share: "1.05",
                asset_address: asset,
                asset_symbol: "tABC",
                asset_decimals: 18,
            },
        )
        .await
        .unwrap();

        let latest = get_latest_for_token(&pool, token).await.unwrap().unwrap();
        assert_eq!(latest.assets_per_share, "1.05");
        assert_eq!(latest.block_number, 200);
    }

    #[rocket::async_test]
    async fn test_get_at_or_before_block_picks_latest_not_after() {
        let pool = fresh_pool().await;
        let token = addr(7);
        let asset = addr(8);

        for (bn, rate) in [(100u64, "1.00"), (200, "1.10"), (300, "1.20")] {
            insert_snapshot(
                &pool,
                &NewWrappedRateSnapshot {
                    token_address: token,
                    block_number: bn,
                    block_timestamp: bn * 10,
                    assets_per_share: rate,
                    asset_address: asset,
                    asset_symbol: "tXYZ",
                    asset_decimals: 18,
                },
            )
            .await
            .unwrap();
        }

        let s = get_at_or_before_block(&pool, token, 250)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.block_number, 200);
        assert_eq!(s.assets_per_share, "1.10");

        let s = get_at_or_before_block(&pool, token, 300)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.block_number, 300);

        let s = get_at_or_before_block(&pool, token, 50).await.unwrap();
        assert!(s.is_none());
    }

    #[rocket::async_test]
    async fn test_list_latest_per_token() {
        let pool = fresh_pool().await;
        let token_a = addr(1);
        let token_b = addr(2);

        insert_snapshot(
            &pool,
            &NewWrappedRateSnapshot {
                token_address: token_a,
                block_number: 100,
                block_timestamp: 1_700_000_000,
                assets_per_share: "1.0",
                asset_address: addr(11),
                asset_symbol: "tA",
                asset_decimals: 18,
            },
        )
        .await
        .unwrap();
        insert_snapshot(
            &pool,
            &NewWrappedRateSnapshot {
                token_address: token_a,
                block_number: 200,
                block_timestamp: 1_700_000_010,
                assets_per_share: "1.5",
                asset_address: addr(11),
                asset_symbol: "tA",
                asset_decimals: 18,
            },
        )
        .await
        .unwrap();
        insert_snapshot(
            &pool,
            &NewWrappedRateSnapshot {
                token_address: token_b,
                block_number: 150,
                block_timestamp: 1_700_000_005,
                assets_per_share: "2.0",
                asset_address: addr(12),
                asset_symbol: "tB",
                asset_decimals: 18,
            },
        )
        .await
        .unwrap();

        let rows = list_latest_per_token(&pool).await.unwrap();
        assert_eq!(rows.len(), 2);

        let by_token: std::collections::HashMap<_, _> = rows
            .into_iter()
            .map(|r| (r.token_address.clone(), r))
            .collect();
        assert_eq!(by_token[&lowercase_addr(token_a)].assets_per_share, "1.5");
        assert_eq!(by_token[&lowercase_addr(token_b)].assets_per_share, "2.0");
    }
}
