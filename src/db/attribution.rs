//! Persistence for confirmed trade attribution and reporting.

use super::DbPool;
use crate::attribution::compute_api_key_hash;
use alloy::primitives::{Address, B256};
use futures::TryStreamExt;
use sqlx::{FromRow, QueryBuilder, Sqlite, Transaction};
use std::str::FromStr;

#[derive(Debug, Clone, FromRow)]
pub(crate) struct ApiKeyIdentity {
    pub(crate) id: i64,
    pub(crate) key_id: String,
    pub(crate) label: String,
    pub(crate) owner: String,
}

#[derive(Debug, FromRow)]
pub(crate) struct AttributionCursor {
    pub(crate) start_block: i64,
    pub(crate) last_block: i64,
    pub(crate) last_log_index: i64,
    pub(crate) last_trade_id: String,
}

pub(crate) struct CursorPosition<'a> {
    pub(crate) block: i64,
    pub(crate) log_index: i64,
    pub(crate) trade_id: &'a str,
}

pub(crate) struct NewAttributedTrade<'a> {
    pub(crate) chain_id: i64,
    pub(crate) raindex_address: String,
    pub(crate) indexed_trade_id: String,
    pub(crate) transaction_hash: String,
    pub(crate) log_index: i64,
    pub(crate) block_number: i64,
    pub(crate) block_timestamp: i64,
    pub(crate) order_hash: String,
    pub(crate) taker: String,
    pub(crate) api_key_hash: B256,
    pub(crate) identity: Option<&'a ApiKeyIdentity>,
    pub(crate) input_token: String,
    pub(crate) input_amount: String,
    pub(crate) output_token: String,
    pub(crate) output_amount: String,
}

pub(crate) async fn snapshot_current_api_keys(pool: &DbPool) -> Result<(), sqlx::Error> {
    let identities = sqlx::query_as::<_, ApiKeyIdentity>(
        "SELECT id, key_id, label, owner FROM api_keys ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    let mut transaction = pool.begin().await?;
    for identity in identities {
        upsert_api_key_identity(&mut transaction, &identity).await?;
    }
    transaction.commit().await
}

pub(crate) async fn snapshot_api_key(
    transaction: &mut Transaction<'_, Sqlite>,
    id: i64,
    key_id: &str,
    label: &str,
    owner: &str,
) -> Result<(), sqlx::Error> {
    upsert_api_key_identity(
        transaction,
        &ApiKeyIdentity {
            id,
            key_id: key_id.to_string(),
            label: label.to_string(),
            owner: owner.to_string(),
        },
    )
    .await
}

async fn upsert_api_key_identity(
    transaction: &mut Transaction<'_, Sqlite>,
    identity: &ApiKeyIdentity,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO attribution_api_keys (\
            api_key_hash, api_key_database_id, api_key_id, api_key_label, api_key_owner\
         ) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(api_key_hash) DO UPDATE SET \
            api_key_database_id = excluded.api_key_database_id, \
            api_key_id = excluded.api_key_id, \
            api_key_label = excluded.api_key_label, \
            api_key_owner = excluded.api_key_owner, \
            updated_at = datetime('now') \
         WHERE api_key_database_id IS NOT excluded.api_key_database_id \
            OR api_key_id IS NOT excluded.api_key_id \
            OR api_key_label IS NOT excluded.api_key_label \
            OR api_key_owner IS NOT excluded.api_key_owner",
    )
    .bind(compute_api_key_hash(&identity.key_id).to_string())
    .bind(identity.id)
    .bind(&identity.key_id)
    .bind(&identity.label)
    .bind(&identity.owner)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) async fn load_api_key_identities(
    pool: &DbPool,
) -> Result<Vec<ApiKeyIdentity>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyIdentity>(
        "SELECT api_key_database_id AS id, api_key_id AS key_id, \
                api_key_label AS label, api_key_owner AS owner \
         FROM attribution_api_keys ORDER BY api_key_id",
    )
    .fetch_all(pool)
    .await
}

