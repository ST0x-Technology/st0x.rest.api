/// Direct SQLite trade fetcher
///
/// Bypasses the rain.orderbook library's per-query connection model by
/// running batch SQL queries for multiple order hashes in one call instead
/// of N individual queries that each open their own connection.
///
/// Opens a fresh read-only connection per query so that concurrent API
/// requests can read in parallel under SQLite WAL mode without blocking
/// each other or the background sync writer.
use crate::error::ApiError;
use crate::types::order::OrderTradeEntry;
use alloy::primitives::{Address, B256};
use rain_math_float::Float;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Instant;
use tokio::task::spawn_blocking;

/// Holds configuration for opening read-only SQLite connections to the
/// raindex local database. Each query opens its own connection so that
/// concurrent readers never block each other (SQLite WAL allows this).
pub(crate) struct DirectTradesFetcher {
    db_path: PathBuf,
    chain_id: i64,
    orderbook_address: String,
}

/// Open a read-only connection with WAL mode and appropriate timeouts.
fn open_read_connection(db_path: &Path) -> Result<Connection, ApiError> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| {
        tracing::error!(error = %e, "failed to open raindex db for reading");
        ApiError::Internal("trade query failed".into())
    })?;

    conn.pragma_update(None, "journal_mode", "wal")
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set WAL");
            ApiError::Internal("trade query failed".into())
        })?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set busy_timeout");
            ApiError::Internal("trade query failed".into())
        })?;

    Ok(conn)
}

impl DirectTradesFetcher {
    pub(crate) fn new(
        db_path: &Path,
        chain_id: u32,
        orderbook_address: Address,
    ) -> Result<Self, String> {
        // Open a temporary connection to create indexes, then drop it.
        let conn =
            Connection::open(db_path).map_err(|e| format!("failed to open raindex db: {e}"))?;

        conn.pragma_update(None, "journal_mode", "wal")
            .map_err(|e| format!("failed to set WAL: {e}"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| format!("failed to set busy_timeout: {e}"))?;

        // Create indexes that the upstream library is missing. These speed up
        // the join between take_orders and order_add_events (which uses
        // owner+nonce), and the vault_balance_changes lookup by block+log.
        let indexes = [
            "CREATE INDEX IF NOT EXISTS idx_take_orders_owner_nonce \
             ON take_orders (chain_id, orderbook_address, order_owner, order_nonce)",
            "CREATE INDEX IF NOT EXISTS idx_vbc_block_log \
             ON vault_balance_changes (chain_id, orderbook_address, owner, token, vault_id, block_number, log_index)",
            "CREATE INDEX IF NOT EXISTS idx_take_orders_sender \
             ON take_orders (chain_id, orderbook_address, sender)",
            "CREATE INDEX IF NOT EXISTS idx_take_orders_sender_covering \
             ON take_orders (chain_id, orderbook_address, sender, transaction_hash, block_timestamp)",
        ];
        for sql in &indexes {
            if let Err(e) = conn.execute_batch(sql) {
                tracing::warn!(error = %e, sql, "failed to create performance index (non-fatal)");
            }
        }

        drop(conn);

        Ok(Self {
            db_path: db_path.to_path_buf(),
            chain_id: chain_id as i64,
            orderbook_address: format!("{:#x}", orderbook_address),
        })
    }

    /// Fetch trades for multiple order hashes in a single batch query.
    pub(crate) async fn batch_fetch(
        &self,
        hashes: &[B256],
    ) -> Result<HashMap<B256, Vec<OrderTradeEntry>>, ApiError> {
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }

        let db_path = self.db_path.clone();
        let chain_id = self.chain_id;
        let ob_addr = self.orderbook_address.clone();
        let hash_strings: Vec<String> = hashes.iter().map(|h| format!("{:#x}", h)).collect();

        spawn_blocking(move || {
            let start = Instant::now();
            let conn = open_read_connection(&db_path)?;

            let placeholders: Vec<String> = (0..hash_strings.len())
                .map(|i| format!("?{}", i + 3))
                .collect();
            let in_clause = placeholders.join(", ");
            let query = build_batch_query(&in_clause);

            let mut stmt = conn.prepare(&query).map_err(|e| {
                tracing::error!(error = %e, "failed to prepare batch trades query");
                ApiError::Internal("trade query failed".into())
            })?;

            // Bind: ?1 = chain_id, ?2 = orderbook_address, ?3..N = order hashes
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
                Vec::with_capacity(hash_strings.len() + 2);
            params.push(Box::new(chain_id));
            params.push(Box::new(ob_addr));
            for h in &hash_strings {
                params.push(Box::new(h.clone()));
            }
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();

            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok(RawTradeRow {
                        order_hash: row.get(0)?,
                        transaction_hash: row.get(1)?,
                        block_timestamp: row.get(2)?,
                        transaction_sender: row.get(3)?,
                        input_delta: row.get(4)?,
                        output_delta_raw: row.get(5)?,
                        trade_id: row.get(6)?,
                    })
                })
                .map_err(|e| {
                    tracing::error!(error = %e, "batch trades query failed");
                    ApiError::Internal("trade query failed".into())
                })?;

            let mut result: HashMap<B256, Vec<OrderTradeEntry>> = HashMap::new();
            let mut row_count = 0u32;

            for row_result in rows {
                let raw = row_result.map_err(|e| {
                    tracing::error!(error = %e, "failed to read trade row");
                    ApiError::Internal("trade query failed".into())
                })?;

                row_count += 1;

                match convert_raw_trade(&raw) {
                    Ok((hash, entry)) => {
                        result.entry(hash).or_default().push(entry);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            order_hash = %raw.order_hash,
                            "skipping malformed trade row"
                        );
                    }
                }
            }

