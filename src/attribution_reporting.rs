//! Background attribution of confirmed trades indexed by the local Raindex database.

use crate::attribution::{
    compute_api_key_hash, verify_signed_attribution, ATTRIBUTION_CONTEXT_WORDS,
};
use crate::db::DbPool;
use alloy::primitives::{Address, Bytes, B256};
use rain_orderbook_bindings::IRaindexV6::SignedContextV1;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::{Orbit, Rocket};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const SUPPORTED_RAINDEX_SCHEMA_VERSION: u32 = 5;
const WORKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const _: () = assert!(
    rain_orderbook_app_settings::local_db_manifest::DB_SCHEMA_VERSION
        == SUPPORTED_RAINDEX_SCHEMA_VERSION,
    "Raindex local database schema changed; review the attribution queries"
);

#[derive(Debug, Clone)]
pub(crate) struct AttributionWorker {
    app_pool: DbPool,
    raindex_db_path: PathBuf,
    signer: Address,
    start_block: u64,
    interval: Duration,
    batch_size: u32,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl AttributionWorker {
    pub(crate) fn new(
        app_pool: DbPool,
        raindex_db_path: PathBuf,
        signer: Address,
        start_block: u64,
        interval: Duration,
        batch_size: u32,
    ) -> Self {
        Self {
            app_pool,
            raindex_db_path,
            signer,
            start_block,
            interval,
            batch_size: batch_size.max(1),
            task: Arc::new(Mutex::new(None)),
        }
    }
}

#[rocket::async_trait]
impl Fairing for AttributionWorker {
    fn info(&self) -> Info {
        Info {
            name: "confirmed trade attribution worker",
            kind: Kind::Liftoff | Kind::Shutdown,
        }
    }

    async fn on_liftoff(&self, rocket: &Rocket<Orbit>) {
        if let Err(error) = record_attribution_signer(&self.app_pool, self.signer).await {
            tracing::error!(%error, "failed to record current attribution signer at startup");
        }
        let worker = self.clone();
        let mut shutdown = rocket.shutdown();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(worker.interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = &mut shutdown => {
                        tracing::info!("attribution worker shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                    }
                }

                let sync = run_sync_iteration(&worker);
                tokio::select! {
                    _ = &mut shutdown => {
                        tracing::info!("cancelling in-flight attribution sync during shutdown");
                        break;
                    }
                    _ = sync => {}
                }
            }
        });
        *self.task.lock().await = Some(handle);
    }

    async fn on_shutdown(&self, _rocket: &Rocket<Orbit>) {
        let handle = self.task.lock().await.take();
        if let Some(handle) = handle {
            finish_worker_task(handle, WORKER_SHUTDOWN_TIMEOUT).await;
        }
    }
}

async fn finish_worker_task(mut handle: JoinHandle<()>, timeout: Duration) {
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::error!(%error, "attribution worker task failed during shutdown");
        }
        Err(_) => {
            tracing::warn!("aborting attribution worker after shutdown timeout");
            handle.abort();
            if let Err(error) = handle.await {
                if !error.is_cancelled() {
                    tracing::error!(%error, "attribution worker abort failed");
                }
            }
        }
    }
}

