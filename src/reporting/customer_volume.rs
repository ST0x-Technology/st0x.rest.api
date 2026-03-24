use crate::db::DbPool;
use crate::raindex::RaindexProvider;
use rain_math_float::Float;
use rain_orderbook_common::raindex_client::RaindexError;
use reqwest::Client as HttpClient;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, QueryBuilder};
use std::collections::{BTreeMap, HashMap};
use std::ops::Add;

const MATCH_WINDOW_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CustomerVolumeReportArgs {
    pub start_time: u64,
    pub end_time: u64,
    pub json: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CustomerVolumeReportError {
    #[error("start_time must be less than or equal to end_time")]
    InvalidWindow,
    #[error("raindex local db path is unavailable")]
    MissingLocalDbPath,
    #[error("raindex local db file not found: {path}")]
    MissingLocalDb { path: String },
    #[error("no orderbooks configured for chain {chain_id}")]
    MissingOrderbooks { chain_id: u32 },
    #[error("no RPC URLs configured for chain {chain_id}")]
    MissingRpcUrls { chain_id: u32 },
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("float error: {0}")]
    Float(#[from] rain_math_float::FloatError),
    #[error("raindex client error: {0}")]
    Raindex(#[from] RaindexError),
    #[error("failed to fetch transaction input for {tx_hash}: {message}")]
    RpcLookup { tx_hash: String, message: String },
    #[error("transaction {tx_hash} was not found via RPC")]
    TransactionNotFound { tx_hash: String },
    #[error("inconsistent candidate transaction data for {tx_hash}")]
    InconsistentCandidate { tx_hash: String },
}

#[derive(Debug, Clone, FromRow)]
struct IssuedSwapRow {
    id: i64,
    api_key_id: i64,
    key_id: String,
    label: String,
    owner: String,
    taker: String,
    to_address: String,
    calldata: String,
    input_token: String,
    output_token: String,
    created_at_unix: i64,
}

#[derive(Debug, Clone, FromRow)]
struct TakeOrderRow {
    orderbook_address: String,
    transaction_hash: String,
    block_number: i64,
    block_timestamp: i64,
    sender: String,
    taker_input: String,
    taker_output: String,
}

#[derive(Debug, Clone)]
struct CandidateExecution {
    tx_hash: String,
    orderbook_address: String,
    sender: String,
    block_number: u64,
    timestamp: u64,
    total_input_hex: String,
    total_output_hex: String,
}

#[derive(Debug, Clone)]
struct MatchedExecution {
    issued_swap_calldata_id: i64,
    api_key_id: i64,
    key_id: String,
    label: String,
    owner: String,
    tx_hash: String,
    sender: String,
    to_address: String,
    block_number: u64,
    timestamp: u64,
    input_token: String,
    output_token: String,
    total_input_hex: String,
    total_output_hex: String,
    duplicate_match_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ReportAudit {
    matched_executions: usize,
    unmatched_executions: usize,
    ambiguous_matches: usize,
}

#[derive(Debug, Clone, Serialize)]
struct PairVolume {
    input_token: String,
    output_token: String,
    total_input_amount: String,
    total_output_amount: String,
}

#[derive(Debug, Clone, Serialize)]
struct CustomerVolumeSummary {
    api_key_id: i64,
    key_id: String,
    label: String,
    owner: String,
    executed_txs: usize,
    pairs: Vec<PairVolume>,
}

#[derive(Debug, Clone, Serialize)]
struct MatchedTransactionReport {
    tx_hash: String,
    api_key_id: i64,
    key_id: String,
    label: String,
    owner: String,
    sender: String,
    to_address: String,
    block_number: u64,
    timestamp: u64,
    input_token: String,
    output_token: String,
    total_input_amount: String,
    total_output_amount: String,
    duplicate_match_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct UnmatchedTransactionReport {
    tx_hash: String,
    sender: String,
    to_address: String,
    block_number: u64,
    timestamp: u64,
    total_input_amount: String,
    total_output_amount: String,
}

#[derive(Debug, Clone, Serialize)]
struct CustomerVolumeReport {
    start_time: u64,
    end_time: u64,
    audit: ReportAudit,
    customers: Vec<CustomerVolumeSummary>,
    matched_transactions: Vec<MatchedTransactionReport>,
    unmatched_transactions: Vec<UnmatchedTransactionReport>,
}

#[derive(Debug)]
struct BuiltCustomerVolumeReport {
    report: CustomerVolumeReport,
    matched_executions: Vec<MatchedExecution>,
}

type IssuedRowIndex<'a> = HashMap<(String, String, String), Vec<&'a IssuedSwapRow>>;

#[derive(Debug, Deserialize)]
struct JsonRpcEnvelope<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcTransactionResponse {
    input: String,
}

pub(crate) async fn run(
    pool: &DbPool,
    raindex: &RaindexProvider,
    args: CustomerVolumeReportArgs,
) -> Result<(), CustomerVolumeReportError> {
    if args.start_time > args.end_time {
        return Err(CustomerVolumeReportError::InvalidWindow);
    }

    tracing::info!(
        start_time = args.start_time,
        end_time = args.end_time,
        "customer volume report requested"
    );

    let built_report = build_report(pool, raindex, args.start_time, args.end_time).await?;
    persist_matches(pool, &built_report.matched_executions).await?;
    persist_report_run(pool, &built_report.report).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&built_report.report)?);
    } else {
        print_human_report(&built_report.report);
    }

    tracing::info!(
        matched = built_report.report.audit.matched_executions,
        unmatched = built_report.report.audit.unmatched_executions,
        ambiguous = built_report.report.audit.ambiguous_matches,
        "customer volume report complete"
    );

    Ok(())
}

async fn build_report(
    pool: &DbPool,
    raindex: &RaindexProvider,
    start_time: u64,
    end_time: u64,
) -> Result<BuiltCustomerVolumeReport, CustomerVolumeReportError> {
    let chain_orderbooks = orderbooks_for_chain(raindex)?;
    let issued_rows = load_issued_rows(pool, start_time, end_time).await?;
    let issued_row_index = build_issued_row_index(&issued_rows);

    tracing::info!(
        orderbook_count = chain_orderbooks.len(),
        issued_count = issued_rows.len(),
        "loaded issued swap rows for customer volume report"
    );

    let raindex_db = open_raindex_db(raindex).await?;
    let candidate_rows =
        load_take_order_rows(&raindex_db, &chain_orderbooks, start_time, end_time).await?;
    let candidates = aggregate_candidate_rows(candidate_rows)?;
    let rpc_urls = rpc_urls_for_chain(raindex)?;
    let rpc = HttpClient::new();

    tracing::info!(
        candidate_count = candidates.len(),
        rpc_url_count = rpc_urls.len(),
        "loaded candidate executions from raindex db"
    );

    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    let mut ambiguous_matches = 0usize;

    for candidate in candidates {
        let tx_input = fetch_transaction_input(&rpc, &rpc_urls, &candidate.tx_hash).await?;
        let selection = select_issued_row(&candidate, &tx_input, &issued_row_index);

        if selection.duplicate_match_count > 1 {
            ambiguous_matches += 1;
        }

        match selection.row {
            Some(row) => matched.push(MatchedExecution {
                issued_swap_calldata_id: row.id,
                api_key_id: row.api_key_id,
                key_id: row.key_id.clone(),
                label: row.label.clone(),
                owner: row.owner.clone(),
                tx_hash: candidate.tx_hash.clone(),
                sender: candidate.sender.clone(),
                to_address: candidate.orderbook_address.clone(),
                block_number: candidate.block_number,
                timestamp: candidate.timestamp,
                input_token: row.input_token.clone(),
                output_token: row.output_token.clone(),
                total_input_hex: candidate.total_input_hex.clone(),
                total_output_hex: candidate.total_output_hex.clone(),
                duplicate_match_count: selection.duplicate_match_count,
            }),
            None => unmatched.push(UnmatchedTransactionReport {
                tx_hash: candidate.tx_hash.clone(),
                sender: candidate.sender.clone(),
                to_address: candidate.orderbook_address.clone(),
                block_number: candidate.block_number,
                timestamp: candidate.timestamp,
                total_input_amount: format_hex(&candidate.total_input_hex)?,
                total_output_amount: format_hex(&candidate.total_output_hex)?,
            }),
        }
    }

    let customers = build_customer_summaries(&matched)?;
    let matched_transactions = matched
        .iter()
        .map(|execution| {
            Ok(MatchedTransactionReport {
                tx_hash: execution.tx_hash.clone(),
                api_key_id: execution.api_key_id,
                key_id: execution.key_id.clone(),
                label: execution.label.clone(),
                owner: execution.owner.clone(),
                sender: execution.sender.clone(),
                to_address: execution.to_address.clone(),
                block_number: execution.block_number,
                timestamp: execution.timestamp,
                input_token: execution.input_token.clone(),
                output_token: execution.output_token.clone(),
                total_input_amount: format_hex(&execution.total_input_hex)?,
                total_output_amount: format_hex(&execution.total_output_hex)?,
                duplicate_match_count: execution.duplicate_match_count,
            })
        })
        .collect::<Result<Vec<_>, CustomerVolumeReportError>>()?;

    Ok(BuiltCustomerVolumeReport {
        report: CustomerVolumeReport {
            start_time,
            end_time,
            audit: ReportAudit {
                matched_executions: matched_transactions.len(),
                unmatched_executions: unmatched.len(),
                ambiguous_matches,
            },
            customers,
            matched_transactions,
            unmatched_transactions: unmatched,
        },
        matched_executions: matched,
    })
}

fn orderbooks_for_chain(
    raindex: &RaindexProvider,
) -> Result<Vec<String>, CustomerVolumeReportError> {
    let mut addresses = raindex
        .client()
        .get_all_orderbooks()?
        .into_values()
        .filter(|orderbook| orderbook.network.chain_id == crate::CHAIN_ID)
        .map(|orderbook| normalize_address(&format!("{:#x}", orderbook.address)))
        .collect::<Vec<_>>();

    addresses.sort();
    addresses.dedup();

    if addresses.is_empty() {
        return Err(CustomerVolumeReportError::MissingOrderbooks {
            chain_id: crate::CHAIN_ID,
        });
    }

    Ok(addresses)
}

fn rpc_urls_for_chain(raindex: &RaindexProvider) -> Result<Vec<Url>, CustomerVolumeReportError> {
    let network = raindex.client().get_network_by_chain_id(crate::CHAIN_ID)?;
    if network.rpcs.is_empty() {
        return Err(CustomerVolumeReportError::MissingRpcUrls {
            chain_id: crate::CHAIN_ID,
        });
    }
    Ok(network.rpcs)
}

async fn open_raindex_db(
    raindex: &RaindexProvider,
) -> Result<sqlx::Pool<sqlx::Sqlite>, CustomerVolumeReportError> {
    let path = raindex
        .db_path()
        .ok_or(CustomerVolumeReportError::MissingLocalDbPath)?;

    if !path.exists() {
        return Err(CustomerVolumeReportError::MissingLocalDb {
            path: path.display().to_string(),
        });
    }

    let options = SqliteConnectOptions::new().filename(path).read_only(true);

    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(CustomerVolumeReportError::Database)
}

async fn load_issued_rows(
    pool: &DbPool,
    start_time: u64,
    end_time: u64,
) -> Result<Vec<IssuedSwapRow>, CustomerVolumeReportError> {
    let min_issue_time = start_time.saturating_sub(MATCH_WINDOW_SECS as u64) as i64;
    let max_issue_time = end_time as i64;

    sqlx::query_as::<_, IssuedSwapRow>(
        "SELECT
            isc.id,
            isc.api_key_id,
            ak.key_id,
            ak.label,
            ak.owner,
            isc.taker,
            isc.to_address,
            isc.calldata,
            isc.input_token,
            isc.output_token,
            CAST(strftime('%s', isc.created_at) AS INTEGER) AS created_at_unix
         FROM issued_swap_calldata isc
         JOIN api_keys ak ON ak.id = isc.api_key_id
         WHERE isc.chain_id = ?
           AND CAST(strftime('%s', isc.created_at) AS INTEGER) >= ?
           AND CAST(strftime('%s', isc.created_at) AS INTEGER) <= ?
         ORDER BY created_at_unix DESC, isc.id DESC",
    )
    .bind(crate::CHAIN_ID as i64)
    .bind(min_issue_time)
    .bind(max_issue_time)
    .fetch_all(pool)
    .await
    .map_err(CustomerVolumeReportError::Database)
}

async fn load_take_order_rows(
    raindex_db: &sqlx::Pool<sqlx::Sqlite>,
    orderbook_addresses: &[String],
    start_time: u64,
    end_time: u64,
) -> Result<Vec<TakeOrderRow>, CustomerVolumeReportError> {
    let mut query_builder = QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT
            orderbook_address,
            transaction_hash,
            block_number,
            block_timestamp,
            sender,
            taker_input,
            taker_output
         FROM take_orders
         WHERE chain_id = ",
    );

    query_builder
        .push_bind(crate::CHAIN_ID as i64)
        .push(" AND block_timestamp >= ")
        .push_bind(start_time as i64)
        .push(" AND block_timestamp <= ")
        .push_bind(end_time as i64)
        .push(" AND orderbook_address IN (");

    let mut separated = query_builder.separated(", ");
    for orderbook in orderbook_addresses {
        separated.push_bind(orderbook);
    }
    separated.push_unseparated(")");
    query_builder.push(" ORDER BY block_number, log_index");

    query_builder
        .build_query_as::<TakeOrderRow>()
        .fetch_all(raindex_db)
        .await
        .map_err(CustomerVolumeReportError::Database)
}