            tracing::info!(
                hash_count = hash_strings.len(),
                trade_rows = row_count,
                duration_ms = start.elapsed().as_millis() as u64,
                "direct batch trades query completed"
            );

            Ok(result)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "batch trades blocking task failed");
            ApiError::Internal("trade query failed".into())
        })?
    }

    /// Fetch unique transaction hashes where `sender` was the taker.
    /// Returns (tx_hash, timestamp) sorted by timestamp descending.
    pub(crate) async fn fetch_taker_tx_hashes(
        &self,
        sender: &Address,
    ) -> Result<Vec<(B256, u64)>, ApiError> {
        let db_path = self.db_path.clone();
        let chain_id = self.chain_id;
        let ob_addr = self.orderbook_address.clone();
        let sender_hex = format!("{:#x}", sender);

        spawn_blocking(move || {
            let start = Instant::now();
            let conn = open_read_connection(&db_path)?;

            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT transaction_hash, MAX(block_timestamp) as ts \
                     FROM take_orders \
                     WHERE sender = ?1 AND chain_id = ?2 AND orderbook_address = ?3 \
                     GROUP BY transaction_hash \
                     ORDER BY ts DESC",
                )
                .map_err(|e| {
                    tracing::error!(error = %e, "failed to prepare taker tx query");
                    ApiError::Internal("taker trades query failed".into())
                })?;

            let rows = stmt
                .query_map(rusqlite::params![sender_hex, chain_id, ob_addr], |row| {
                    let tx_hash: String = row.get(0)?;
                    let timestamp: i64 = row.get(1)?;
                    Ok((tx_hash, timestamp))
                })
                .map_err(|e| {
                    tracing::error!(error = %e, "taker tx query failed");
                    ApiError::Internal("taker trades query failed".into())
                })?;

            let mut results = Vec::new();
            for row_result in rows {
                let (hash_str, ts) = row_result.map_err(|e| {
                    tracing::error!(error = %e, "failed to read taker tx row");
                    ApiError::Internal("taker trades query failed".into())
                })?;
                let hash = B256::from_str(&hash_str).map_err(|e| {
                    tracing::error!(error = %e, hash = %hash_str, "invalid tx hash in taker query");
                    ApiError::Internal("taker trades query failed".into())
                })?;
                results.push((hash, ts as u64));
            }

            tracing::info!(
                sender = %sender_hex,
                tx_count = results.len(),
                duration_ms = start.elapsed().as_millis() as u64,
                "fetched taker tx hashes"
            );

            Ok(results)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "taker tx hashes blocking task failed");
            ApiError::Internal("taker trades query failed".into())
        })?
    }

    /// Fetch trades associated with a specific transaction hash.
    /// Returns trades grouped by order hash — same shape as `batch_fetch`.
    pub(crate) async fn fetch_by_tx_hash(
        &self,
        tx_hash: &B256,
    ) -> Result<HashMap<B256, Vec<OrderTradeEntry>>, ApiError> {
        let db_path = self.db_path.clone();
        let chain_id = self.chain_id;
        let ob_addr = self.orderbook_address.clone();
        let tx_hex = format!("{:#x}", tx_hash);

        spawn_blocking(move || {
            let start = Instant::now();
            let conn = open_read_connection(&db_path)?;

            let query = build_tx_hash_query();
            let mut stmt = conn.prepare(&query).map_err(|e| {
                tracing::error!(error = %e, "failed to prepare tx hash trades query");
                ApiError::Internal("trade query failed".into())
            })?;

            let rows = stmt
                .query_map(rusqlite::params![chain_id, ob_addr, tx_hex], |row| {
                    Ok(RawTradeRow {
                        order_hash: row.get(0)?,
                        transaction_hash: row.get(1)?,
                        block_timestamp: row.get(2)?,
                        transaction_sender: row.get(3)?,
                        input_delta: row.get(4)?,
                        output_delta_raw: row.get(5)?,
                        trade_id: row.get(6)?,
                    })
                })
                .map_err(|e| {
                    tracing::error!(error = %e, "tx hash trades query failed");
                    ApiError::Internal("trade query failed".into())
                })?;

            let mut result: HashMap<B256, Vec<OrderTradeEntry>> = HashMap::new();
            let mut row_count = 0u32;

            for row_result in rows {
                let raw = row_result.map_err(|e| {
                    tracing::error!(error = %e, "failed to read trade row");
                    ApiError::Internal("trade query failed".into())
                })?;

                row_count += 1;

                match convert_raw_trade(&raw) {
                    Ok((hash, entry)) => {
                        result.entry(hash).or_default().push(entry);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            order_hash = %raw.order_hash,
                            "skipping malformed trade row"
                        );
                    }
                }
            }

            tracing::info!(
                tx_hash = %tx_hex,
                trade_rows = row_count,
                duration_ms = start.elapsed().as_millis() as u64,
                "direct tx hash trades query completed"
            );

            Ok(result)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "tx hash trades blocking task failed");
            ApiError::Internal("trade query failed".into())
        })?
    }

    /// Fetch enriched trades for multiple transaction hashes in a single batch query.
    /// Returns trades grouped by tx_hash with all fields needed for TradesByTxResponse
    /// (order_owner, token addresses, block_number, etc.).
    /// This avoids the slow library path entirely.
    pub(crate) async fn fetch_taker_tx_trades(
        &self,
        tx_hashes: &[B256],
    ) -> Result<HashMap<B256, Vec<EnrichedTradeRow>>, ApiError> {
        if tx_hashes.is_empty() {
            return Ok(HashMap::new());
        }

        let db_path = self.db_path.clone();
        let chain_id = self.chain_id;
        let ob_addr = self.orderbook_address.clone();
        let tx_hex_strings: Vec<String> = tx_hashes.iter().map(|h| format!("{:#x}", h)).collect();

        spawn_blocking(move || {
            let start = Instant::now();
            let conn = open_read_connection(&db_path)?;

            let placeholders: Vec<String> = (0..tx_hex_strings.len())
                .map(|i| format!("?{}", i + 3))
                .collect();
            let in_clause = placeholders.join(", ");
            let query = build_taker_tx_batch_query(&in_clause);

            let mut stmt = conn.prepare(&query).map_err(|e| {
                tracing::error!(error = %e, "failed to prepare taker tx batch query");
                ApiError::Internal("trade query failed".into())
            })?;

            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
                Vec::with_capacity(tx_hex_strings.len() + 2);
            params.push(Box::new(chain_id));
            params.push(Box::new(ob_addr));
            for h in &tx_hex_strings {
                params.push(Box::new(h.clone()));
            }
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();

            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok(RawEnrichedTradeRow {
                        order_hash: row.get(0)?,
                        transaction_hash: row.get(1)?,
                        block_number: row.get(2)?,
                        block_timestamp: row.get(3)?,
                        sender: row.get(4)?,
                        order_owner: row.get(5)?,
                        input_delta: row.get(6)?,
                        output_delta_raw: row.get(7)?,
                        input_token: row.get(8)?,
                        output_token: row.get(9)?,
                        input_token_symbol: row.get(10)?,
                        output_token_symbol: row.get(11)?,
                        input_token_decimals: row.get(12)?,
                        output_token_decimals: row.get(13)?,
                    })
                })
                .map_err(|e| {
                    tracing::error!(error = %e, "taker tx batch query failed");
                    ApiError::Internal("trade query failed".into())
                })?;

            let mut result: HashMap<B256, Vec<EnrichedTradeRow>> = HashMap::new();
            let mut row_count = 0u32;

            for row_result in rows {
                let raw = row_result.map_err(|e| {
                    tracing::error!(error = %e, "failed to read enriched trade row");
                    ApiError::Internal("trade query failed".into())
                })?;

                row_count += 1;

                match convert_enriched_trade(&raw) {
                    Ok(enriched) => {
                        result
                            .entry(enriched.transaction_hash)
                            .or_default()
                            .push(enriched);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            order_hash = %raw.order_hash,
                            "skipping malformed enriched trade row"
                        );
                    }
                }
            }

            tracing::info!(
                tx_count = tx_hex_strings.len(),
                trade_rows = row_count,
                duration_ms = start.elapsed().as_millis() as u64,
                "direct taker tx batch query completed"
            );

            Ok(result)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "taker tx batch blocking task failed");
            ApiError::Internal("trade query failed".into())
        })?
    }
}