async fn run_sync_iteration(worker: &AttributionWorker) {
    match open_raindex_pool(&worker.raindex_db_path).await {
        Ok(source_pool) => {
            let result = process_available_trades(
                &worker.app_pool,
                &source_pool,
                worker.signer,
                worker.start_block,
                worker.batch_size,
            )
            .await;
            source_pool.close().await;
            if let Err(error) = result {
                tracing::error!(error = %error, "attribution sync failed");
            }
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                path = %worker.raindex_db_path.display(),
                "attribution source database is not ready"
            );
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AttributionReportingError {
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("attribution start block does not fit in SQLite INTEGER")]
    StartBlockOverflow,
}

async fn open_raindex_pool(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .busy_timeout(Duration::from_secs(5));
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
}

#[derive(Debug, FromRow)]
struct SyncTarget {
    chain_id: i64,
    raindex_address: String,
    last_indexed_block: i64,
}

#[derive(Debug, FromRow)]
struct CursorRow {
    start_block: i64,
    last_block: i64,
    last_log_index: i64,
    last_trade_id: String,
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
struct IndexedTrade {
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
    contexts: Vec<IndexedSignedContext>,
}

#[derive(Debug)]
struct IndexedSignedContext {
    encoded_signer_and_signature: String,
    values: [String; ATTRIBUTION_CONTEXT_WORDS],
}

#[derive(Debug, Clone, FromRow)]
struct ApiKeyIdentity {
    id: i64,
    key_id: String,
    label: String,
    owner: String,
}

pub(crate) async fn process_available_trades(
    app_pool: &DbPool,
    source_pool: &SqlitePool,
    signer: Address,
    start_block: u64,
    batch_size: u32,
) -> Result<u64, AttributionReportingError> {
    let start_block =
        i64::try_from(start_block).map_err(|_| AttributionReportingError::StartBlockOverflow)?;
    record_attribution_signer(app_pool, signer).await?;
    let targets = sqlx::query_as::<_, SyncTarget>(
        "SELECT chain_id, raindex_address, last_block AS last_indexed_block \
         FROM target_watermarks WHERE chain_id = ? ORDER BY raindex_address",
    )
    .bind(i64::from(crate::CHAIN_ID))
    .fetch_all(source_pool)
    .await?;
    snapshot_current_api_keys(app_pool).await?;
    let identities = load_api_key_identities(app_pool).await?;
    let identity_by_hash: HashMap<B256, ApiKeyIdentity> = identities
        .into_iter()
        .map(|identity| (compute_api_key_hash(&identity.key_id), identity))
        .collect();

    let mut attributed_count = 0;
    let trusted_signers = load_attribution_signers(app_pool).await?;
    for mut target in targets {
        let mut source_transaction = source_pool.begin().await?;
        let Some(last_indexed_block) = sqlx::query_scalar::<_, i64>(
            "SELECT last_block FROM target_watermarks \
             WHERE chain_id = ? AND raindex_address = ?",
        )
        .bind(target.chain_id)
        .bind(&target.raindex_address)
        .fetch_optional(&mut *source_transaction)
        .await?
        else {
            source_transaction.commit().await?;
            continue;
        };
        target.last_indexed_block = last_indexed_block;
        attributed_count += process_target(
            app_pool,
            &mut source_transaction,
            &trusted_signers,
            start_block,
            batch_size.max(1),
            &target,
            &identity_by_hash,
        )
        .await?;
        source_transaction.commit().await?;
    }
    Ok(attributed_count)
}

async fn load_api_key_identities(app_pool: &DbPool) -> Result<Vec<ApiKeyIdentity>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyIdentity>(
        "SELECT api_key_database_id AS id, api_key_id AS key_id, \
                api_key_label AS label, api_key_owner AS owner \
         FROM attribution_api_keys ORDER BY api_key_id",
    )
    .fetch_all(app_pool)
    .await
}

async fn snapshot_current_api_keys(app_pool: &DbPool) -> Result<(), sqlx::Error> {
    let identities = sqlx::query_as::<_, ApiKeyIdentity>(
        "SELECT id, key_id, label, owner FROM api_keys ORDER BY id",
    )
    .fetch_all(app_pool)
    .await?;
    let mut transaction = app_pool.begin().await?;
    for identity in identities {
        upsert_api_key_identity(&mut transaction, &identity).await?;
    }
    transaction.commit().await
}

async fn upsert_api_key_identity(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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

pub(crate) async fn snapshot_api_key(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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

async fn record_attribution_signer(app_pool: &DbPool, signer: Address) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO attribution_signers (address) VALUES (?) \
         ON CONFLICT(address) DO NOTHING",
    )
    .bind(signer.to_string())
    .execute(app_pool)
    .await?;
    Ok(())
}

async fn load_attribution_signers(app_pool: &DbPool) -> Result<Vec<Address>, sqlx::Error> {
    let values: Vec<String> =
        sqlx::query_scalar("SELECT address FROM attribution_signers ORDER BY first_seen_at")
            .fetch_all(app_pool)
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

async fn process_target(
    app_pool: &DbPool,
    source_transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    trusted_signers: &[Address],
    configured_start_block: i64,
    batch_size: u32,
    target: &SyncTarget,
    identities: &HashMap<B256, ApiKeyIdentity>,
) -> Result<u64, AttributionReportingError> {
    // The cursor read, attributed-trade upserts, and cursor update share one transaction.
    // A concurrent worker therefore cannot commit a cursor derived from stale progress.
    let mut transaction = app_pool.begin().await?;
    let mut stored_cursor = sqlx::query_as::<_, CursorRow>(
        "SELECT start_block, last_block, last_log_index, last_trade_id \
         FROM attribution_sync_cursors WHERE chain_id = ? AND raindex_address = ?",
    )
    .bind(target.chain_id)
    .bind(&target.raindex_address)
    .fetch_optional(&mut *transaction)
    .await?;

    let start_block_reset = stored_cursor
        .as_ref()
        .is_some_and(|cursor| cursor.start_block != configured_start_block);
    if start_block_reset {
        reset_target_start_block(&mut transaction, target, configured_start_block).await?;
        stored_cursor = None;
    }
    let cursor = stored_cursor;
    let last_block = cursor.as_ref().map_or(0, |cursor| cursor.last_block);
    let last_log_index = cursor.as_ref().map_or(-1, |cursor| cursor.last_log_index);
    let last_trade_id = cursor
        .as_ref()
        .map_or_else(String::new, |cursor| cursor.last_trade_id.clone());
    let has_cursor = cursor.is_some();
    let mut attributed_count = 0;

    let rows = fetch_batch(
        source_transaction,
        target,
        configured_start_block,
        has_cursor,
        last_block,
        last_log_index,
        &last_trade_id,
        batch_size,
    )
    .await?;
    let trades = group_context_rows(rows);
    // Raindex materializes all derived rows through a target's watermark before advancing
    // target_watermarks in the same source transaction. Because this function rereads the
    // watermark and the rows from one source snapshot, a partial batch proves that every
    // trade through last_indexed_block has been observed and can safely use the MAX sentinel.
    let next_cursor = if trades.len() < batch_size as usize {
        (target.last_indexed_block, i64::MAX, String::new())
    } else {
        let last_trade = trades
            .last()
            .ok_or_else(|| sqlx::Error::Protocol("non-empty attribution batch expected".into()))?;
        (
            last_trade.block_number,
            last_trade.log_index,
            last_trade.trade_id.clone(),
        )
    };

    for trade in &trades {
        let Some(api_key_hash) = trade
            .contexts
            .iter()
            .find_map(|context| verify_context(context, trade, trusted_signers))
        else {
            continue;
        };
        let identity = identities.get(&api_key_hash);
        sqlx::query(
            "INSERT INTO attributed_trades (\
                    chain_id, raindex_address, indexed_trade_id, transaction_hash, log_index, \
                    block_number, block_timestamp, order_hash, taker, api_key_hash, \
                    api_key_database_id, api_key_id, api_key_label, api_key_owner, \
                    input_token, input_amount, \
                    output_token, output_amount\
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
        .bind(&trade.trade_id)
        .bind(&trade.transaction_hash)
        .bind(trade.log_index)
        .bind(trade.block_number)
        .bind(trade.block_timestamp)
        .bind(&trade.order_hash)
        .bind(&trade.transaction_sender)
        .bind(api_key_hash.to_string())
        .bind(identity.map(|value| value.id))
        .bind(identity.map(|value| value.key_id.as_str()))
        .bind(identity.map(|value| value.label.as_str()))
        .bind(identity.map(|value| value.owner.as_str()))
        .bind(&trade.input_token)
        .bind(&trade.input_delta)
        .bind(&trade.output_token)
        .bind(&trade.output_delta)
        .execute(&mut *transaction)
        .await?;
        attributed_count += 1;
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
    .bind(target.chain_id)
    .bind(&target.raindex_address)
    .bind(configured_start_block)
    .bind(next_cursor.0)
    .bind(next_cursor.1)
    .bind(&next_cursor.2)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    if start_block_reset {
        tracing::warn!(
            chain_id = target.chain_id,
            raindex_address = %target.raindex_address,
            configured_start_block,
            "attribution start block changed; target cursor reset"
        );
    }
    if attributed_count > 0 {
        tracing::info!(
            chain_id = target.chain_id,
            raindex_address = %target.raindex_address,
            attributed_count,
            last_indexed_block = target.last_indexed_block,
            "confirmed trades attributed"
        );
    }
    Ok(attributed_count)
}

async fn reset_target_start_block(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target: &SyncTarget,
    configured_start_block: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM attributed_trades \
         WHERE chain_id = ? AND raindex_address = ? AND block_number < ?",
    )
    .bind(target.chain_id)
    .bind(&target.raindex_address)
    .bind(configured_start_block)
    .execute(&mut **transaction)
    .await?;
    sqlx::query("DELETE FROM attribution_sync_cursors WHERE chain_id = ? AND raindex_address = ?")
        .bind(target.chain_id)
        .bind(&target.raindex_address)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn fetch_batch(
    source_transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target: &SyncTarget,
    start_block: i64,
    has_cursor: bool,
    last_block: i64,
    last_log_index: i64,
    last_trade_id: &str,
    batch_size: u32,
) -> Result<Vec<IndexedContextRow>, sqlx::Error> {
    sqlx::query_as::<_, IndexedContextRow>(
        "WITH batch AS (\
            SELECT chain_id, raindex_address, trade_id, transaction_hash, log_index, \
                   block_number, block_timestamp, transaction_sender, order_hash, \
                   input_token, input_delta, output_token, output_delta \
            FROM derived_trades \
            WHERE trade_kind = 'take' \
              AND chain_id = ? AND raindex_address = ? \
              AND block_number >= ? AND block_number <= ? \
              AND (? = 0 OR block_number > ? \
                   OR (block_number = ? AND log_index > ?) \
                   OR (block_number = ? AND log_index = ? AND trade_id > ?)) \
            ORDER BY block_number, log_index, trade_id \
            LIMIT ?\
         ) \
         SELECT b.*, c.context_index, c.context_value, \
                MAX(CASE WHEN v.value_index = 0 THEN v.value END) AS value_0, \
                MAX(CASE WHEN v.value_index = 1 THEN v.value END) AS value_1, \
                MAX(CASE WHEN v.value_index = 2 THEN v.value END) AS value_2, \
                MAX(CASE WHEN v.value_index = 3 THEN v.value END) AS value_3, \
                COUNT(v.value_index) AS value_count \
         FROM batch b \
         LEFT JOIN take_order_contexts c \
           ON c.chain_id = b.chain_id \
          AND c.raindex_address = b.raindex_address \
          AND c.transaction_hash = b.transaction_hash \
          AND c.log_index = b.log_index \
         LEFT JOIN context_values v \
           ON v.chain_id = c.chain_id \
          AND v.raindex_address = c.raindex_address \
          AND v.transaction_hash = c.transaction_hash \
          AND v.log_index = c.log_index \
          AND v.context_index = c.context_index \
         GROUP BY b.chain_id, b.raindex_address, b.trade_id, b.transaction_hash, b.log_index, \
                  b.block_number, b.block_timestamp, b.transaction_sender, b.order_hash, \
                  b.input_token, b.input_delta, b.output_token, b.output_delta, \
                  c.context_index, c.context_value \
         ORDER BY b.block_number, b.log_index, b.trade_id, c.context_index",
    )
    .bind(target.chain_id)
    .bind(&target.raindex_address)
    .bind(start_block)
    .bind(target.last_indexed_block)
    .bind(has_cursor)
    .bind(last_block)
    .bind(last_block)
    .bind(last_log_index)
    .bind(last_block)
    .bind(last_log_index)
    .bind(last_trade_id)
    .bind(i64::from(batch_size))
    .fetch_all(&mut **source_transaction)
    .await
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

fn verify_context(
    candidate: &IndexedSignedContext,
    trade: &IndexedTrade,
    trusted_signers: &[Address],
) -> Option<B256> {
    let (signer, signature) = parse_signer_and_signature(&candidate.encoded_signer_and_signature)?;
    let context = candidate
        .values
        .iter()
        .map(|value| B256::from_str(value).ok())
        .collect::<Option<Vec<_>>>()?;
    // RaindexV6 records TakeOrderV3.sender (the immediate orderbook caller) as
    // transaction_sender. Generated attribution deliberately binds that same caller as the
    // taker. A relayer must request calldata with its own calling address; forwarding context
    // signed for a different EOA is rejected instead of being attributed to the wrong customer.
    let taker = Address::from_str(&trade.transaction_sender).ok()?;
    let order_hash = B256::from_str(&trade.order_hash).ok()?;
    if !trusted_signers.contains(&signer) {
        return None;
    }
    verify_signed_attribution(
        &SignedContextV1 {
            signer,
            context,
            signature,
        },
        signer,
        taker,
        order_hash,
    )
}

fn parse_signer_and_signature(value: &str) -> Option<(Address, Bytes)> {
    let value = value.strip_prefix("signer:")?;
    let (signer, signature) = value.split_once(",signature:")?;
    let signer = Address::from_str(signer).ok()?;
    let signature = signature.strip_prefix("0x").unwrap_or(signature);
    let bytes = alloy::hex::decode(signature).ok()?;
    Some((signer, Bytes::from(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{Attribution, AttributionSigner};
    use alloy::primitives::address;
    use rain_math_float::Float;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::NamedTempFile;

    const TEST_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const ORDER_HASH: B256 = B256::new([0x33; 32]);
    const TAKER: Address = address!("A9d6ab42f32dC476269dB2407715D8c15A36D781");

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    async fn source_pool() -> SqlitePool {
        let file = NamedTempFile::new().expect("source file");
        let path = file.into_temp_path().keep().expect("persist source file");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true),
            )
            .await
            .expect("source pool");
        sqlx::raw_sql(rain_orderbook_common::local_db::query::create_tables::create_tables_sql())
            .execute(&pool)
            .await
            .expect("source schema");
        pool
    }

    async fn app_pool() -> DbPool {
        crate::db::init(
            &format!(
                "sqlite:file:{}?mode=memory&cache=shared",
                uuid::Uuid::new_v4()
            ),
            2,
        )
        .await
        .expect("app pool")
    }

    #[tokio::test]
    async fn shutdown_timeout_aborts_a_stuck_worker() {
        let dropped = Arc::new(AtomicBool::new(false));
        let task_flag = Arc::clone(&dropped);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _drop_flag = DropFlag(task_flag);
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.expect("worker started");

        finish_worker_task(handle, Duration::from_millis(1)).await;

        assert!(dropped.load(Ordering::SeqCst));
    }

    async fn seed_trade_at(
        source: &SqlitePool,
        key_id: &str,
        trade_id: &str,
        transaction_hash: &str,
        log_index: i64,
        block_number: i64,
        watermark: i64,
    ) {
        let signer = AttributionSigner::from_hex_key(TEST_KEY).expect("signer");
        let attribution = Attribution {
            api_key_hash: compute_api_key_hash(key_id),
            taker: TAKER,
        };
        let signed = signer
            .sign_context(&attribution, ORDER_HASH)
            .await
            .expect("signed context");
        let input = Float::parse("0.001".to_string())
            .expect("input float")
            .as_hex();
        let output = Float::parse("-0.205184".to_string())
            .expect("output float")
            .as_hex();
        let raindex = "0x1111111111111111111111111111111111111111";
        let tx_hash = transaction_hash.to_string();
        // Match Raindex ingestion: derived rows and their watermark become visible atomically,
        // with the watermark written only after all rows through that block are materialized.
        let mut transaction = source.begin().await.expect("source transaction");
        sqlx::query(
            "INSERT INTO derived_trades (\
                chain_id, raindex_address, trade_id, trade_kind, trade_side, \
                order_hash, order_owner, order_nonce, transaction_hash, log_index, \
                block_number, block_timestamp, transaction_sender, input_vault_id, \
                input_token, input_delta, output_vault_id, output_token, output_delta\
             ) VALUES (?, ?, ?, 'take', 'sell', ?, ?, '0', ?, ?, ?, ?, ?, '0', ?, ?, '0', ?, ?)",
        )
        .bind(i64::from(crate::CHAIN_ID))
        .bind(raindex)
        .bind(trade_id)
        .bind(ORDER_HASH.to_string())
        .bind(signer.address().to_string())
        .bind(&tx_hash)
        .bind(log_index)
        .bind(block_number)
        .bind(1_753_000_000_i64)
        .bind(TAKER.to_string())
        .bind("0x2222222222222222222222222222222222222222")
        .bind(input)
        .bind("0x3333333333333333333333333333333333333333")
        .bind(output)
        .execute(&mut *transaction)
        .await
        .expect("trade");
        let context_value = format!(
            "signer:{},signature:{}",
            signer.address(),
            alloy::hex::encode_prefixed(signed.signature)
        );
        sqlx::query(
            "INSERT INTO take_orders (\
                chain_id, raindex_address, transaction_hash, log_index, block_number, \
                block_timestamp, sender, order_owner, order_nonce, input_io_index, \
                output_io_index, taker_input, taker_output\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, '0', 0, 0, ?, ?)",
        )
        .bind(i64::from(crate::CHAIN_ID))
        .bind(raindex)
        .bind(&tx_hash)
        .bind(log_index)
        .bind(block_number)
        .bind(1_753_000_000_i64)
        .bind(TAKER.to_string())
        .bind(signer.address().to_string())
        .bind("0.001")
        .bind("0.205184")
        .execute(&mut *transaction)
        .await
        .expect("take order");
        sqlx::query(
            "INSERT INTO take_order_contexts (\
                chain_id, raindex_address, transaction_hash, log_index, \
                context_index, context_value\
             ) VALUES (?, ?, ?, ?, 0, ?)",
        )
        .bind(i64::from(crate::CHAIN_ID))
        .bind(raindex)
        .bind(&tx_hash)
        .bind(log_index)
        .bind(context_value)
        .execute(&mut *transaction)
        .await
        .expect("context");
        for (index, value) in signed.context.iter().enumerate() {
            sqlx::query(
                "INSERT INTO context_values (\
                    chain_id, raindex_address, transaction_hash, log_index, \
                    context_index, value_index, value\
                 ) VALUES (?, ?, ?, ?, 0, ?, ?)",
            )
            .bind(i64::from(crate::CHAIN_ID))
            .bind(raindex)
            .bind(&tx_hash)
            .bind(log_index)
            .bind(i64::try_from(index).expect("context index"))
            .bind(value.to_string())
            .execute(&mut *transaction)
            .await
            .expect("context value");
        }
        sqlx::query(
            "INSERT INTO target_watermarks (chain_id, raindex_address, last_block) \
             VALUES (?, ?, ?) \
             ON CONFLICT(chain_id, raindex_address) DO UPDATE SET last_block = excluded.last_block",
        )
        .bind(i64::from(crate::CHAIN_ID))
        .bind(raindex)
        .bind(watermark)
        .execute(&mut *transaction)
        .await
        .expect("sync target");
        transaction.commit().await.expect("source commit");
    }

    async fn seed_trade(source: &SqlitePool, key_id: &str) {
        seed_trade_at(
            source,
            key_id,
            "0x01",
            "0x48f6ed8a67769c007491262e72b78eb934a0a53bc0d67506081fc9a1c35c276f",
            7,
            48_963_501,
            48_963_501,
        )
        .await;
    }

    #[tokio::test]
    async fn attributes_confirmed_trade_and_advances_cursor_idempotently() {
        let app = app_pool().await;
        let source = source_pool().await;
        let key_id = "customer-key";
        sqlx::query(
            "INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, 'hash', ?, ?)",
        )
        .bind(key_id)
        .bind("Customer")
        .bind("customer@example.com")
        .execute(&app)
        .await
        .expect("API key");
        seed_trade(&source, key_id).await;
        let signer = AttributionSigner::from_hex_key(TEST_KEY).expect("signer");

        let first = process_available_trades(&app, &source, signer.address(), 48_963_501, 100)
            .await
            .expect("first sync");
        let second = process_available_trades(&app, &source, signer.address(), 48_963_501, 100)
            .await
            .expect("second sync");
        assert_eq!(first, 1);
        assert_eq!(second, 0);

        let row: (String, String, String, String) = sqlx::query_as(
            "SELECT transaction_hash, api_key_id, api_key_label, api_key_owner \
             FROM attributed_trades",
        )
        .fetch_one(&app)
        .await
        .expect("attributed trade");
        assert_eq!(
            row.0,
            "0x48f6ed8a67769c007491262e72b78eb934a0a53bc0d67506081fc9a1c35c276f"
        );
        assert_eq!(row.1, key_id);
        assert_eq!(row.2, "Customer");
        assert_eq!(row.3, "customer@example.com");

        let cursor: (i64, i64) =
            sqlx::query_as("SELECT start_block, last_block FROM attribution_sync_cursors")
                .fetch_one(&app)
                .await
                .expect("cursor");
        assert_eq!(cursor, (48_963_501, 48_963_501));

        process_available_trades(&app, &source, signer.address(), 48_963_502, 100)
            .await
            .expect("raise start block");
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attributed_trades")
            .fetch_one(&app)
            .await
            .expect("remaining attributed trades");
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn ignores_trade_before_configured_start_block() {
        let app = app_pool().await;
        let source = source_pool().await;
        seed_trade(&source, "customer-key").await;
        let signer = AttributionSigner::from_hex_key(TEST_KEY).expect("signer");

        let count = process_available_trades(&app, &source, signer.address(), 48_963_502, 100)
            .await
            .expect("sync");
        assert_eq!(count, 0);
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attributed_trades")
            .fetch_one(&app)
            .await
            .expect("count");
        assert_eq!(stored, 0);
        let checkpoint: (i64, i64) =
            sqlx::query_as("SELECT last_block, last_log_index FROM attribution_sync_cursors")
                .fetch_one(&app)
                .await
                .expect("empty range checkpoint");
        assert_eq!(checkpoint, (48_963_501, i64::MAX));

        process_available_trades(&app, &source, signer.address(), 48_963_503, 100)
            .await
            .expect("changed start block resets the derived reporting cursor");
        let reset_start: i64 =
            sqlx::query_scalar("SELECT start_block FROM attribution_sync_cursors")
                .fetch_one(&app)
                .await
                .expect("reset cursor");
        assert_eq!(reset_start, 48_963_503);
    }

    #[tokio::test]
    async fn partial_batch_advances_to_complete_source_watermark() {
        let app = app_pool().await;
        let source = source_pool().await;
        let key_id = "customer-key";
        sqlx::query(
            "INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, 'hash', '', '')",
        )
        .bind(key_id)
        .execute(&app)
        .await
        .expect("API key");
        seed_trade_at(
            &source,
            key_id,
            "0x01",
            "0x0000000000000000000000000000000000000000000000000000000000000001",
            7,
            48_963_501,
            48_963_502,
        )
        .await;
        let signer = AttributionSigner::from_hex_key(TEST_KEY).expect("signer");

        let count = process_available_trades(&app, &source, signer.address(), 48_963_501, 100)
            .await
            .expect("partial batch");
        assert_eq!(count, 1);
        let cursor: (i64, i64) =
            sqlx::query_as("SELECT last_block, last_log_index FROM attribution_sync_cursors")
                .fetch_one(&app)
                .await
                .expect("watermark cursor");
        assert_eq!(cursor, (48_963_502, i64::MAX));

        seed_trade_at(
            &source,
            key_id,
            "0x02",
            "0x0000000000000000000000000000000000000000000000000000000000000002",
            1,
            48_963_503,
            48_963_503,
        )
        .await;
        let next = process_available_trades(&app, &source, signer.address(), 48_963_501, 100)
            .await
            .expect("next complete watermark");
        assert_eq!(next, 1);
    }

    #[tokio::test]
    async fn attributed_rows_and_cursor_commit_atomically() {
        let app = app_pool().await;
        let source = source_pool().await;
        let key_id = "customer-key";
        sqlx::query(
            "INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, 'hash', '', '')",
        )
        .bind(key_id)
        .execute(&app)
        .await
        .expect("API key");
        seed_trade(&source, key_id).await;
        sqlx::query(
            "CREATE TRIGGER reject_attribution_cursor \
             BEFORE INSERT ON attribution_sync_cursors \
             BEGIN SELECT RAISE(FAIL, 'cursor rejected'); END",
        )
        .execute(&app)
        .await
        .expect("cursor trigger");
        let signer = AttributionSigner::from_hex_key(TEST_KEY).expect("signer");

        let result =
            process_available_trades(&app, &source, signer.address(), 48_963_501, 100).await;
        assert!(result.is_err());
        let trade_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attributed_trades")
            .fetch_one(&app)
            .await
            .expect("trade count");
        let cursor_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attribution_sync_cursors")
            .fetch_one(&app)
            .await
            .expect("cursor count");
        assert_eq!((trade_count, cursor_count), (0, 0));
    }

    #[tokio::test]
    async fn rejects_context_signed_for_a_different_orderbook_caller() {
        let app = app_pool().await;
        let source = source_pool().await;
        let key_id = "customer-key";
        sqlx::query(
            "INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, 'hash', '', '')",
        )
        .bind(key_id)
        .execute(&app)
        .await
        .expect("API key");
        seed_trade(&source, key_id).await;
        sqlx::query("UPDATE derived_trades SET transaction_sender = ?")
            .bind(Address::from([0x55; 20]).to_string())
            .execute(&source)
            .await
            .expect("relayed caller");
        let signer = AttributionSigner::from_hex_key(TEST_KEY).expect("signer");

        let count = process_available_trades(&app, &source, signer.address(), 48_963_501, 100)
            .await
            .expect("sync");
        assert_eq!(count, 0);
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attributed_trades")
            .fetch_one(&app)
            .await
            .expect("attributed count");
        assert_eq!(stored, 0);
    }

    #[tokio::test]
    async fn full_batches_resume_without_skipping_shared_event_positions() {
        let app = app_pool().await;
        let source = source_pool().await;
        let key_id = "customer-key";
        sqlx::query(
            "INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, 'hash', '', '')",
        )
        .bind(key_id)
        .execute(&app)
        .await
        .expect("API key");
        seed_trade_at(
            &source,
            key_id,
            "0x01",
            "0x0000000000000000000000000000000000000000000000000000000000000001",
            7,
            48_963_501,
            48_963_502,
        )
        .await;
        seed_trade_at(
            &source,
            key_id,
            "0x02",
            "0x0000000000000000000000000000000000000000000000000000000000000002",
            7,
            48_963_501,
            48_963_502,
        )
        .await;
        seed_trade_at(
            &source,
            key_id,
            "0x03",
            "0x0000000000000000000000000000000000000000000000000000000000000003",
            1,
            48_963_502,
            48_963_502,
        )
        .await;
        let signer = AttributionSigner::from_hex_key(TEST_KEY).expect("signer");

        let first = process_available_trades(&app, &source, signer.address(), 48_963_501, 2)
            .await
            .expect("first batch");
        let second = process_available_trades(&app, &source, signer.address(), 48_963_501, 2)
            .await
            .expect("second batch");
        let third = process_available_trades(&app, &source, signer.address(), 48_963_501, 2)
            .await
            .expect("empty tail");
        assert_eq!((first, second, third), (2, 1, 0));

        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attributed_trades")
            .fetch_one(&app)
            .await
            .expect("attributed count");
        assert_eq!(stored, 3);
        let cursor: (i64, i64) =
            sqlx::query_as("SELECT last_block, last_log_index FROM attribution_sync_cursors")
                .fetch_one(&app)
                .await
                .expect("final cursor");
        assert_eq!(cursor, (48_963_502, i64::MAX));
    }

    #[tokio::test]
    async fn trusts_signers_recorded_before_key_rotation() {
        let app = app_pool().await;
        let source = source_pool().await;
        let key_id = "customer-key";
        sqlx::query(
            "INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, 'hash', '', '')",
        )
        .bind(key_id)
        .execute(&app)
        .await
        .expect("API key");
        let old_signer = AttributionSigner::from_hex_key(TEST_KEY).expect("old signer");
        process_available_trades(&app, &source, old_signer.address(), 48_963_501, 100)
            .await
            .expect("record old signer");
        seed_trade(&source, key_id).await;

        let new_signer = Address::from([0x44; 20]);
        let count = process_available_trades(&app, &source, new_signer, 48_963_501, 100)
            .await
            .expect("process after rotation");
        assert_eq!(count, 1);
        let signer_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attribution_signers")
            .fetch_one(&app)
            .await
            .expect("trusted signer count");
        assert_eq!(signer_count, 2);
    }
}