fn aggregate_candidate_rows(
    rows: Vec<TakeOrderRow>,
) -> Result<Vec<CandidateExecution>, CustomerVolumeReportError> {
    let mut aggregates: HashMap<String, CandidateExecution> = HashMap::new();

    for row in rows {
        let tx_hash = normalize_hex(&row.transaction_hash);
        let sender = normalize_address(&row.sender);
        let orderbook_address = normalize_address(&row.orderbook_address);
        let input_delta = normalize_hex(&row.taker_output);
        let output_delta = normalize_hex(&row.taker_input);

        match aggregates.get_mut(&tx_hash) {
            Some(existing) => {
                if existing.sender != sender
                    || existing.orderbook_address != orderbook_address
                    || existing.block_number != row.block_number as u64
                    || existing.timestamp != row.block_timestamp as u64
                {
                    return Err(CustomerVolumeReportError::InconsistentCandidate { tx_hash });
                }

                add_hex_amount(&mut existing.total_input_hex, &input_delta)?;
                add_hex_amount(&mut existing.total_output_hex, &output_delta)?;
            }
            None => {
                let mut total_input_hex = zero_hex()?;
                add_hex_amount(&mut total_input_hex, &input_delta)?;

                let mut total_output_hex = zero_hex()?;
                add_hex_amount(&mut total_output_hex, &output_delta)?;

                aggregates.insert(
                    tx_hash.clone(),
                    CandidateExecution {
                        tx_hash,
                        orderbook_address,
                        sender,
                        block_number: row.block_number as u64,
                        timestamp: row.block_timestamp as u64,
                        total_input_hex,
                        total_output_hex,
                    },
                );
            }
        }
    }

    let mut candidates = aggregates.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.block_number.cmp(&right.block_number))
            .then_with(|| left.tx_hash.cmp(&right.tx_hash))
    });
    Ok(candidates)
}