struct RawTradeRow {
    order_hash: String,
    transaction_hash: String,
    block_timestamp: i64,
    transaction_sender: String,
    input_delta: String,
    output_delta_raw: String,
    trade_id: String,
}

/// Enriched trade row with token and owner info for building TradesByTxResponse directly.
#[allow(dead_code)]
pub(crate) struct EnrichedTradeRow {
    pub order_hash: B256,
    pub transaction_hash: B256,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub sender: Address,
    pub order_owner: Address,
    pub input_amount: String,
    pub output_amount: String,
    pub input_token: Address,
    pub output_token: Address,
    pub input_token_symbol: String,
    pub output_token_symbol: String,
    pub input_token_decimals: u8,
    pub output_token_decimals: u8,
}

fn convert_raw_trade(raw: &RawTradeRow) -> Result<(B256, OrderTradeEntry), ApiError> {
    let order_hash = B256::from_str(&raw.order_hash)
        .map_err(|e| ApiError::Internal(format!("invalid order hash: {e}")))?;

    let tx_hash = B256::from_str(&raw.transaction_hash)
        .map_err(|e| ApiError::Internal(format!("invalid tx hash: {e}")))?;

    let sender = Address::from_str(&raw.transaction_sender)
        .map_err(|e| ApiError::Internal(format!("invalid sender address: {e}")))?;

    let input_amount = format_float_hex(&raw.input_delta)?;
    let output_amount = negate_and_format_float_hex(&raw.output_delta_raw)?;

    let entry = OrderTradeEntry {
        id: raw.trade_id.clone(),
        tx_hash,
        input_amount,
        output_amount,
        timestamp: raw.block_timestamp as u64,
        sender,
    };

    Ok((order_hash, entry))
}