pub(crate) async fn record_signer(pool: &DbPool, signer: Address) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO attribution_signers (address) VALUES (?) \
         ON CONFLICT(address) DO NOTHING",
    )
    .bind(signer.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn load_signers(pool: &DbPool) -> Result<Vec<Address>, sqlx::Error> {
    let values: Vec<String> =
        sqlx::query_scalar("SELECT address FROM attribution_signers ORDER BY first_seen_at")
            .fetch_all(pool)
            .await?;
    Ok(values
        .into_iter()
        .filter_map(|value| match Address::from_str(&value) {
            Ok(address) => Some(address),
            Err(error) => {
                tracing::error!(%error, address = %value, "invalid trusted attribution signer");
                None
            }
        })
        .collect())
}

pub(crate) async fn load_cursor(
    transaction: &mut Transaction<'_, Sqlite>,
    chain_id: i64,
    raindex_address: &str,
) -> Result<Option<AttributionCursor>, sqlx::Error> {
    sqlx::query_as::<_, AttributionCursor>(
        "SELECT start_block, last_block, last_log_index, last_trade_id \
         FROM attribution_sync_cursors WHERE chain_id = ? AND raindex_address = ?",
    )
    .bind(chain_id)
    .bind(raindex_address)
    .fetch_optional(&mut **transaction)
    .await
}

pub(crate) async fn store_batch(
    transaction: &mut Transaction<'_, Sqlite>,
    chain_id: i64,
    raindex_address: &str,
    start_block: i64,
    next_cursor: CursorPosition<'_>,
    trades: &[NewAttributedTrade<'_>],
) -> Result<(), sqlx::Error> {
    for trade in trades {
        let identity = trade.identity.as_ref();
        sqlx::query(
            "INSERT INTO attributed_trades (\
                chain_id, raindex_address, indexed_trade_id, transaction_hash, log_index, \
                block_number, block_timestamp, order_hash, taker, api_key_hash, \
                api_key_database_id, api_key_id, api_key_label, api_key_owner, \
                input_token, input_amount, output_token, output_amount\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(chain_id, raindex_address, indexed_trade_id) DO UPDATE SET \
                api_key_database_id = excluded.api_key_database_id, \
                api_key_id = excluded.api_key_id, \
                api_key_label = excluded.api_key_label, \
                api_key_owner = excluded.api_key_owner, \
                updated_at = datetime('now')",
        )
        .bind(trade.chain_id)
        .bind(&trade.raindex_address)
        .bind(&trade.indexed_trade_id)
        .bind(&trade.transaction_hash)
        .bind(trade.log_index)
        .bind(trade.block_number)
        .bind(trade.block_timestamp)
        .bind(&trade.order_hash)
        .bind(&trade.taker)
        .bind(trade.api_key_hash.to_string())
        .bind(identity.map(|value| value.id))
        .bind(identity.map(|value| value.key_id.as_str()))
        .bind(identity.map(|value| value.label.as_str()))
        .bind(identity.map(|value| value.owner.as_str()))
        .bind(&trade.input_token)
        .bind(&trade.input_amount)
        .bind(&trade.output_token)
        .bind(&trade.output_amount)
        .execute(&mut **transaction)
        .await?;
    }
    sqlx::query(
        "INSERT INTO attribution_sync_cursors (\
            chain_id, raindex_address, start_block, last_block, last_log_index, last_trade_id\
         ) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(chain_id, raindex_address) DO UPDATE SET \
            start_block = excluded.start_block, \
            last_block = excluded.last_block, \
            last_log_index = excluded.last_log_index, \
            last_trade_id = excluded.last_trade_id, \
            updated_at = datetime('now')",
    )
    .bind(chain_id)
    .bind(raindex_address)
    .bind(start_block)
    .bind(next_cursor.block)
    .bind(next_cursor.log_index)
    .bind(next_cursor.trade_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) async fn reset_target(
    transaction: &mut Transaction<'_, Sqlite>,
    chain_id: i64,
    raindex_address: &str,
    configured_start_block: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM attributed_trades \
         WHERE chain_id = ? AND raindex_address = ? AND block_number < ?",
    )
    .bind(chain_id)
    .bind(raindex_address)
    .bind(configured_start_block)
    .execute(&mut **transaction)
    .await?;
    sqlx::query("DELETE FROM attribution_sync_cursors WHERE chain_id = ? AND raindex_address = ?")
        .bind(chain_id)
        .bind(raindex_address)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

#[derive(Debug, FromRow)]
pub(crate) struct AttributedTradeRow {
    pub(crate) indexed_trade_id: String,
    pub(crate) chain_id: i64,
    pub(crate) raindex_address: String,
    pub(crate) transaction_hash: String,
    pub(crate) log_index: i64,
    pub(crate) block_number: i64,
    pub(crate) block_timestamp: i64,
    pub(crate) order_hash: String,
    pub(crate) taker: String,
    pub(crate) api_key_hash: String,
    pub(crate) api_key_database_id: Option<i64>,
    pub(crate) api_key_id: Option<String>,
    pub(crate) api_key_label: Option<String>,
    pub(crate) api_key_owner: Option<String>,
    pub(crate) input_token: String,
    pub(crate) input_amount: String,
    pub(crate) output_token: String,
    pub(crate) output_amount: String,
}

pub(crate) struct AttributionFilter<'a> {
    pub(crate) api_key_hash: Option<&'a str>,
    pub(crate) start_block: Option<i64>,
    pub(crate) end_block: Option<i64>,
    pub(crate) transaction_hash: Option<&'a str>,
}

pub(crate) struct ExecutionCursor<'a> {
    pub(crate) block: i64,
    pub(crate) log_index: i64,
    pub(crate) trade_id: &'a str,
}

pub(crate) async fn list_attributed_trades(
    pool: &DbPool,
    filter: AttributionFilter<'_>,
    cursor: Option<ExecutionCursor<'_>>,
    limit: u32,
) -> Result<Vec<AttributedTradeRow>, sqlx::Error> {
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT indexed_trade_id, chain_id, raindex_address, transaction_hash, log_index, block_number, \
                block_timestamp, order_hash, taker, api_key_hash, api_key_database_id, api_key_id, \
                api_key_label, api_key_owner, input_token, input_amount, output_token, output_amount \
         FROM attributed_trades WHERE 1 = 1",
    );
    push_filters(&mut query, filter);
    if let Some(cursor) = cursor {
        query
            .push(" AND (block_number < ")
            .push_bind(cursor.block)
            .push(" OR (block_number = ")
            .push_bind(cursor.block)
            .push(" AND log_index < ")
            .push_bind(cursor.log_index)
            .push(") OR (block_number = ")
            .push_bind(cursor.block)
            .push(" AND log_index = ")
            .push_bind(cursor.log_index)
            .push(" AND indexed_trade_id < ")
            .push_bind(cursor.trade_id)
            .push("))");
    }
    query
        .push(" ORDER BY block_number DESC, log_index DESC, indexed_trade_id DESC LIMIT ")
        .push_bind(i64::from(limit));
    query
        .build_query_as::<AttributedTradeRow>()
        .fetch_all(pool)
        .await
}

