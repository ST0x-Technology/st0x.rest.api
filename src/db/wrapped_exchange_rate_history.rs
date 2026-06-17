use super::DbPool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewWrappedExchangeRateSnapshot {
    pub share_token_address: String,
    pub asset_token_address: String,
    pub assets_per_share: String,
    pub block_number: i64,
    pub block_timestamp: Option<i64>,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct WrappedExchangeRateSnapshot {
    pub share_token_address: String,
    pub asset_token_address: String,
    pub assets_per_share: String,
    pub block_number: i64,
    pub block_timestamp: Option<i64>,
    pub captured_at: String,
}

pub(crate) async fn insert_wrapped_exchange_rate_snapshots(
    pool: &DbPool,
    snapshots: &[NewWrappedExchangeRateSnapshot],
) -> Result<u64, sqlx::Error> {
    let mut rows_affected = 0;
    let mut tx = pool.begin().await?;

    for snapshot in snapshots {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO wrapped_exchange_rate_snapshots \
             (share_token_address, asset_token_address, assets_per_share, block_number, block_timestamp, captured_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&snapshot.share_token_address)
        .bind(&snapshot.asset_token_address)
        .bind(&snapshot.assets_per_share)
        .bind(snapshot.block_number)
        .bind(snapshot.block_timestamp)
        .bind(&snapshot.captured_at)
        .execute(&mut *tx)
        .await?;
        rows_affected += result.rows_affected();
    }

    tx.commit().await?;
    Ok(rows_affected)
}

pub(crate) async fn count_wrapped_exchange_rate_snapshots_for_share(
    pool: &DbPool,
    share_token_address: &str,
) -> Result<u64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) \
         FROM wrapped_exchange_rate_snapshots \
         WHERE share_token_address = ?",
    )
    .bind(share_token_address)
    .fetch_one(pool)
    .await?;

    Ok(u64::try_from(count).unwrap_or(0))
}