fn format_float_hex(hex: &str) -> Result<String, ApiError> {
    let float = Float::from_hex(hex).map_err(|e| {
        tracing::error!(error = %e, hex, "failed to parse float hex");
        ApiError::Internal("float conversion failed".into())
    })?;
    float.format().map_err(|e| {
        tracing::error!(error = %e, "failed to format float");
        ApiError::Internal("float formatting failed".into())
    })
}

/// Negate a Float hex value and format it — replicates the SQL FLOAT_NEGATE
/// function in Rust so we don't need to register custom SQLite functions.
fn negate_and_format_float_hex(hex: &str) -> Result<String, ApiError> {
    let neg_one = Float::parse("-1".to_string()).map_err(|e| {
        tracing::error!(error = %e, "failed to create neg-one float");
        ApiError::Internal("float conversion failed".into())
    })?;
    let float = Float::from_hex(hex).map_err(|e| {
        tracing::error!(error = %e, hex, "failed to parse float hex");
        ApiError::Internal("float conversion failed".into())
    })?;
    let negated = (neg_one * float).map_err(|e| {
        tracing::error!(error = %e, "failed to negate float");
        ApiError::Internal("float conversion failed".into())
    })?;
    negated.format().map_err(|e| {
        tracing::error!(error = %e, "failed to format negated float");
        ApiError::Internal("float formatting failed".into())
    })
}

struct RawEnrichedTradeRow {
    order_hash: String,
    transaction_hash: String,
    block_number: i64,
    block_timestamp: i64,
    sender: String,
    order_owner: String,
    input_delta: String,
    output_delta_raw: String,
    input_token: Option<String>,
    output_token: Option<String>,
    input_token_symbol: Option<String>,
    output_token_symbol: Option<String>,
    input_token_decimals: Option<i32>,
    output_token_decimals: Option<i32>,
}

