use crate::db::DbPool;
use crate::raindex::RaindexProvider;
use futures::stream::{self, StreamExt};
use rain_math_float::Float;
use rain_orderbook_common::raindex_client::RaindexError;
use reqwest::Client as HttpClient;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, QueryBuilder};
use std::collections::{BTreeMap, HashMap};
use std::ops::Add;
use std::time::Duration;

const MATCH_WINDOW_SECS: i64 = 2 * 60;
const RPC_LOOKUP_CONCURRENCY: usize = 8;
const RPC_LOOKUP_TIMEOUT_SECS: u64 = 5;
const RPC_LOOKUP_MAX_ATTEMPTS: u32 = 3;
const RPC_LOOKUP_RETRY_BASE_DELAY_MS: u64 = 200;

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
    rpc_lookup_failures: usize,
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
struct AmbiguousMatchCandidateReport {
    api_key_id: i64,
    key_id: String,
    label: String,
    owner: String,
}

#[derive(Debug, Clone, Serialize)]
struct AmbiguousTransactionReport {
    tx_hash: String,
    sender: String,
    to_address: String,
    block_number: u64,
    timestamp: u64,
    total_input_amount: String,
    total_output_amount: String,
    candidates: Vec<AmbiguousMatchCandidateReport>,
}

#[derive(Debug, Clone, Serialize)]
struct RpcLookupFailureReport {
    tx_hash: String,
    sender: String,
    to_address: String,
    block_number: u64,
    timestamp: u64,
    error: String,
}