struct Selection<'a> {
    row: Option<&'a IssuedSwapRow>,
    duplicate_match_count: usize,
}

fn build_issued_row_index(issued_rows: &[IssuedSwapRow]) -> IssuedRowIndex<'_> {
    let mut index = HashMap::new();

    for row in issued_rows {
        index
            .entry(issued_row_key(&row.taker, &row.to_address, &row.calldata))
            .or_insert_with(Vec::new)
            .push(row);
    }

    for rows in index.values_mut() {
        rows.sort_by(|left, right| {
            right
                .created_at_unix
                .cmp(&left.created_at_unix)
                .then_with(|| right.id.cmp(&left.id))
        });
    }

    index
}

fn issued_row_key(taker: &str, to_address: &str, calldata: &str) -> (String, String, String) {
    (
        normalize_address(taker),
        normalize_address(to_address),
        normalize_hex(calldata),
    )
}

fn select_issued_row<'a>(
    candidate: &CandidateExecution,
    tx_input: &str,
    issued_row_index: &IssuedRowIndex<'a>,
) -> Selection<'a> {
    let candidate_timestamp = candidate.timestamp as i64;
    let issued_rows = match issued_row_index.get(&issued_row_key(
        &candidate.sender,
        &candidate.orderbook_address,
        tx_input,
    )) {
        Some(rows) => rows,
        None => {
            return Selection {
                row: None,
                duplicate_match_count: 0,
            };
        }
    };

    let mut matched_row = None;
    let mut duplicate_match_count = 0;

    for row in issued_rows {
        if row.created_at_unix > candidate_timestamp {
            continue;
        }

        if candidate_timestamp <= row.created_at_unix + MATCH_WINDOW_SECS {
            duplicate_match_count += 1;
            if matched_row.is_none() {
                matched_row = Some(*row);
            }
            continue;
        }

        break;
    }

    Selection {
        row: matched_row,
        duplicate_match_count,
    }
}