fn convert_enriched_trade(raw: &RawEnrichedTradeRow) -> Result<EnrichedTradeRow, ApiError> {
    let order_hash = B256::from_str(&raw.order_hash)
        .map_err(|e| ApiError::Internal(format!("invalid order hash: {e}")))?;
    let tx_hash = B256::from_str(&raw.transaction_hash)
        .map_err(|e| ApiError::Internal(format!("invalid tx hash: {e}")))?;
    let sender = Address::from_str(&raw.sender)
        .map_err(|e| ApiError::Internal(format!("invalid sender address: {e}")))?;
    let order_owner = Address::from_str(&raw.order_owner)
        .map_err(|e| ApiError::Internal(format!("invalid order owner: {e}")))?;
    let input_token = raw
        .input_token
        .as_deref()
        .map(Address::from_str)
        .transpose()
        .map_err(|e| ApiError::Internal(format!("invalid input token: {e}")))?
        .unwrap_or(Address::ZERO);
    let output_token = raw
        .output_token
        .as_deref()
        .map(Address::from_str)
        .transpose()
        .map_err(|e| ApiError::Internal(format!("invalid output token: {e}")))?
        .unwrap_or(Address::ZERO);

    let input_amount = format_float_hex(&raw.input_delta)?;
    let output_amount = negate_and_format_float_hex(&raw.output_delta_raw)?;

    Ok(EnrichedTradeRow {
        order_hash,
        transaction_hash: tx_hash,
        block_number: raw.block_number as u64,
        block_timestamp: raw.block_timestamp as u64,
        sender,
        order_owner,
        input_amount,
        output_amount,
        input_token,
        output_token,
        input_token_symbol: raw.input_token_symbol.clone().unwrap_or_default(),
        output_token_symbol: raw.output_token_symbol.clone().unwrap_or_default(),
        input_token_decimals: raw.input_token_decimals.unwrap_or(0) as u8,
        output_token_decimals: raw.output_token_decimals.unwrap_or(0) as u8,
    })
}

/// Build a batch query for trades across multiple transaction hashes.
/// Joins order_ios + erc20_tokens to get token addresses and metadata.
/// ?1 = chain_id, ?2 = orderbook_address, ?3..N = transaction hashes
fn build_taker_tx_batch_query(in_clause: &str) -> String {
    format!(
        r#"
SELECT
  oe.order_hash,
  t.transaction_hash,
  t.block_number,
  t.block_timestamp,
  t.sender,
  oe.order_owner,
  t.taker_output AS input_delta,
  t.taker_input AS output_delta_raw,
  io_in.token AS input_token,
  io_out.token AS output_token,
  tok_in.symbol AS input_token_symbol,
  tok_out.symbol AS output_token_symbol,
  tok_in.decimals AS input_token_decimals,
  tok_out.decimals AS output_token_decimals
FROM take_orders t
JOIN order_events oe
  ON oe.chain_id = t.chain_id
 AND oe.orderbook_address = t.orderbook_address
 AND oe.order_owner = t.order_owner
 AND oe.order_nonce = t.order_nonce
 AND oe.event_type = 'AddOrderV3'
 AND (oe.block_number < t.block_number
   OR (oe.block_number = t.block_number AND oe.log_index <= t.log_index))
 AND NOT EXISTS (
   SELECT 1 FROM order_events newer
   WHERE newer.chain_id = oe.chain_id
    AND newer.orderbook_address = oe.orderbook_address
    AND newer.order_owner = oe.order_owner
    AND newer.order_nonce = oe.order_nonce
    AND newer.event_type = 'AddOrderV3'
    AND (newer.block_number < t.block_number
      OR (newer.block_number = t.block_number AND newer.log_index <= t.log_index))
    AND (newer.block_number > oe.block_number
      OR (newer.block_number = oe.block_number AND newer.log_index > oe.log_index))
 )
LEFT JOIN order_ios io_in
  ON io_in.chain_id = oe.chain_id
 AND io_in.orderbook_address = oe.orderbook_address
 AND io_in.transaction_hash = oe.transaction_hash
 AND io_in.log_index = oe.log_index
 AND io_in.io_index = t.input_io_index
 AND io_in.io_type = 'input'
LEFT JOIN order_ios io_out
  ON io_out.chain_id = oe.chain_id
 AND io_out.orderbook_address = oe.orderbook_address
 AND io_out.transaction_hash = oe.transaction_hash
 AND io_out.log_index = oe.log_index
 AND io_out.io_index = t.output_io_index
 AND io_out.io_type = 'output'
LEFT JOIN erc20_tokens tok_in
  ON tok_in.chain_id = oe.chain_id
 AND tok_in.orderbook_address = oe.orderbook_address
 AND tok_in.token_address = io_in.token
LEFT JOIN erc20_tokens tok_out
  ON tok_out.chain_id = oe.chain_id
 AND tok_out.orderbook_address = oe.orderbook_address
 AND tok_out.token_address = io_out.token
WHERE t.chain_id = ?1
  AND t.orderbook_address = ?2
  AND t.transaction_hash IN ({in_clause})
ORDER BY t.transaction_hash, t.block_timestamp DESC, t.log_index DESC
"#,
        in_clause = in_clause
    )
}

