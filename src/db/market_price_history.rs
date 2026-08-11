use super::DbPool;
use sqlx::{QueryBuilder, Sqlite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewMarketPriceSnapshot {
    pub chain_id: i64,
    pub asset_token_address: String,
    pub quote_token_address: String,
    pub best_bid: String,
    pub best_ask: String,
    pub midpoint: String,
    pub assets_per_share: String,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct MarketPriceSnapshot {
    pub chain_id: i64,
    pub asset_token_address: String,
    pub quote_token_address: String,
    pub best_bid: String,
    pub best_ask: String,
    pub midpoint: String,
    pub assets_per_share: String,
    pub observed_at: i64,
}

pub(crate) async fn insert_market_price_snapshots(
    pool: &DbPool,
    snapshots: &[NewMarketPriceSnapshot],
) -> Result<u64, sqlx::Error> {
    if snapshots.is_empty() {
        return Ok(0);
    }

    let mut tx = pool.begin().await?;
    let mut query = QueryBuilder::<Sqlite>::new(
        "INSERT OR IGNORE INTO market_price_snapshots \
         (chain_id, asset_token_address, quote_token_address, best_bid, best_ask, midpoint, assets_per_share, observed_at) ",
    );
    query.push_values(snapshots, |mut row, snapshot| {
        row.push_bind(snapshot.chain_id)
            .push_bind(&snapshot.asset_token_address)
            .push_bind(&snapshot.quote_token_address)
            .push_bind(&snapshot.best_bid)
            .push_bind(&snapshot.best_ask)
            .push_bind(&snapshot.midpoint)
            .push_bind(&snapshot.assets_per_share)
            .push_bind(snapshot.observed_at);
    });
    let rows_affected = query.build().execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(rows_affected)
}

pub(crate) async fn delete_market_price_snapshots_before(
    pool: &DbPool,
    cutoff: i64,
) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM market_price_snapshots WHERE observed_at < ?")
        .bind(cutoff)
        .execute(pool)
        .await
        .map(|result| result.rows_affected())
}

pub(crate) async fn list_market_prices_at_or_before(
    pool: &DbPool,
    chain_id: i64,
    quote_token_address: &str,
    not_before: i64,
    observed_at: i64,
) -> Result<Vec<MarketPriceSnapshot>, sqlx::Error> {
    sqlx::query_as::<_, MarketPriceSnapshot>(
        "SELECT chain_id, asset_token_address, quote_token_address, best_bid, best_ask, midpoint, assets_per_share, observed_at \
         FROM ( \
             SELECT chain_id, asset_token_address, quote_token_address, best_bid, best_ask, midpoint, assets_per_share, observed_at, \
                    ROW_NUMBER() OVER (PARTITION BY asset_token_address ORDER BY observed_at DESC) AS row_number \
             FROM market_price_snapshots \
             WHERE chain_id = ? AND quote_token_address = ? \
               AND observed_at >= ? AND observed_at <= ? \
         ) ranked \
         WHERE row_number = 1 \
         ORDER BY asset_token_address",
    )
    .bind(chain_id)
    .bind(quote_token_address)
    .bind(not_before)
    .bind(observed_at)
    .fetch_all(pool)
    .await
}

pub(crate) async fn list_market_price_history(
    pool: &DbPool,
    chain_id: i64,
    asset_token_addresses: &[String],
    quote_token_address: &str,
    start_time: i64,
    end_time: i64,
    interval: i64,
) -> Result<Vec<MarketPriceSnapshot>, sqlx::Error> {
    if asset_token_addresses.is_empty() {
        return Ok(Vec::new());
    }

    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT chain_id, asset_token_address, quote_token_address, best_bid, best_ask, midpoint, assets_per_share, observed_at \
         FROM ( \
             SELECT chain_id, asset_token_address, quote_token_address, best_bid, best_ask, midpoint, assets_per_share, observed_at, \
                    ROW_NUMBER() OVER ( \
                        PARTITION BY ((observed_at - ",
    );
    query
        .push_bind(start_time)
        .push(") / ")
        .push_bind(interval)
        .push(
            ") \
                        ORDER BY observed_at DESC, asset_token_address DESC \
                    ) AS row_number \
             FROM market_price_snapshots \
             WHERE chain_id = ",
        )
        .push_bind(chain_id)
        .push(" AND asset_token_address IN (");
    {
        let mut addresses = query.separated(", ");
        for address in asset_token_addresses {
            addresses.push_bind(address);
        }
    }
    query
        .push(") AND quote_token_address = ")
        .push_bind(quote_token_address)
        .push(" AND observed_at >= ")
        .push_bind(start_time)
        .push(" AND observed_at <= ")
        .push_bind(end_time)
        .push(
            " \
         ) ranked \
         WHERE row_number = 1 \
         ORDER BY observed_at ASC",
        );
    query
        .build_query_as::<MarketPriceSnapshot>()
        .fetch_all(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> DbPool {
        let database_url = format!(
            "sqlite:file:{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        );
        crate::db::init(&database_url, 5)
            .await
            .expect("database init")
    }

    fn snapshot(asset: &str, midpoint: &str, observed_at: i64) -> NewMarketPriceSnapshot {
        NewMarketPriceSnapshot {
            chain_id: 8453,
            asset_token_address: asset.to_string(),
            quote_token_address: "0xquote".to_string(),
            best_bid: midpoint.to_string(),
            best_ask: midpoint.to_string(),
            midpoint: midpoint.to_string(),
            assets_per_share: "1".to_string(),
            observed_at,
        }
    }

    #[tokio::test]
    async fn inserts_snapshots_idempotently_and_lists_latest_per_asset() {
        let pool = test_pool().await;
        let snapshots = vec![
            snapshot("0xasset-a", "10", 100),
            snapshot("0xasset-a", "11", 160),
            snapshot("0xasset-b", "20", 120),
        ];

        let inserted = insert_market_price_snapshots(&pool, &snapshots)
            .await
            .expect("insert snapshots");
        let duplicate = insert_market_price_snapshots(&pool, &snapshots)
            .await
            .expect("ignore duplicate snapshots");

        assert_eq!(inserted, 3);
        assert_eq!(duplicate, 0);

        let latest = list_market_prices_at_or_before(&pool, 8453, "0xquote", 0, 200)
            .await
            .expect("list latest");
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].asset_token_address, "0xasset-a");
        assert_eq!(latest[0].midpoint, "11");
        assert_eq!(latest[1].asset_token_address, "0xasset-b");

        let expired = list_market_prices_at_or_before(&pool, 8453, "0xquote", 180, 200)
            .await
            .expect("exclude expired snapshots");
        assert!(expired.is_empty());
    }

    #[tokio::test]
    async fn queries_history_window_and_deletes_expired_rows() {
        let pool = test_pool().await;
        insert_market_price_snapshots(
            &pool,
            &[
                snapshot("0xasset", "10", 100),
                snapshot("0xasset", "11", 160),
                snapshot("0xasset", "12", 220),
            ],
        )
        .await
        .expect("insert snapshots");

        let history = list_market_price_history(
            &pool,
            8453,
            &["0xasset".to_string()],
            "0xquote",
            100,
            220,
            120,
        )
        .await
        .expect("list history");
        assert_eq!(
            history
                .iter()
                .map(|row| row.midpoint.as_str())
                .collect::<Vec<_>>(),
            vec!["11", "12"]
        );

        let deleted = delete_market_price_snapshots_before(&pool, 200)
            .await
            .expect("delete expired");
        assert_eq!(deleted, 2);
        let remaining = list_market_prices_at_or_before(&pool, 8453, "0xquote", 0, 300)
            .await
            .expect("get remaining snapshots");
        assert_eq!(remaining.len(), 1);
    }
}