fn build_customer_summaries(
    executions: &[MatchedExecution],
) -> Result<Vec<CustomerVolumeSummary>, CustomerVolumeReportError> {
    #[derive(Debug)]
    struct PairAccumulator {
        total_input_hex: String,
        total_output_hex: String,
    }

    #[derive(Debug)]
    struct CustomerAccumulator {
        api_key_id: i64,
        key_id: String,
        label: String,
        owner: String,
        executed_txs: usize,
        pairs: BTreeMap<(String, String), PairAccumulator>,
    }

    let mut customers: BTreeMap<i64, CustomerAccumulator> = BTreeMap::new();

    for execution in executions {
        let customer =
            customers
                .entry(execution.api_key_id)
                .or_insert_with(|| CustomerAccumulator {
                    api_key_id: execution.api_key_id,
                    key_id: execution.key_id.clone(),
                    label: execution.label.clone(),
                    owner: execution.owner.clone(),
                    executed_txs: 0,
                    pairs: BTreeMap::new(),
                });

        customer.executed_txs += 1;

        let pair_key = (
            execution.input_token.clone(),
            execution.output_token.clone(),
        );
        match customer.pairs.entry(pair_key) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                add_hex_amount(
                    &mut entry.get_mut().total_input_hex,
                    &execution.total_input_hex,
                )?;
                add_hex_amount(
                    &mut entry.get_mut().total_output_hex,
                    &execution.total_output_hex,
                )?;
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                let mut total_input_hex = zero_hex()?;
                add_hex_amount(&mut total_input_hex, &execution.total_input_hex)?;

                let mut total_output_hex = zero_hex()?;
                add_hex_amount(&mut total_output_hex, &execution.total_output_hex)?;

                entry.insert(PairAccumulator {
                    total_input_hex,
                    total_output_hex,
                });
            }
        }
    }

    customers
        .into_values()
        .map(|customer| {
            let pairs = customer
                .pairs
                .into_iter()
                .map(|((input_token, output_token), pair)| {
                    Ok(PairVolume {
                        input_token,
                        output_token,
                        total_input_amount: format_hex(&pair.total_input_hex)?,
                        total_output_amount: format_hex(&pair.total_output_hex)?,
                    })
                })
                .collect::<Result<Vec<_>, CustomerVolumeReportError>>()?;

            Ok(CustomerVolumeSummary {
                api_key_id: customer.api_key_id,
                key_id: customer.key_id,
                label: customer.label,
                owner: customer.owner,
                executed_txs: customer.executed_txs,
                pairs,
            })
        })
        .collect()
}