/// Build a batch trade query with a dynamic IN-clause. This is a simplified
/// version of rain.orderbook's `fetch_order_trades/query.sql` that:
/// - Accepts multiple order hashes at once (via IN-clause)
/// - Drops vault balance snapshot lookups (not needed for the API response)
/// - Skips FLOAT_NEGATE (handled in Rust after fetching)
fn build_batch_query(in_clause: &str) -> String {
    format!(
        r#"
WITH
order_add_events AS (
  SELECT
    oe.chain_id, oe.orderbook_address, oe.transaction_hash, oe.log_index,
    oe.block_number, oe.block_timestamp, oe.order_owner, oe.order_nonce, oe.order_hash
  FROM order_events oe
  WHERE oe.chain_id = ?1
    AND oe.orderbook_address = ?2
    AND oe.order_hash IN ({in_clause})
    AND oe.event_type = 'AddOrderV3'
),
take_trades AS (
  SELECT
    oe.order_hash,
    t.transaction_hash,
    t.log_index,
    t.block_timestamp,
    t.sender AS transaction_sender,
    t.taker_output AS input_delta,
    t.taker_input AS output_delta_raw
  FROM take_orders t
  JOIN order_add_events oe
    ON oe.chain_id = t.chain_id
   AND oe.orderbook_address = t.orderbook_address
   AND oe.order_owner = t.order_owner
   AND oe.order_nonce = t.order_nonce
   AND (oe.block_number < t.block_number
     OR (oe.block_number = t.block_number AND oe.log_index <= t.log_index))
   AND NOT EXISTS (
     SELECT 1 FROM order_add_events newer
     WHERE newer.chain_id = oe.chain_id
      AND newer.orderbook_address = oe.orderbook_address
      AND newer.order_owner = oe.order_owner
      AND newer.order_nonce = oe.order_nonce
      AND (newer.block_number < t.block_number
        OR (newer.block_number = t.block_number AND newer.log_index <= t.log_index))
      AND (newer.block_number > oe.block_number
        OR (newer.block_number = oe.block_number AND newer.log_index > oe.log_index))
   )
  WHERE t.chain_id = ?1
    AND t.orderbook_address = ?2
),
clear_alice AS (
  SELECT DISTINCT
    oe.order_hash,
    c.transaction_hash,
    c.log_index,
    c.block_timestamp,
    c.sender AS transaction_sender,
    a.alice_input AS input_delta,
    a.alice_output AS output_delta_raw
  FROM clear_v3_events c
  JOIN order_add_events oe
    ON oe.chain_id = c.chain_id
   AND oe.orderbook_address = c.orderbook_address
   AND oe.order_hash = c.alice_order_hash
   AND (oe.block_number < c.block_number
     OR (oe.block_number = c.block_number AND oe.log_index <= c.log_index))
   AND NOT EXISTS (
     SELECT 1 FROM order_add_events newer
     WHERE newer.chain_id = oe.chain_id
      AND newer.orderbook_address = oe.orderbook_address
      AND newer.order_hash = oe.order_hash
      AND (newer.block_number < c.block_number
        OR (newer.block_number = c.block_number AND newer.log_index <= c.log_index))
      AND (newer.block_number > oe.block_number
        OR (newer.block_number = oe.block_number AND newer.log_index > oe.log_index))
   )
  JOIN after_clear_v2_events a
    ON a.chain_id = c.chain_id
   AND a.orderbook_address = c.orderbook_address
   AND a.transaction_hash = c.transaction_hash
   AND a.log_index = (
       SELECT MIN(ac.log_index)
       FROM after_clear_v2_events ac
       WHERE ac.chain_id = c.chain_id
         AND ac.orderbook_address = c.orderbook_address
         AND ac.transaction_hash = c.transaction_hash
         AND ac.log_index > c.log_index
   )
  WHERE c.chain_id = ?1
    AND c.orderbook_address = ?2
    AND c.alice_order_hash IN ({in_clause})
),
clear_bob AS (
  SELECT DISTINCT
    oe.order_hash,
    c.transaction_hash,
    c.log_index,
    c.block_timestamp,
    c.sender AS transaction_sender,
    a.bob_input AS input_delta,
    a.bob_output AS output_delta_raw
  FROM clear_v3_events c
  JOIN order_add_events oe
    ON oe.chain_id = c.chain_id
   AND oe.orderbook_address = c.orderbook_address
   AND oe.order_hash = c.bob_order_hash
   AND (oe.block_number < c.block_number
     OR (oe.block_number = c.block_number AND oe.log_index <= c.log_index))
   AND NOT EXISTS (
     SELECT 1 FROM order_add_events newer
     WHERE newer.chain_id = oe.chain_id
      AND newer.orderbook_address = oe.orderbook_address
      AND newer.order_hash = oe.order_hash
      AND (newer.block_number < c.block_number
        OR (newer.block_number = c.block_number AND newer.log_index <= c.log_index))
      AND (newer.block_number > oe.block_number
        OR (newer.block_number = oe.block_number AND newer.log_index > oe.log_index))
   )
  JOIN after_clear_v2_events a
    ON a.chain_id = c.chain_id
   AND a.orderbook_address = c.orderbook_address
   AND a.transaction_hash = c.transaction_hash
   AND a.log_index = (
       SELECT MIN(ac.log_index)
       FROM after_clear_v2_events ac
       WHERE ac.chain_id = c.chain_id
         AND ac.orderbook_address = c.orderbook_address
         AND ac.transaction_hash = c.transaction_hash
         AND ac.log_index > c.log_index
   )
  WHERE c.chain_id = ?1
    AND c.orderbook_address = ?2
    AND c.bob_order_hash IN ({in_clause})
)
SELECT
  order_hash,
  transaction_hash,
  block_timestamp,
  transaction_sender,
  input_delta,
  output_delta_raw,
  ('0x' || lower(replace(transaction_hash, '0x', '')) || printf('%016x', log_index)) AS trade_id
FROM (
  SELECT * FROM take_trades
  UNION ALL
  SELECT * FROM clear_alice
  UNION ALL
  SELECT * FROM clear_bob
)
ORDER BY order_hash, block_timestamp DESC, log_index DESC
"#,
        in_clause = in_clause
    )
}

