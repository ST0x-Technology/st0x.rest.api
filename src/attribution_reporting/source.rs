//! Read-only access to attribution inputs in the Raindex local database.

use crate::attribution::ATTRIBUTION_CONTEXT_WORDS;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};
use std::path::Path;
use std::time::Duration;

const FETCH_BATCH_SQL: &str = include_str!("fetch_batch.sql");
const SUPPORTED_RAINDEX_SCHEMA_VERSION: u32 = 5;
const _: () = assert!(
    rain_orderbook_app_settings::local_db_manifest::DB_SCHEMA_VERSION
        == SUPPORTED_RAINDEX_SCHEMA_VERSION,
    "Raindex local database schema changed; review the attribution queries"
);

#[derive(Debug, FromRow)]
pub(crate) struct SyncTarget {
    pub(crate) chain_id: i64,
    pub(crate) raindex_address: String,
    pub(crate) last_indexed_block: i64,
}

#[derive(Debug, Clone, FromRow)]
struct IndexedContextRow {
    chain_id: i64,
    raindex_address: String,
    trade_id: String,
    transaction_hash: String,
    log_index: i64,
    block_number: i64,
    block_timestamp: i64,
    transaction_sender: String,
    order_hash: String,
    input_token: String,
    input_delta: String,
    output_token: String,
    output_delta: String,
    context_index: Option<i64>,
    context_value: Option<String>,
    value_0: Option<String>,
    value_1: Option<String>,
    value_2: Option<String>,
    value_3: Option<String>,
    value_count: i64,
}

#[derive(Debug)]
pub(crate) struct IndexedTrade {
    pub(crate) chain_id: i64,
    pub(crate) raindex_address: String,
    pub(crate) trade_id: String,
    pub(crate) transaction_hash: String,
    pub(crate) log_index: i64,
    pub(crate) block_number: i64,
    pub(crate) block_timestamp: i64,
    pub(crate) transaction_sender: String,
    pub(crate) order_hash: String,
    pub(crate) input_token: String,
    pub(crate) input_delta: String,
    pub(crate) output_token: String,
    pub(crate) output_delta: String,
    pub(crate) contexts: Vec<IndexedSignedContext>,
}

#[derive(Debug)]
pub(crate) struct IndexedSignedContext {
    pub(crate) encoded_signer_and_signature: String,
    pub(crate) values: [String; ATTRIBUTION_CONTEXT_WORDS],
}

pub(crate) struct BatchCursor<'a> {
    pub(crate) block: i64,
    pub(crate) log_index: i64,
    pub(crate) trade_id: &'a str,
}

pub(crate) async fn open_pool(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .busy_timeout(Duration::from_secs(5));
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
}

pub(crate) async fn list_targets(
    pool: &SqlitePool,
    chain_id: u32,
) -> Result<Vec<SyncTarget>, sqlx::Error> {
    sqlx::query_as::<_, SyncTarget>(
        "SELECT chain_id, raindex_address, last_block AS last_indexed_block \
         FROM target_watermarks WHERE chain_id = ? ORDER BY raindex_address",
    )
    .bind(i64::from(chain_id))
    .fetch_all(pool)
    .await
}

pub(crate) async fn refresh_watermark(
    transaction: &mut Transaction<'_, Sqlite>,
    target: &SyncTarget,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT last_block FROM target_watermarks \
         WHERE chain_id = ? AND raindex_address = ?",
    )
    .bind(target.chain_id)
    .bind(&target.raindex_address)
    .fetch_optional(&mut **transaction)
    .await
}

pub(crate) async fn fetch_batch(
    transaction: &mut Transaction<'_, Sqlite>,
    target: &SyncTarget,
    start_block: i64,
    cursor: Option<BatchCursor<'_>>,
    batch_size: u32,
) -> Result<Vec<IndexedTrade>, sqlx::Error> {
    let cursor_block = cursor.as_ref().map_or(0, |cursor| cursor.block);
    let cursor_log_index = cursor.as_ref().map_or(-1, |cursor| cursor.log_index);
    let cursor_trade_id = cursor.as_ref().map_or("", |cursor| cursor.trade_id);
    let rows = sqlx::query_as::<_, IndexedContextRow>(FETCH_BATCH_SQL)
        .bind(target.chain_id)
        .bind(&target.raindex_address)
        .bind(start_block)
        .bind(target.last_indexed_block)
        .bind(cursor.is_some())
        .bind(cursor_block)
        .bind(cursor_block)
        .bind(cursor_log_index)
        .bind(cursor_block)
        .bind(cursor_log_index)
        .bind(cursor_trade_id)
        .bind(i64::from(batch_size))
        .fetch_all(&mut **transaction)
        .await?;
    Ok(group_context_rows(rows))
}

fn group_context_rows(rows: Vec<IndexedContextRow>) -> Vec<IndexedTrade> {
    let mut trades: Vec<IndexedTrade> = Vec::new();
    for row in rows {
        let is_new_trade = trades.last().is_none_or(|trade| {
            trade.chain_id != row.chain_id
                || trade.raindex_address != row.raindex_address
                || trade.trade_id != row.trade_id
        });
        if is_new_trade {
            trades.push(IndexedTrade {
                chain_id: row.chain_id,
                raindex_address: row.raindex_address.clone(),
                trade_id: row.trade_id.clone(),
                transaction_hash: row.transaction_hash.clone(),
                log_index: row.log_index,
                block_number: row.block_number,
                block_timestamp: row.block_timestamp,
                transaction_sender: row.transaction_sender.clone(),
                order_hash: row.order_hash.clone(),
                input_token: row.input_token.clone(),
                input_delta: row.input_delta.clone(),
                output_token: row.output_token.clone(),
                output_delta: row.output_delta.clone(),
                contexts: Vec::new(),
            });
        }

        let values = match (
            row.value_0,
            row.value_1,
            row.value_2,
            row.value_3,
            row.value_count,
        ) {
            (Some(v0), Some(v1), Some(v2), Some(v3), 4) => Some([v0, v1, v2, v3]),
            _ => None,
        };
        if row.context_index.is_some() {
            if let (Some(context_value), Some(values), Some(trade)) =
                (row.context_value, values, trades.last_mut())
            {
                trade.contexts.push(IndexedSignedContext {
                    encoded_signer_and_signature: context_value,
                    values,
                });
            }
        }
    }
    trades
}