async fn fetch_transaction_input(
    client: &HttpClient,
    rpc_urls: &[Url],
    tx_hash: &str,
) -> Result<String, CustomerVolumeReportError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getTransactionByHash",
        "params": [tx_hash],
    });

    let mut last_error = None;

    for rpc_url in rpc_urls {
        let response = match client.post(rpc_url.clone()).json(&payload).send().await {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };

        let envelope = match response
            .json::<JsonRpcEnvelope<RpcTransactionResponse>>()
            .await
        {
            Ok(envelope) => envelope,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };

        if let Some(result) = envelope.result {
            return Ok(normalize_hex(&result.input));
        }

        if let Some(error) = envelope.error {
            last_error = Some(error.message);
        }
    }

    if let Some(message) = last_error {
        return Err(CustomerVolumeReportError::RpcLookup {
            tx_hash: tx_hash.to_string(),
            message,
        });
    }

    Err(CustomerVolumeReportError::TransactionNotFound {
        tx_hash: tx_hash.to_string(),
    })
}

async fn persist_matches(
    pool: &DbPool,
    matches: &[MatchedExecution],
) -> Result<(), CustomerVolumeReportError> {
    let mut transaction = pool.begin().await?;

    for entry in matches {
        sqlx::query(
            "INSERT INTO attributed_swap_txs (
                tx_hash,
                issued_swap_calldata_id,
                api_key_id,
                chain_id,
                sender,
                to_address,
                block_number,
                block_timestamp,
                input_token,
                output_token,
                total_input_amount,
                total_output_amount
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(tx_hash) DO UPDATE SET
                issued_swap_calldata_id = excluded.issued_swap_calldata_id,
                api_key_id = excluded.api_key_id,
                chain_id = excluded.chain_id,
                sender = excluded.sender,
                to_address = excluded.to_address,
                block_number = excluded.block_number,
                block_timestamp = excluded.block_timestamp,
                input_token = excluded.input_token,
                output_token = excluded.output_token,
                total_input_amount = excluded.total_input_amount,
                total_output_amount = excluded.total_output_amount",
        )
        .bind(&entry.tx_hash)
        .bind(entry.issued_swap_calldata_id)
        .bind(entry.api_key_id)
        .bind(crate::CHAIN_ID as i64)
        .bind(&entry.sender)
        .bind(&entry.to_address)
        .bind(entry.block_number as i64)
        .bind(entry.timestamp as i64)
        .bind(&entry.input_token)
        .bind(&entry.output_token)
        .bind(format_hex(&entry.total_input_hex)?)
        .bind(format_hex(&entry.total_output_hex)?)
        .execute(&mut *transaction)
        .await?;
    }

    transaction.commit().await?;
    Ok(())
}

async fn persist_report_run(
    pool: &DbPool,
    report: &CustomerVolumeReport,
) -> Result<(), CustomerVolumeReportError> {
    sqlx::query(
        "INSERT INTO swap_report_runs (
            report_name,
            start_time,
            end_time,
            matched_count,
            unmatched_count,
            ambiguous_count
        ) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("customer-volume")
    .bind(report.start_time as i64)
    .bind(report.end_time as i64)
    .bind(report.audit.matched_executions as i64)
    .bind(report.audit.unmatched_executions as i64)
    .bind(report.audit.ambiguous_matches as i64)
    .execute(pool)
    .await?;

    Ok(())
}

fn print_human_report(report: &CustomerVolumeReport) {
    println!("Customer volume report");
    println!("Window: {} -> {}", report.start_time, report.end_time);
    println!(
        "Matched executed txs: {} | Unmatched executed txs: {} | Ambiguous matches: {}",
        report.audit.matched_executions,
        report.audit.unmatched_executions,
        report.audit.ambiguous_matches
    );
    println!();

    if report.customers.is_empty() {
        println!("No matched customer executions found.");
        return;
    }

    for customer in &report.customers {
        println!(
            "{} ({}) [{}] executions={}",
            customer.label, customer.owner, customer.key_id, customer.executed_txs
        );

        for pair in &customer.pairs {
            println!(
                "  {} -> {} | input={} | output={}",
                pair.input_token,
                pair.output_token,
                pair.total_input_amount,
                pair.total_output_amount
            );
        }

        println!();
    }
}

fn normalize_address(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(address) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        format!("0x{}", address.to_ascii_lowercase())
    } else {
        format!("0x{}", trimmed.to_ascii_lowercase())
    }
}

fn normalize_hex(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        format!("0x{}", hex.to_ascii_lowercase())
    } else {
        format!("0x{}", trimmed.to_ascii_lowercase())
    }
}

fn zero_hex() -> Result<String, CustomerVolumeReportError> {
    Ok(Float::zero()?.as_hex().to_ascii_lowercase())
}