/// Build a query that finds all trades in a given transaction.
/// Filters by `transaction_hash` on the take_orders / clear tables
/// and joins back to order_events to get the order_hash.
/// ?1 = chain_id, ?2 = orderbook_address, ?3 = transaction_hash
fn build_tx_hash_query() -> String {
    r#"
WITH
take_trades AS (
  SELECT
    oe.order_hash,
    t.transaction_hash,
    t.log_index,
    t.block_timestamp,
    t.sender AS transaction_sender,
    t.taker_output AS input_delta,
    t.taker_input AS output_delta_raw
  FROM take_orders t
  JOIN order_events oe
    ON oe.chain_id = t.chain_id
   AND oe.orderbook_address = t.orderbook_address
   AND oe.order_owner = t.order_owner
   AND oe.order_nonce = t.order_nonce
   AND oe.event_type = 'AddOrderV3'
   AND (oe.block_number < t.block_number
     OR (oe.block_number = t.block_number AND oe.log_index <= t.log_index))
   AND NOT EXISTS (
     SELECT 1 FROM order_events newer
     WHERE newer.chain_id = oe.chain_id
      AND newer.orderbook_address = oe.orderbook_address
      AND newer.order_owner = oe.order_owner
      AND newer.order_nonce = oe.order_nonce
      AND newer.event_type = 'AddOrderV3'
      AND (newer.block_number < t.block_number
        OR (newer.block_number = t.block_number AND newer.log_index <= t.log_index))
      AND (newer.block_number > oe.block_number
        OR (newer.block_number = oe.block_number AND newer.log_index > oe.log_index))
   )
  WHERE t.chain_id = ?1
    AND t.orderbook_address = ?2
    AND t.transaction_hash = ?3
),
clear_alice AS (
  SELECT DISTINCT
    oe.order_hash,
    c.transaction_hash,
    c.log_index,
    c.block_timestamp,
    c.sender AS transaction_sender,
    a.alice_input AS input_delta,
    a.alice_output AS output_delta_raw
  FROM clear_v3_events c
  JOIN order_events oe
    ON oe.chain_id = c.chain_id
   AND oe.orderbook_address = c.orderbook_address
   AND oe.order_hash = c.alice_order_hash
   AND oe.event_type = 'AddOrderV3'
   AND (oe.block_number < c.block_number
     OR (oe.block_number = c.block_number AND oe.log_index <= c.log_index))
   AND NOT EXISTS (
     SELECT 1 FROM order_events newer
     WHERE newer.chain_id = oe.chain_id
      AND newer.orderbook_address = oe.orderbook_address
      AND newer.order_hash = oe.order_hash
      AND newer.event_type = 'AddOrderV3'
      AND (newer.block_number < c.block_number
        OR (newer.block_number = c.block_number AND newer.log_index <= c.log_index))
      AND (newer.block_number > oe.block_number
        OR (newer.block_number = oe.block_number AND newer.log_index > oe.log_index))
   )
  JOIN after_clear_v2_events a
    ON a.chain_id = c.chain_id
   AND a.orderbook_address = c.orderbook_address
   AND a.transaction_hash = c.transaction_hash
   AND a.log_index = (
       SELECT MIN(ac.log_index)
       FROM after_clear_v2_events ac
       WHERE ac.chain_id = c.chain_id
         AND ac.orderbook_address = c.orderbook_address
         AND ac.transaction_hash = c.transaction_hash
         AND ac.log_index > c.log_index
   )
  WHERE c.chain_id = ?1
    AND c.orderbook_address = ?2
    AND c.transaction_hash = ?3
),
clear_bob AS (
  SELECT DISTINCT
    oe.order_hash,
    c.transaction_hash,
    c.log_index,
    c.block_timestamp,
    c.sender AS transaction_sender,
    a.bob_input AS input_delta,
    a.bob_output AS output_delta_raw
  FROM clear_v3_events c
  JOIN order_events oe
    ON oe.chain_id = c.chain_id
   AND oe.orderbook_address = c.orderbook_address
   AND oe.order_hash = c.bob_order_hash
   AND oe.event_type = 'AddOrderV3'
   AND (oe.block_number < c.block_number
     OR (oe.block_number = c.block_number AND oe.log_index <= c.log_index))
   AND NOT EXISTS (
     SELECT 1 FROM order_events newer
     WHERE newer.chain_id = oe.chain_id
      AND newer.orderbook_address = oe.orderbook_address
      AND newer.order_hash = oe.order_hash
      AND newer.event_type = 'AddOrderV3'
      AND (newer.block_number < c.block_number
        OR (newer.block_number = c.block_number AND newer.log_index <= c.log_index))
      AND (newer.block_number > oe.block_number
        OR (newer.block_number = oe.block_number AND newer.log_index > oe.log_index))
   )
  JOIN after_clear_v2_events a
    ON a.chain_id = c.chain_id
   AND a.orderbook_address = c.orderbook_address
   AND a.transaction_hash = c.transaction_hash
   AND a.log_index = (
       SELECT MIN(ac.log_index)
       FROM after_clear_v2_events ac
       WHERE ac.chain_id = c.chain_id
         AND ac.orderbook_address = c.orderbook_address
         AND ac.transaction_hash = c.transaction_hash
         AND ac.log_index > c.log_index
   )
  WHERE c.chain_id = ?1
    AND c.orderbook_address = ?2
    AND c.bob_order_hash IN (
      SELECT DISTINCT bob_order_hash FROM clear_v3_events
      WHERE chain_id = ?1 AND orderbook_address = ?2 AND transaction_hash = ?3
    )
)
SELECT
  order_hash,
  transaction_hash,
  block_timestamp,
  transaction_sender,
  input_delta,
  output_delta_raw,
  ('0x' || lower(replace(transaction_hash, '0x', '')) || printf('%016x', log_index)) AS trade_id
FROM (
  SELECT * FROM take_trades
  UNION ALL
  SELECT * FROM clear_alice
  UNION ALL
  SELECT * FROM clear_bob
)
ORDER BY order_hash, block_timestamp DESC, log_index DESC
"#
    .to_string()
}