impl RpcLookupFailureReport {
    fn from_error(candidate: &CandidateExecution, error: CustomerVolumeReportError) -> Self {
        let (tx_hash, error) = match error {
            CustomerVolumeReportError::RpcLookup { tx_hash, message } => (tx_hash, message),
            CustomerVolumeReportError::TransactionNotFound { tx_hash } => {
                (tx_hash, "transaction not found via RPC".to_string())
            }
            other => (candidate.tx_hash.clone(), other.to_string()),
        };

        Self {
            tx_hash,
            sender: candidate.sender.clone(),
            to_address: candidate.orderbook_address.clone(),
            block_number: candidate.block_number,
            timestamp: candidate.timestamp,
            error,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct CustomerVolumeReport {
    start_time: u64,
    end_time: u64,
    audit: ReportAudit,
    customers: Vec<CustomerVolumeSummary>,
    matched_transactions: Vec<MatchedTransactionReport>,
    unmatched_transactions: Vec<UnmatchedTransactionReport>,
    ambiguous_transactions: Vec<AmbiguousTransactionReport>,
    rpc_lookup_failures: Vec<RpcLookupFailureReport>,
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

    let report = build_report(pool, raindex, args.start_time, args.end_time).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }

    tracing::info!(
        matched = report.audit.matched_executions,
        unmatched = report.audit.unmatched_executions,
        ambiguous = report.audit.ambiguous_matches,
        rpc_lookup_failures = report.audit.rpc_lookup_failures,
        "customer volume report complete"
    );

    Ok(())
}

async fn build_report(
    pool: &DbPool,
    raindex: &RaindexProvider,
    start_time: u64,
    end_time: u64,
) -> Result<CustomerVolumeReport, CustomerVolumeReportError> {
    let issued_rows = load_issued_rows(pool, start_time, end_time).await?;
    let issued_row_index = build_issued_row_index(&issued_rows);

    tracing::info!(
        issued_count = issued_rows.len(),
        "loaded issued swap rows for customer volume report"
    );

    let raindex_db = open_raindex_db(raindex).await?;
    let candidate_rows = load_take_order_rows(&raindex_db, start_time, end_time).await?;
    let candidates = aggregate_candidate_rows(candidate_rows)?;
    let rpc_urls = rpc_urls_for_chain(raindex)?;
    let rpc = HttpClient::builder()
        .timeout(Duration::from_secs(RPC_LOOKUP_TIMEOUT_SECS))
        .build()
        .map_err(CustomerVolumeReportError::Http)?;

    tracing::info!(
        candidate_count = candidates.len(),
        rpc_url_count = rpc_urls.len(),
        "loaded candidate executions from raindex db"
    );

    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    let mut ambiguous_matches = 0usize;
    let mut ambiguous_transactions = Vec::new();
    let mut rpc_lookup_failures = Vec::new();

    let mut lookup_results = stream::iter(candidates.into_iter().map(|candidate| {
        let rpc = rpc.clone();
        let rpc_urls = rpc_urls.clone();

        async move {
            let tx_input = fetch_transaction_input(&rpc, &rpc_urls, &candidate.tx_hash).await;
            (candidate, tx_input)
        }
    }))
    .buffered(RPC_LOOKUP_CONCURRENCY);

    while let Some((candidate, tx_input)) = lookup_results.next().await {
        let tx_input = match tx_input {
            Ok(tx_input) => tx_input,
            Err(error @ CustomerVolumeReportError::RpcLookup { .. })
            | Err(error @ CustomerVolumeReportError::TransactionNotFound { .. }) => {
                let failure = RpcLookupFailureReport::from_error(&candidate, error);
                tracing::warn!(
                    tx_hash = %failure.tx_hash,
                    sender = %failure.sender,
                    to = %failure.to_address,
                    error = %failure.error,
                    "skipping candidate execution after RPC lookup failure"
                );
                rpc_lookup_failures.push(failure);
                continue;
            }
            Err(error) => return Err(error),
        };
        let selection = select_issued_row(&candidate, &tx_input, &issued_row_index);

        if selection.distinct_match_count > 1 {
            ambiguous_matches += 1;
            ambiguous_transactions.push(AmbiguousTransactionReport {
                tx_hash: candidate.tx_hash.clone(),
                sender: candidate.sender.clone(),
                to_address: candidate.orderbook_address.clone(),
                block_number: candidate.block_number,
                timestamp: candidate.timestamp,
                total_input_amount: format_hex(&candidate.total_input_hex)?,
                total_output_amount: format_hex(&candidate.total_output_hex)?,
                candidates: selection
                    .distinct_matches
                    .iter()
                    .map(|row| AmbiguousMatchCandidateReport {
                        api_key_id: row.api_key_id,
                        key_id: row.key_id.clone(),
                        label: row.label.clone(),
                        owner: row.owner.clone(),
                    })
                    .collect(),
            });
            continue;
        }

        match selection.row {
            Some(row) => matched.push(MatchedExecution {
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
                duplicate_match_count: selection.matching_row_count,
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

    Ok(CustomerVolumeReport {
        start_time,
        end_time,
        audit: ReportAudit {
            matched_executions: matched_transactions.len(),
            unmatched_executions: unmatched.len(),
            ambiguous_matches,
            rpc_lookup_failures: rpc_lookup_failures.len(),
        },
        customers,
        matched_transactions,
        unmatched_transactions: unmatched,
        ambiguous_transactions,
        rpc_lookup_failures,
    })
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
            isc.key_id,
            isc.label,
            isc.owner,
            isc.taker,
            isc.to_address,
            isc.calldata,
            isc.input_token,
            isc.output_token,
            CAST(strftime('%s', isc.created_at) AS INTEGER) AS created_at_unix
         FROM issued_swap_calldata isc
         WHERE isc.chain_id = ?
           AND isc.created_at >= datetime(?,'unixepoch')
           AND isc.created_at <= datetime(?,'unixepoch')
         ORDER BY isc.created_at DESC, isc.id DESC",
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
        .push(" ORDER BY block_number, log_index");

    query_builder
        .build_query_as::<TakeOrderRow>()
        .fetch_all(raindex_db)
        .await
        .map_err(CustomerVolumeReportError::Database)
}

fn aggregate_candidate_rows(
    rows: Vec<TakeOrderRow>,
) -> Result<Vec<CandidateExecution>, CustomerVolumeReportError> {
    let mut aggregates: HashMap<(String, String), CandidateExecution> = HashMap::new();

    for row in rows {
        let tx_hash = normalize_hex(&row.transaction_hash);
        let sender = normalize_address(&row.sender);
        let orderbook_address = normalize_address(&row.orderbook_address);
        let input_delta = normalize_hex(&row.taker_output);
        let output_delta = normalize_hex(&row.taker_input);

        let aggregate_key = (tx_hash.clone(), orderbook_address.clone());

        match aggregates.get_mut(&aggregate_key) {
            Some(existing) => {
                if existing.sender != sender
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
                    aggregate_key,
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
            .then_with(|| left.orderbook_address.cmp(&right.orderbook_address))
    });
    Ok(candidates)
}

struct Selection<'a> {
    row: Option<&'a IssuedSwapRow>,
    matching_row_count: usize,
    distinct_match_count: usize,
    distinct_matches: Vec<&'a IssuedSwapRow>,
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
                matching_row_count: 0,
                distinct_match_count: 0,
                distinct_matches: Vec::new(),
            };
        }
    };

    let mut matched_row = None;
    let mut matching_row_count = 0;
    let mut distinct_matches = Vec::new();
    let mut seen_api_keys = std::collections::HashSet::new();

    for row in issued_rows {
        if row.created_at_unix > candidate_timestamp {
            continue;
        }

        if candidate_timestamp <= row.created_at_unix + MATCH_WINDOW_SECS {
            matching_row_count += 1;
            if matched_row.is_none() {
                matched_row = Some(*row);
            }

            if seen_api_keys.insert(row.api_key_id) {
                distinct_matches.push(*row);
            }
            continue;
        }

        break;
    }

    let row = if distinct_matches.len() == 1 {
        matched_row
    } else {
        None
    };

    Selection {
        row,
        matching_row_count,
        distinct_match_count: distinct_matches.len(),
        distinct_matches,
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
        for attempt in 1..=RPC_LOOKUP_MAX_ATTEMPTS {
            let response = match client.post(rpc_url.clone()).json(&payload).send().await {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(error.to_string());
                    sleep_before_retry(attempt).await;
                    continue;
                }
            };

            if !response.status().is_success() {
                last_error = Some(format!("rpc status {}", response.status()));
                sleep_before_retry(attempt).await;
                continue;
            }

            let envelope = match response
                .json::<JsonRpcEnvelope<RpcTransactionResponse>>()
                .await
            {
                Ok(envelope) => envelope,
                Err(error) => {
                    last_error = Some(error.to_string());
                    sleep_before_retry(attempt).await;
                    continue;
                }
            };

            if let Some(result) = envelope.result {
                return Ok(normalize_hex(&result.input));
            }

            if let Some(error) = envelope.error {
                last_error = Some(error.message);
            }

            break;
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

async fn sleep_before_retry(attempt: u32) {
    if attempt >= RPC_LOOKUP_MAX_ATTEMPTS {
        return;
    }

    tokio::time::sleep(Duration::from_millis(
        RPC_LOOKUP_RETRY_BASE_DELAY_MS * u64::from(attempt),
    ))
    .await;
}

fn print_human_report(report: &CustomerVolumeReport) {
    println!("Customer volume report");
    println!("Window: {} -> {}", report.start_time, report.end_time);
    println!(
        "Matched executed txs: {} | Unmatched executed txs: {} | Ambiguous matches: {} | RPC lookup failures: {}",
        report.audit.matched_executions,
        report.audit.unmatched_executions,
        report.audit.ambiguous_matches,
        report.audit.rpc_lookup_failures
    );
    println!();

    if !report.rpc_lookup_failures.is_empty() {
        println!("RPC lookup failures:");
        for failure in &report.rpc_lookup_failures {
            println!(
                "  {} | sender={} | to={} | block={} | timestamp={} | error={}",
                failure.tx_hash,
                failure.sender,
                failure.to_address,
                failure.block_number,
                failure.timestamp,
                failure.error
            );
        }
        println!();
    }

    if !report.ambiguous_transactions.is_empty() {
        println!("Ambiguous transactions:");
        for transaction in &report.ambiguous_transactions {
            println!(
                "  {} | sender={} | to={} | block={} | timestamp={} | input={} | output={}",
                transaction.tx_hash,
                transaction.sender,
                transaction.to_address,
                transaction.block_number,
                transaction.timestamp,
                transaction.total_input_amount,
                transaction.total_output_amount
            );
            for candidate in &transaction.candidates {
                println!(
                    "    candidate api_key_id={} key_id={} label={} owner={}",
                    candidate.api_key_id, candidate.key_id, candidate.label, candidate.owner
                );
            }
        }
        println!();
    }

    if report.customers.is_empty() {
        println!("No matched customer executions found.");
    } else {
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
}

fn normalize_address(value: &str) -> String {
    normalize_prefixed_hex(value)
}

fn normalize_hex(value: &str) -> String {
    normalize_prefixed_hex(value)
}

fn normalize_prefixed_hex(value: &str) -> String {
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Debug)]
    struct SeededMatchContext {
        api_key_id: i64,
        key_id: String,
    }

    async fn test_pool() -> DbPool {
        let id = uuid::Uuid::new_v4();
        crate::db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("test database init")
    }

    async fn seed_match_context(pool: &DbPool, created_at_unix: i64) -> SeededMatchContext {
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

        sqlx::query(
            "INSERT INTO issued_swap_calldata (
                api_key_id,
                key_id,
                label,
                owner,
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime(?,'unixepoch'))",
        )
        .bind(api_key_id)
        .bind(&key_id)
        .bind("alpha")
        .bind("owner-a")
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

        SeededMatchContext { api_key_id, key_id }
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
    fn test_aggregate_candidate_rows_keeps_same_tx_separate_per_orderbook() {
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
                orderbook_address: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
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
        assert_eq!(aggregated.len(), 2);
        assert_eq!(
            aggregated[0].orderbook_address,
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            aggregated[1].orderbook_address,
            "0xd2938e7c9fe3597f78832ce780feb61945c377d7"
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
                180,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert_eq!(selection.row.map(|row| row.id), Some(1));
        assert_eq!(selection.matching_row_count, 1);
        assert_eq!(selection.distinct_match_count, 1);
    }

    #[test]
    fn test_select_issued_row_rejects_future_issuance() {
        let rows = vec![issued_row(1, 1, "alpha", "owner-a", 200)];
        let issued_row_index = build_issued_row_index(&rows);

        let selection = select_issued_row(
            &candidate(
                "0xtx",
                "0x1111111111111111111111111111111111111111",
                "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                199,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert!(selection.row.is_none());
        assert_eq!(selection.matching_row_count, 0);
        assert_eq!(selection.distinct_match_count, 0);
    }

    #[test]
    fn test_select_issued_row_accepts_same_timestamp_issuance() {
        let rows = vec![issued_row(1, 1, "alpha", "owner-a", 100)];
        let issued_row_index = build_issued_row_index(&rows);

        let selection = select_issued_row(
            &candidate(
                "0xtx",
                "0x1111111111111111111111111111111111111111",
                "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                100,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert_eq!(selection.row.map(|row| row.id), Some(1));
        assert_eq!(selection.matching_row_count, 1);
        assert_eq!(selection.distinct_match_count, 1);
    }

    #[test]
    fn test_select_issued_row_accepts_exact_match_window_boundary() {
        let rows = vec![issued_row(1, 1, "alpha", "owner-a", 100)];
        let issued_row_index = build_issued_row_index(&rows);

        let selection = select_issued_row(
            &candidate(
                "0xtx",
                "0x1111111111111111111111111111111111111111",
                "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                220,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert_eq!(selection.row.map(|row| row.id), Some(1));
        assert_eq!(selection.matching_row_count, 1);
        assert_eq!(selection.distinct_match_count, 1);
    }

    #[test]
    fn test_select_issued_row_rejects_after_match_window_boundary() {
        let rows = vec![issued_row(1, 1, "alpha", "owner-a", 100)];
        let issued_row_index = build_issued_row_index(&rows);

        let selection = select_issued_row(
            &candidate(
                "0xtx",
                "0x1111111111111111111111111111111111111111",
                "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                221,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert!(selection.row.is_none());
        assert_eq!(selection.matching_row_count, 0);
        assert_eq!(selection.distinct_match_count, 0);
    }

    #[test]
    fn test_select_issued_row_returns_none_for_ambiguous_distinct_keys() {
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
                210,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert!(selection.row.is_none());
        assert_eq!(selection.matching_row_count, 2);
        assert_eq!(selection.distinct_match_count, 2);
    }

    #[test]
    fn test_select_issued_row_treats_same_key_duplicates_as_single_match() {
        let rows = vec![
            issued_row(1, 1, "alpha", "owner-a", 100),
            issued_row(2, 1, "alpha", "owner-a", 200),
        ];
        let issued_row_index = build_issued_row_index(&rows);

        let selection = select_issued_row(
            &candidate(
                "0xtx",
                "0x1111111111111111111111111111111111111111",
                "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                210,
            ),
            "0xabcdef",
            &issued_row_index,
        );

        assert_eq!(selection.row.map(|row| row.id), Some(2));
        assert_eq!(selection.matching_row_count, 2);
        assert_eq!(selection.distinct_match_count, 1);
    }

    #[test]
    fn test_build_customer_summaries_groups_by_customer_and_pair() {
        let executions = vec![
            MatchedExecution {
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

    async fn mock_rpc_url_with_body(body: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock rpc server");
        let addr = listener.local_addr().expect("mock rpc addr");

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

    async fn mock_rpc_url_with_bodies(bodies: Vec<String>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock rpc server");
        let addr = listener.local_addr().expect("mock rpc addr");
        let bodies = Arc::new(bodies);
        let next_index = Arc::new(AtomicUsize::new(0));

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };

                let bodies = Arc::clone(&bodies);
                let next_index = Arc::clone(&next_index);

                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer).await;

                    let index = next_index.fetch_add(1, Ordering::SeqCst);
                    let body = bodies
                        .get(index)
                        .or_else(|| bodies.last())
                        .cloned()
                        .expect("at least one mock rpc body");

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

    async fn mock_rpc_url_for_input(input: &str) -> String {
        mock_rpc_url_with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "input": input,
                }
            })
            .to_string(),
        )
        .await
    }

    async fn mock_rpc_url_for_missing_transaction() -> String {
        mock_rpc_url_with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": serde_json::Value::Null,
            })
            .to_string(),
        )
        .await
    }

    async fn mock_rpc_url_for_rpc_error(message: &str) -> String {
        mock_rpc_url_with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "message": message,
                }
            })
            .to_string(),
        )
        .await
    }

    async fn seed_raindex_take_orders(
        db_path: &std::path::Path,
        tx_hash: &str,
        block_timestamp: i64,
    ) {
        seed_raindex_take_orders_for_orderbook(
            db_path,
            tx_hash,
            block_timestamp,
            "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
        )
        .await;
    }

    async fn seed_raindex_take_orders_for_orderbook(
        db_path: &std::path::Path,
        tx_hash: &str,
        block_timestamp: i64,
        orderbook_address: &str,
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
        .bind(orderbook_address)
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

    fn test_registry_settings(rpc_url: &str) -> String {
        test_registry_settings_with_orderbook(rpc_url, "0xd2938e7c9fe3597f78832ce780feb61945c377d7")
    }

    fn test_registry_settings_with_orderbook(rpc_url: &str, orderbook_address: &str) -> String {
        format!(
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
    address: {orderbook_address}
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
        )
    }

    async fn load_test_raindex(
        db_path: std::path::PathBuf,
        tx_input: &str,
    ) -> crate::raindex::RaindexProvider {
        let rpc_url = mock_rpc_url_for_input(tx_input).await;
        load_test_raindex_with_rpc_url(db_path, &rpc_url).await
    }

    async fn load_test_raindex_with_rpc_url(
        db_path: std::path::PathBuf,
        rpc_url: &str,
    ) -> crate::raindex::RaindexProvider {
        let settings = test_registry_settings(rpc_url);
        let registry_url =
            crate::test_helpers::mock_raindex_registry_url_with_settings(&settings).await;

        crate::raindex::RaindexProvider::load(&registry_url, Some(db_path))
            .await
            .expect("load test raindex provider")
    }

    async fn load_test_raindex_with_rpc_url_and_orderbook(
        db_path: std::path::PathBuf,
        rpc_url: &str,
        orderbook_address: &str,
    ) -> crate::raindex::RaindexProvider {
        let settings = test_registry_settings_with_orderbook(rpc_url, orderbook_address);
        let registry_url =
            crate::test_helpers::mock_raindex_registry_url_with_settings(&settings).await;

        crate::raindex::RaindexProvider::load(&registry_url, Some(db_path))
            .await
            .expect("load test raindex provider")
    }

    #[tokio::test]
    async fn test_build_report_reconciles_customer_volume_report() {
        let pool = test_pool().await;
        let expected = seed_match_context(&pool, 100).await;
        let tx_hash = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders(&db_path, tx_hash, 105).await;

        let raindex = load_test_raindex(db_path, "0xabcdef").await;

        let report = build_report(&pool, &raindex, 90, 110)
            .await
            .expect("build report");

        assert_eq!(report.start_time, 90);
        assert_eq!(report.end_time, 110);
        assert_eq!(report.audit.matched_executions, 1);
        assert_eq!(report.audit.unmatched_executions, 0);
        assert_eq!(report.audit.ambiguous_matches, 0);
        assert_eq!(report.audit.rpc_lookup_failures, 0);
        assert_eq!(report.customers.len(), 1);
        assert_eq!(report.customers[0].api_key_id, expected.api_key_id);
        assert_eq!(report.customers[0].key_id, expected.key_id);
        assert_eq!(report.customers[0].label, "alpha");
        assert_eq!(report.customers[0].owner, "owner-a");
        assert_eq!(report.customers[0].executed_txs, 1);
        assert_eq!(report.customers[0].pairs.len(), 1);
        assert_eq!(report.customers[0].pairs[0].input_token, "0xinput");
        assert_eq!(report.customers[0].pairs[0].output_token, "0xoutput");
        assert_eq!(report.customers[0].pairs[0].total_input_amount, "50");
        assert_eq!(report.customers[0].pairs[0].total_output_amount, "0.25");
        assert_eq!(report.matched_transactions.len(), 1);
        assert_eq!(report.matched_transactions[0].tx_hash, tx_hash);
        assert_eq!(
            report.matched_transactions[0].api_key_id,
            expected.api_key_id
        );
        assert_eq!(report.matched_transactions[0].key_id, expected.key_id);
        assert_eq!(
            report.matched_transactions[0].sender,
            "0x1111111111111111111111111111111111111111"
        );
        assert_eq!(
            report.matched_transactions[0].to_address,
            "0xd2938e7c9fe3597f78832ce780feb61945c377d7"
        );
        assert_eq!(report.matched_transactions[0].input_token, "0xinput");
        assert_eq!(report.matched_transactions[0].output_token, "0xoutput");
        assert_eq!(report.matched_transactions[0].total_input_amount, "50");
        assert_eq!(report.matched_transactions[0].total_output_amount, "0.25");
        assert_eq!(report.unmatched_transactions.len(), 0);
        assert!(report.ambiguous_transactions.is_empty());
        assert!(report.rpc_lookup_failures.is_empty());
    }

    #[tokio::test]
    async fn test_build_report_uses_issued_snapshot_after_api_key_deletion() {
        let pool = test_pool().await;
        let expected = seed_match_context(&pool, 100).await;
        let tx_hash = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(expected.api_key_id)
            .execute(&pool)
            .await
            .expect("delete api key");

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders(&db_path, tx_hash, 105).await;

        let raindex = load_test_raindex(db_path, "0xabcdef").await;

        let report = build_report(&pool, &raindex, 90, 110)
            .await
            .expect("build report");

        assert_eq!(report.customers.len(), 1);
        assert_eq!(report.customers[0].api_key_id, expected.api_key_id);
        assert_eq!(report.customers[0].key_id, expected.key_id);
        assert_eq!(report.customers[0].label, "alpha");
        assert_eq!(report.customers[0].owner, "owner-a");
        assert_eq!(report.matched_transactions.len(), 1);
        assert_eq!(
            report.matched_transactions[0].key_id,
            report.customers[0].key_id
        );
        assert!(report.ambiguous_transactions.is_empty());
        assert!(report.rpc_lookup_failures.is_empty());
    }

    #[tokio::test]
    async fn test_build_report_uses_chain_history_not_current_orderbook_registry() {
        let pool = test_pool().await;
        let expected = seed_match_context(&pool, 100).await;
        let tx_hash = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders_for_orderbook(
            &db_path,
            tx_hash,
            105,
            "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
        )
        .await;

        let rpc_url = mock_rpc_url_for_input("0xabcdef").await;
        let raindex = load_test_raindex_with_rpc_url_and_orderbook(
            db_path,
            &rpc_url,
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .await;

        let report = build_report(&pool, &raindex, 90, 110)
            .await
            .expect("build report");

        assert_eq!(report.audit.matched_executions, 1);
        assert_eq!(report.matched_transactions.len(), 1);
        assert_eq!(report.matched_transactions[0].tx_hash, tx_hash);
        assert_eq!(
            report.matched_transactions[0].api_key_id,
            expected.api_key_id
        );
        assert!(report.unmatched_transactions.is_empty());
        assert!(report.ambiguous_transactions.is_empty());
        assert!(report.rpc_lookup_failures.is_empty());
    }

    #[tokio::test]
    async fn test_build_report_does_not_match_when_execution_precedes_issuance() {
        let pool = test_pool().await;
        seed_match_context(&pool, 105).await;
        let tx_hash = "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders(&db_path, tx_hash, 90).await;

        let raindex = load_test_raindex(db_path, "0xabcdef").await;

        let report = build_report(&pool, &raindex, 80, 110)
            .await
            .expect("build report");

        assert_eq!(report.audit.matched_executions, 0);
        assert_eq!(report.matched_transactions.len(), 0);
        assert_eq!(report.audit.unmatched_executions, 1);
        assert_eq!(report.unmatched_transactions.len(), 1);
        assert_eq!(report.unmatched_transactions[0].tx_hash, tx_hash);
        assert!(report.ambiguous_transactions.is_empty());
        assert!(report.rpc_lookup_failures.is_empty());
    }

    #[tokio::test]
    async fn test_build_report_excludes_issued_rows_after_execution() {
        let pool = test_pool().await;
        seed_match_context(&pool, 111).await;
        let tx_hash = "0xabababababababababababababababababababababababababababababababab";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders(&db_path, tx_hash, 110).await;

        let raindex = load_test_raindex(db_path, "0xabcdef").await;

        let report = build_report(&pool, &raindex, 90, 110)
            .await
            .expect("build report");

        assert_eq!(report.audit.matched_executions, 0);
        assert_eq!(report.matched_transactions.len(), 0);
        assert_eq!(report.audit.unmatched_executions, 1);
        assert_eq!(report.unmatched_transactions.len(), 1);
        assert_eq!(report.unmatched_transactions[0].tx_hash, tx_hash);
        assert!(report.ambiguous_transactions.is_empty());
        assert!(report.rpc_lookup_failures.is_empty());
    }

    #[tokio::test]
    async fn test_build_report_keeps_same_tx_hash_separate_per_orderbook() {
        let pool = test_pool().await;
        let expected = seed_match_context(&pool, 100).await;
        let tx_hash = "0xedededededededededededededededededededededededededededededededed";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders_for_orderbook(
            &db_path,
            tx_hash,
            105,
            "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
        )
        .await;
        seed_raindex_take_orders_for_orderbook(
            &db_path,
            tx_hash,
            105,
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .await;

        let raindex = load_test_raindex(db_path, "0xabcdef").await;

        let report = build_report(&pool, &raindex, 90, 110)
            .await
            .expect("build report");

        assert_eq!(report.audit.matched_executions, 1);
        assert_eq!(report.audit.unmatched_executions, 1);
        assert_eq!(report.matched_transactions.len(), 1);
        assert_eq!(report.matched_transactions[0].tx_hash, tx_hash);
        assert_eq!(
            report.matched_transactions[0].api_key_id,
            expected.api_key_id
        );
        assert_eq!(report.unmatched_transactions.len(), 1);
        assert_eq!(report.unmatched_transactions[0].tx_hash, tx_hash);
        assert_eq!(
            report.unmatched_transactions[0].to_address,
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(report.ambiguous_transactions.is_empty());
        assert!(report.rpc_lookup_failures.is_empty());
    }

    #[tokio::test]
    async fn test_build_report_records_ambiguous_transactions_without_attribution() {
        let pool = test_pool().await;
        let first = seed_match_context(&pool, 100).await;
        let second = seed_match_context(&pool, 101).await;
        let tx_hash = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders(&db_path, tx_hash, 105).await;

        let raindex = load_test_raindex(db_path, "0xabcdef").await;

        let report = build_report(&pool, &raindex, 90, 110)
            .await
            .expect("build report");

        assert_eq!(report.audit.matched_executions, 0);
        assert_eq!(report.audit.unmatched_executions, 0);
        assert_eq!(report.audit.ambiguous_matches, 1);
        assert!(report.customers.is_empty());
        assert!(report.matched_transactions.is_empty());
        assert!(report.unmatched_transactions.is_empty());
        assert_eq!(report.ambiguous_transactions.len(), 1);
        assert_eq!(report.ambiguous_transactions[0].tx_hash, tx_hash);
        assert_eq!(report.ambiguous_transactions[0].candidates.len(), 2);
        assert_eq!(
            report.ambiguous_transactions[0].candidates[0].api_key_id,
            second.api_key_id
        );
        assert_eq!(
            report.ambiguous_transactions[0].candidates[1].api_key_id,
            first.api_key_id
        );
        assert!(report.rpc_lookup_failures.is_empty());
    }

    #[tokio::test]
    async fn test_build_report_records_rpc_lookup_failures_without_aborting() {
        let pool = test_pool().await;
        let tx_hash = "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("raindex.db");
        seed_raindex_take_orders(&db_path, tx_hash, 105).await;

        let rpc_url = mock_rpc_url_for_missing_transaction().await;
        let raindex = load_test_raindex_with_rpc_url(db_path, &rpc_url).await;

        let report = build_report(&pool, &raindex, 90, 110)
            .await
            .expect("build report");

        assert_eq!(report.audit.matched_executions, 0);
        assert_eq!(report.audit.unmatched_executions, 0);
        assert_eq!(report.audit.ambiguous_matches, 0);
        assert_eq!(report.audit.rpc_lookup_failures, 1);
        assert!(report.customers.is_empty());
        assert!(report.matched_transactions.is_empty());
        assert!(report.unmatched_transactions.is_empty());
        assert!(report.ambiguous_transactions.is_empty());
        assert_eq!(report.rpc_lookup_failures.len(), 1);
        assert_eq!(report.rpc_lookup_failures[0].tx_hash, tx_hash);
        assert_eq!(
            report.rpc_lookup_failures[0].error,
            "transaction not found via RPC"
        );
    }

    #[tokio::test]
    async fn test_fetch_transaction_input_retries_same_rpc_url_after_invalid_json() {
        let rpc_url = mock_rpc_url_with_bodies(vec![
            "not-json".to_string(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "input": "0xabcdef",
                }
            })
            .to_string(),
        ])
        .await;
        let client = HttpClient::builder()
            .timeout(Duration::from_secs(RPC_LOOKUP_TIMEOUT_SECS))
            .build()
            .expect("http client");

        let input =
            fetch_transaction_input(&client, &[Url::parse(&rpc_url).expect("rpc url")], "0xtx")
                .await
                .expect("rpc retry should succeed");

        assert_eq!(input, "0xabcdef");
    }

    #[tokio::test]
    async fn test_fetch_transaction_input_falls_back_to_next_rpc_url() {
        let failing_rpc_url = mock_rpc_url_for_rpc_error("upstream unavailable").await;
        let succeeding_rpc_url = mock_rpc_url_for_input("0xabcdef").await;
        let client = HttpClient::builder()
            .timeout(Duration::from_secs(RPC_LOOKUP_TIMEOUT_SECS))
            .build()
            .expect("http client");

        let input = fetch_transaction_input(
            &client,
            &[
                Url::parse(&failing_rpc_url).expect("failing rpc url"),
                Url::parse(&succeeding_rpc_url).expect("succeeding rpc url"),
            ],
            "0xtx",
        )
        .await
        .expect("rpc fallback should succeed");

        assert_eq!(input, "0xabcdef");
    }
}