#[derive(Debug, FromRow)]
pub(crate) struct AttributionVolumeRow {
    pub(crate) chain_id: i64,
    pub(crate) api_key_hash: String,
    pub(crate) api_key_database_id: Option<i64>,
    pub(crate) api_key_id: Option<String>,
    pub(crate) api_key_label: Option<String>,
    pub(crate) api_key_owner: Option<String>,
    pub(crate) input_token: String,
    pub(crate) input_amount: String,
    pub(crate) output_token: String,
    pub(crate) output_amount: String,
}

pub(crate) struct VolumeCursor<'a> {
    pub(crate) api_key_hash: &'a str,
    pub(crate) chain_id: i64,
    pub(crate) input_token: &'a str,
    pub(crate) output_token: &'a str,
}

pub(crate) enum VisitRowsError<E> {
    Database(sqlx::Error),
    Visitor(E),
}

pub(crate) async fn visit_volume_rows<E, F>(
    pool: &DbPool,
    filter: AttributionFilter<'_>,
    cursor: Option<VolumeCursor<'_>>,
    mut visitor: F,
) -> Result<(), VisitRowsError<E>>
where
    F: FnMut(AttributionVolumeRow) -> Result<bool, E>,
{
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT trades.chain_id, trades.api_key_hash, \
                identity.api_key_database_id, identity.api_key_id, \
                identity.api_key_label, identity.api_key_owner, \
                trades.input_token, trades.input_amount, \
                trades.output_token, trades.output_amount \
         FROM attributed_trades AS trades \
         LEFT JOIN attribution_api_keys AS identity \
           ON identity.api_key_hash = trades.api_key_hash \
         WHERE 1 = 1",
    );
    push_volume_filters(&mut query, filter);
    if let Some(cursor) = cursor {
        query
            .push(" AND (trades.api_key_hash > ")
            .push_bind(cursor.api_key_hash)
            .push(" OR (trades.api_key_hash = ")
            .push_bind(cursor.api_key_hash)
            .push(" AND trades.chain_id > ")
            .push_bind(cursor.chain_id)
            .push(") OR (trades.api_key_hash = ")
            .push_bind(cursor.api_key_hash)
            .push(" AND trades.chain_id = ")
            .push_bind(cursor.chain_id)
            .push(" AND trades.input_token > ")
            .push_bind(cursor.input_token)
            .push(") OR (trades.api_key_hash = ")
            .push_bind(cursor.api_key_hash)
            .push(" AND trades.chain_id = ")
            .push_bind(cursor.chain_id)
            .push(" AND trades.input_token = ")
            .push_bind(cursor.input_token)
            .push(" AND trades.output_token > ")
            .push_bind(cursor.output_token)
            .push("))");
    }
    query.push(
        " ORDER BY trades.api_key_hash, trades.chain_id, \
                   trades.input_token, trades.output_token",
    );
    let mut rows = query.build_query_as::<AttributionVolumeRow>().fetch(pool);
    while let Some(row) = rows.try_next().await.map_err(VisitRowsError::Database)? {
        if !visitor(row).map_err(VisitRowsError::Visitor)? {
            break;
        }
    }
    Ok(())
}