fn add_hex_amount(
    total_hex: &mut String,
    value_hex: &str,
) -> Result<(), CustomerVolumeReportError> {
    let total = Float::from_hex(total_hex)?;
    let value = Float::from_hex(value_hex)?;
    *total_hex = total.add(value)?.as_hex().to_ascii_lowercase();
    Ok(())
}

fn format_hex(value_hex: &str) -> Result<String, CustomerVolumeReportError> {
    Ok(Float::from_hex(value_hex)?.format()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    async fn test_pool() -> DbPool {
        let id = uuid::Uuid::new_v4();
        crate::db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("test database init")
    }

    async fn seed_match_context(pool: &DbPool, created_at_unix: i64) -> MatchedExecution {
        let key_id = uuid::Uuid::new_v4().to_string();
        let secret_hash = crate::auth::hash_secret("test-secret").expect("hash secret");
        let api_key_insert = sqlx::query(
            "INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, ?, ?, ?)",
        )
        .bind(&key_id)
        .bind(secret_hash)
        .bind("alpha")
        .bind("owner-a")
        .execute(pool)
        .await
        .expect("insert api key");
        let api_key_id = api_key_insert.last_insert_rowid();

        let issued_insert = sqlx::query(
            "INSERT INTO issued_swap_calldata (
                api_key_id,
                chain_id,
                taker,
                to_address,
                tx_value,
                calldata,
                calldata_hash,
                input_token,
                output_token,
                output_amount,
                maximum_io_ratio,
                estimated_input,
                created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime(?,'unixepoch'))",
        )
        .bind(api_key_id)
        .bind(crate::CHAIN_ID as i64)
        .bind("0x1111111111111111111111111111111111111111")
        .bind("0xd2938e7c9fe3597f78832ce780feb61945c377d7")
        .bind("0")
        .bind("0xabcdef")
        .bind("0xdeadbeef")
        .bind("0xinput")
        .bind("0xoutput")
        .bind("1")
        .bind("205")
        .bind("50")
        .bind(created_at_unix)
        .execute(pool)
        .await
        .expect("insert issued calldata");
        let issued_swap_calldata_id = issued_insert.last_insert_rowid();

        MatchedExecution {
            issued_swap_calldata_id,
            api_key_id,
            key_id,
            label: "alpha".to_string(),
            owner: "owner-a".to_string(),
            tx_hash: "0xtx1".to_string(),
            sender: "0x1111111111111111111111111111111111111111".to_string(),
            to_address: "0xd2938e7c9fe3597f78832ce780feb61945c377d7".to_string(),
            block_number: 10,
            timestamp: 100,
            input_token: "0xinput".to_string(),
            output_token: "0xoutput".to_string(),
            total_input_hex: Float::parse("50".to_string()).expect("float").as_hex(),
            total_output_hex: Float::parse("0.25".to_string()).expect("float").as_hex(),
            duplicate_match_count: 1,
        }
    }

    fn issued_row(
        id: i64,
        api_key_id: i64,
        label: &str,
        owner: &str,
        created_at_unix: i64,
    ) -> IssuedSwapRow {
        IssuedSwapRow {
            id,
            api_key_id,
            key_id: format!("key-{api_key_id}"),
            label: label.to_string(),
            owner: owner.to_string(),
            taker: "0x1111111111111111111111111111111111111111".to_string(),
            to_address: "0xd2938e7c9fe3597f78832ce780feb61945c377d7".to_string(),
            calldata: "0xabcdef".to_string(),
            input_token: "0xinput".to_string(),
            output_token: "0xoutput".to_string(),
            created_at_unix,
        }
    }

    fn candidate(tx_hash: &str, sender: &str, to: &str, timestamp: u64) -> CandidateExecution {
        CandidateExecution {
            tx_hash: tx_hash.to_string(),
            orderbook_address: to.to_string(),
            sender: sender.to_string(),
            block_number: 1,
            timestamp,
            total_input_hex: Float::parse("10".to_string()).expect("float").as_hex(),
            total_output_hex: Float::parse("2".to_string()).expect("float").as_hex(),
        }
    }

    #[test]
    fn test_aggregate_candidate_rows_sums_same_transaction() {
        let rows = vec![
            TakeOrderRow {
                orderbook_address: "0xd2938e7c9fe3597f78832ce780feb61945c377d7".to_string(),
                transaction_hash:
                    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                block_number: 10,
                block_timestamp: 100,
                sender: "0x1111111111111111111111111111111111111111".to_string(),
                taker_input: Float::parse("0.25".to_string()).expect("float").as_hex(),
                taker_output: Float::parse("50".to_string()).expect("float").as_hex(),
            },
            TakeOrderRow {
                orderbook_address: "0xd2938e7c9fe3597f78832ce780feb61945c377d7".to_string(),
                transaction_hash:
                    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                block_number: 10,
                block_timestamp: 100,
                sender: "0x1111111111111111111111111111111111111111".to_string(),
                taker_input: Float::parse("0.20".to_string()).expect("float").as_hex(),
                taker_output: Float::parse("40".to_string()).expect("float").as_hex(),
            },
        ];

        let aggregated = aggregate_candidate_rows(rows).expect("aggregate");
        assert_eq!(aggregated.len(), 1);
        assert_eq!(
            format_hex(&aggregated[0].total_input_hex).expect("format"),
            "90"
        );
        assert_eq!(
            format_hex(&aggregated[0].total_output_hex).expect("format"),
            "0.45"
        );
    }

    #[test]
    fn test_select_issued_row_requires_exact_match_and_window() {
        let rows = vec![
            issued_row(1, 1, "alpha", "owner-a", 100),
            issued_row(2, 2, "beta", "owner-b", 10),
        ];
        let issued_row_index = build_issued_row_index(&rows);

        let selection = select_issued_row(
            &candidate(
                "0xtx",
                "0x1111111111111111111111111111111111111111",
                "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                350,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert_eq!(selection.row.map(|row| row.id), Some(1));
        assert_eq!(selection.duplicate_match_count, 1);
    }

    #[test]
    fn test_select_issued_row_prefers_latest_matching_issuance() {
        let rows = vec![
            issued_row(1, 1, "alpha", "owner-a", 100),
            issued_row(2, 2, "beta", "owner-b", 200),
        ];
        let issued_row_index = build_issued_row_index(&rows);

        let selection = select_issued_row(
            &candidate(
                "0xtx",
                "0x1111111111111111111111111111111111111111",
                "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                240,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert_eq!(selection.row.map(|row| row.id), Some(2));
        assert_eq!(selection.duplicate_match_count, 2);
    }

    #[test]
    fn test_build_customer_summaries_groups_by_customer_and_pair() {
        let executions = vec![
            MatchedExecution {
                issued_swap_calldata_id: 1,
                api_key_id: 1,
                key_id: "key-1".to_string(),
                label: "alpha".to_string(),
                owner: "owner-a".to_string(),
                tx_hash: "0xtx1".to_string(),
                sender: "0x1111111111111111111111111111111111111111".to_string(),
                to_address: "0xd2938e7c9fe3597f78832ce780feb61945c377d7".to_string(),
                block_number: 1,
                timestamp: 10,
                input_token: "0xinput".to_string(),
                output_token: "0xoutput".to_string(),
                total_input_hex: Float::parse("50".to_string()).expect("float").as_hex(),
                total_output_hex: Float::parse("0.25".to_string()).expect("float").as_hex(),
                duplicate_match_count: 1,
            },
            MatchedExecution {
                issued_swap_calldata_id: 2,
                api_key_id: 1,
                key_id: "key-1".to_string(),
                label: "alpha".to_string(),
                owner: "owner-a".to_string(),
                tx_hash: "0xtx2".to_string(),
                sender: "0x1111111111111111111111111111111111111111".to_string(),
                to_address: "0xd2938e7c9fe3597f78832ce780feb61945c377d7".to_string(),
                block_number: 2,
                timestamp: 11,
                input_token: "0xinput".to_string(),
                output_token: "0xoutput".to_string(),
                total_input_hex: Float::parse("40".to_string()).expect("float").as_hex(),
                total_output_hex: Float::parse("0.20".to_string()).expect("float").as_hex(),
                duplicate_match_count: 1,
            },
        ];

        let customers = build_customer_summaries(&executions).expect("summary");
        assert_eq!(customers.len(), 1);
        assert_eq!(customers[0].executed_txs, 2);
        assert_eq!(customers[0].pairs.len(), 1);
        assert_eq!(customers[0].pairs[0].total_input_amount, "90");
        assert_eq!(customers[0].pairs[0].total_output_amount, "0.45");
    }

    async fn mock_rpc_url_for_input(input: &str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock rpc server");
        let addr = listener.local_addr().expect("mock rpc addr");
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "input": input,
            }
        })
        .to_string();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };

                let body = body.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ =
                        tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
                });
            }
        });

        format!("http://{addr}")
    }

    async fn seed_raindex_take_orders(
        db_path: &std::path::Path,
        tx_hash: &str,
        block_timestamp: i64,
    ) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("open test raindex db");

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS take_orders (
                chain_id INTEGER NOT NULL,
                orderbook_address TEXT NOT NULL,
                transaction_hash TEXT NOT NULL,
                block_number INTEGER NOT NULL,
                block_timestamp INTEGER NOT NULL,
                sender TEXT NOT NULL,
                taker_input TEXT NOT NULL,
                taker_output TEXT NOT NULL,
                log_index INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create take_orders table");

        sqlx::query(
            "INSERT INTO take_orders (
                chain_id,
                orderbook_address,
                transaction_hash,
                block_number,
                block_timestamp,
                sender,
                taker_input,
                taker_output,
                log_index
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(crate::CHAIN_ID as i64)
        .bind("0xd2938e7c9fe3597f78832ce780feb61945c377d7")
        .bind(tx_hash)
        .bind(10_i64)
        .bind(block_timestamp)
        .bind("0x1111111111111111111111111111111111111111")
        .bind(Float::parse("0.25".to_string()).expect("float").as_hex())
        .bind(Float::parse("50".to_string()).expect("float").as_hex())
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("insert take order");
    }

    #[tokio::test]
    async fn test_run_reconciles_and_persists_customer_volume_report() {
        let pool = test_pool().await;
        let expected = seed_match_context(&pool, 100).await;
        let tx_hash = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders(&db_path, tx_hash, 105).await;

        let rpc_url = mock_rpc_url_for_input("0xabcdef").await;
        let settings = format!(
            "version: 4
networks:
  base:
    rpcs:
      - {rpc_url}
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://example.com/subgraph
orderbooks:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
tokens:
  token1:
    address: 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
    network: base
"
        );
        let registry_url =
            crate::test_helpers::mock_raindex_registry_url_with_settings(&settings).await;
        let raindex = crate::raindex::RaindexProvider::load(&registry_url, Some(db_path))
            .await
            .expect("load test raindex provider");

        run(
            &pool,
            &raindex,
            CustomerVolumeReportArgs {
                start_time: 90,
                end_time: 110,
                json: true,
            },
        )
        .await
        .expect("run report");

        let attributed = sqlx::query(
            "SELECT tx_hash, issued_swap_calldata_id, api_key_id, total_input_amount, total_output_amount
             FROM attributed_swap_txs",
        )
        .fetch_one(&pool)
        .await
        .expect("fetch attributed tx");

        assert_eq!(attributed.get::<String, _>("tx_hash"), tx_hash);
        assert_eq!(
            attributed.get::<i64, _>("issued_swap_calldata_id"),
            expected.issued_swap_calldata_id
        );
        assert_eq!(attributed.get::<i64, _>("api_key_id"), expected.api_key_id);
        assert_eq!(attributed.get::<String, _>("total_input_amount"), "50");
        assert_eq!(attributed.get::<String, _>("total_output_amount"), "0.25");

        let run_counts = sqlx::query(
            "SELECT matched_count, unmatched_count, ambiguous_count FROM swap_report_runs",
        )
        .fetch_one(&pool)
        .await
        .expect("fetch report run");
        assert_eq!(run_counts.get::<i64, _>("matched_count"), 1);
        assert_eq!(run_counts.get::<i64, _>("unmatched_count"), 0);
        assert_eq!(run_counts.get::<i64, _>("ambiguous_count"), 0);

        run(
            &pool,
            &raindex,
            CustomerVolumeReportArgs {
                start_time: 90,
                end_time: 110,
                json: true,
            },
        )
        .await
        .expect("rerun report");

        let attributed_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attributed_swap_txs")
            .fetch_one(&pool)
            .await
            .expect("count attributed txs");
        let report_run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM swap_report_runs")
            .fetch_one(&pool)
            .await
            .expect("count report runs");

        assert_eq!(attributed_count, 1);
        assert_eq!(report_run_count, 2);
    }

    #[tokio::test]
    async fn test_persist_matches_is_idempotent() {
        let pool = test_pool().await;
        let mut execution = seed_match_context(&pool, 100).await;

        persist_matches(&pool, &[execution.clone()])
            .await
            .expect("persist match");

        execution.total_input_hex = Float::parse("60".to_string()).expect("float").as_hex();
        execution.total_output_hex = Float::parse("0.30".to_string()).expect("float").as_hex();

        persist_matches(&pool, &[execution])
            .await
            .expect("persist updated match");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attributed_swap_txs")
            .fetch_one(&pool)
            .await
            .expect("count matches");
        assert_eq!(count, 1);

        let amounts: (String, String) = sqlx::query_as(
            "SELECT total_input_amount, total_output_amount FROM attributed_swap_txs WHERE tx_hash = ?",
        )
        .bind("0xtx1")
        .fetch_one(&pool)
        .await
        .expect("fetch persisted amounts");
        assert_eq!(amounts.0, "60");
        assert_eq!(amounts.1, "0.3");
    }

    #[tokio::test]
    async fn test_persist_report_run_records_audit_counts() {
        let pool = test_pool().await;
        let report = CustomerVolumeReport {
            start_time: 10,
            end_time: 20,
            audit: ReportAudit {
                matched_executions: 2,
                unmatched_executions: 1,
                ambiguous_matches: 1,
            },
            customers: Vec::new(),
            matched_transactions: Vec::new(),
            unmatched_transactions: Vec::new(),
        };

        persist_report_run(&pool, &report)
            .await
            .expect("persist report run");

        let stored: (String, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT report_name, start_time, end_time, matched_count, unmatched_count, ambiguous_count
             FROM swap_report_runs",
        )
        .fetch_one(&pool)
        .await
        .expect("fetch report run");

        assert_eq!(stored.0, "customer-volume");
        assert_eq!(stored.1, 10);
        assert_eq!(stored.2, 20);
        assert_eq!(stored.3, 2);
        assert_eq!(stored.4, 1);
        assert_eq!(stored.5, 1);
    }
}