pub(crate) async fn list_wrapped_exchange_rate_snapshots_for_share(
    pool: &DbPool,
    share_token_address: &str,
    limit: u32,
    offset: u32,
) -> Result<Vec<WrappedExchangeRateSnapshot>, sqlx::Error> {
    sqlx::query_as::<_, WrappedExchangeRateSnapshot>(
        "SELECT share_token_address, asset_token_address, assets_per_share, block_number, block_timestamp, captured_at \
         FROM wrapped_exchange_rate_snapshots \
         WHERE share_token_address = ? \
         ORDER BY block_number DESC, captured_at DESC \
         LIMIT ? OFFSET ?",
    )
    .bind(share_token_address)
    .bind(i64::from(limit))
    .bind(i64::from(offset))
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, sqlx::FromRow)]
    struct SnapshotTestRow {
        share_token_address: String,
        asset_token_address: String,
        assets_per_share: String,
        block_number: i64,
        block_timestamp: Option<i64>,
        captured_at: String,
    }

    async fn test_pool() -> DbPool {
        let database_url = format!(
            "sqlite:file:{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        );
        crate::db::init(&database_url, 5)
            .await
            .expect("database init")
    }

    fn snapshot(
        share_token_address: &str,
        asset_token_address: &str,
        assets_per_share: &str,
        block_number: i64,
        captured_at: &str,
    ) -> NewWrappedExchangeRateSnapshot {
        NewWrappedExchangeRateSnapshot {
            share_token_address: share_token_address.to_string(),
            asset_token_address: asset_token_address.to_string(),
            assets_per_share: assets_per_share.to_string(),
            block_number,
            block_timestamp: Some(block_number + 1000),
            captured_at: captured_at.to_string(),
        }
    }

    #[tokio::test]
    async fn inserts_snapshots_append_only_and_idempotently() {
        let pool = test_pool().await;
        let first = snapshot("0xshare", "0xasset", "1.0", 100, "2026-06-04T10:00:00Z");
        let second = snapshot("0xshare", "0xasset", "1.1", 101, "2026-06-04T10:01:00Z");

        let inserted = insert_wrapped_exchange_rate_snapshots(
            &pool,
            &[first.clone(), first.clone(), second.clone()],
        )
        .await
        .expect("insert snapshots");

        assert_eq!(inserted, 2);

        let rows = list_snapshots_for_share(&pool, "0xshare")
            .await
            .expect("list snapshots");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].block_number, 101);
        assert_eq!(rows[1].block_number, 100);
    }

    #[tokio::test]
    async fn stores_all_snapshot_fields() {
        let pool = test_pool().await;
        let snapshots = vec![snapshot(
            "0xshare",
            "0xasset",
            "1.2",
            120,
            "2026-06-04T10:02:00Z",
        )];
        insert_wrapped_exchange_rate_snapshots(&pool, &snapshots)
            .await
            .expect("insert snapshots");

        let row = sqlx::query_as::<_, SnapshotTestRow>(
            "SELECT share_token_address, asset_token_address, assets_per_share, block_number, block_timestamp, captured_at \
             FROM wrapped_exchange_rate_snapshots \
             WHERE share_token_address = ?",
        )
        .bind("0xshare")
        .fetch_one(&pool)
        .await
        .expect("read snapshot");

        assert_eq!(row.share_token_address, "0xshare");
        assert_eq!(row.asset_token_address, "0xasset");
        assert_eq!(row.assets_per_share, "1.2");
        assert_eq!(row.block_number, 120);
        assert_eq!(row.block_timestamp, Some(1120));
        assert_eq!(row.captured_at, "2026-06-04T10:02:00Z");
    }

    #[tokio::test]
    async fn schema_supports_token_and_time_window_queries() {
        let pool = test_pool().await;
        let snapshots = vec![
            snapshot("0xshare", "0xasset", "1.0", 100, "2026-06-04T10:00:00Z"),
            snapshot("0xshare", "0xasset", "1.1", 101, "2026-06-04T10:01:00Z"),
            snapshot("0xshare", "0xasset", "1.2", 102, "2026-06-04T10:02:00Z"),
            snapshot("0xother", "0xasset", "9.9", 103, "2026-06-04T10:03:00Z"),
        ];
        insert_wrapped_exchange_rate_snapshots(&pool, &snapshots)
            .await
            .expect("insert snapshots");

        let rows = sqlx::query_as::<_, SnapshotTestRow>(
            "SELECT share_token_address, asset_token_address, assets_per_share, block_number, block_timestamp, captured_at \
             FROM wrapped_exchange_rate_snapshots \
             WHERE share_token_address = ? AND captured_at >= ? AND captured_at <= ? \
             ORDER BY captured_at DESC, block_number DESC",
        )
        .bind("0xshare")
        .bind("2026-06-04T10:01:00Z")
        .bind("2026-06-04T10:02:00Z")
        .fetch_all(&pool)
        .await
        .expect("query snapshots by window");

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].assets_per_share, "1.2");
        assert_eq!(rows[1].assets_per_share, "1.1");
    }

    #[tokio::test]
    async fn counts_and_lists_snapshots_for_share_with_pagination() {
        let pool = test_pool().await;
        let snapshots = vec![
            snapshot("0xshare", "0xasset", "1.0", 100, "2026-06-04T10:00:00Z"),
            snapshot("0xshare", "0xasset", "1.1", 101, "2026-06-04T10:01:00Z"),
            snapshot("0xshare", "0xasset", "1.2", 102, "2026-06-04T10:02:00Z"),
            snapshot("0xother", "0xasset", "9.9", 103, "2026-06-04T10:03:00Z"),
        ];
        insert_wrapped_exchange_rate_snapshots(&pool, &snapshots)
            .await
            .expect("insert snapshots");

        let count = count_wrapped_exchange_rate_snapshots_for_share(&pool, "0xshare")
            .await
            .expect("count snapshots");
        assert_eq!(count, 3);

        let rows = list_wrapped_exchange_rate_snapshots_for_share(&pool, "0xshare", 2, 1)
            .await
            .expect("list snapshots");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].block_number, 101);
        assert_eq!(rows[1].block_number, 100);
    }

    async fn list_snapshots_for_share(
        pool: &DbPool,
        share_token_address: &str,
    ) -> Result<Vec<SnapshotTestRow>, sqlx::Error> {
        sqlx::query_as::<_, SnapshotTestRow>(
            "SELECT share_token_address, asset_token_address, assets_per_share, block_number, block_timestamp, captured_at \
             FROM wrapped_exchange_rate_snapshots \
             WHERE share_token_address = ? \
             ORDER BY block_number DESC",
        )
        .bind(share_token_address)
        .fetch_all(pool)
        .await
    }
}