fn push_volume_filters<'args>(
    query: &mut QueryBuilder<'args, Sqlite>,
    filter: AttributionFilter<'args>,
) {
    if let Some(api_key_hash) = filter.api_key_hash {
        query
            .push(" AND trades.api_key_hash = ")
            .push_bind(api_key_hash);
    }
    if let Some(start_block) = filter.start_block {
        query
            .push(" AND trades.block_number >= ")
            .push_bind(start_block);
    }
    if let Some(end_block) = filter.end_block {
        query
            .push(" AND trades.block_number <= ")
            .push_bind(end_block);
    }
    if let Some(transaction_hash) = filter.transaction_hash {
        query
            .push(" AND trades.transaction_hash = ")
            .push_bind(transaction_hash);
    }
}

fn push_filters<'args>(query: &mut QueryBuilder<'args, Sqlite>, filter: AttributionFilter<'args>) {
    if let Some(api_key_hash) = filter.api_key_hash {
        query.push(" AND api_key_hash = ").push_bind(api_key_hash);
    }
    if let Some(start_block) = filter.start_block {
        query.push(" AND block_number >= ").push_bind(start_block);
    }
    if let Some(end_block) = filter.end_block {
        query.push(" AND block_number <= ").push_bind(end_block);
    }
    if let Some(transaction_hash) = filter.transaction_hash {
        query
            .push(" AND transaction_hash = ")
            .push_bind(transaction_hash);
    }
}
