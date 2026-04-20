use crate::auth::AuthenticatedKey;
use crate::cache::AppCache;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::{TokenRef, ValidatedAddress, ValidatedFixedBytes};
use crate::types::order::OrderTradeEntry;
use crate::types::trades::{
    TakerTradesResponse, TradeByAddress, TradeByTxEntry, TradeRequest, TradeResult,
    TradesBatchEntry, TradesBatchRequest, TradesBatchResponse, TradesByAddressResponse,
    TradesByTxResponse, TradesPagination, TradesPaginationParams, TradesTotals,
};
use alloy::primitives::{Address, FixedBytes, B256};
use async_trait::async_trait;
use futures::future::join_all;
use rain_math_float::Float;
use rain_orderbook_common::local_db::OrderbookIdentifier;
use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};
use rain_orderbook_common::raindex_client::trades::RaindexTrade;
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
use rocket::serde::json::Json;
use rocket::{Route, State};
use std::cmp::Reverse;
use std::ops::{Add, Div, Sub};
use std::str::FromStr;
use std::time::Duration;
use std::time::Instant;
use tracing::Instrument;

const ORDERS_SCAN_PAGE_SIZE: u16 = 50;
const FAST_INDEX_CHECK_ATTEMPTS: usize = 1;
const FAST_INDEX_CHECK_INTERVAL_MS: u64 = 0;
const TRADES_BY_ADDRESS_CACHE_TTL: Duration = Duration::from_secs(10);
const TRADES_BY_TX_CACHE_TTL: Duration = Duration::from_secs(300);
const TRADES_BY_ORDER_HASH_CACHE_TTL: Duration = Duration::from_secs(60);
const TRADES_CACHE_CAPACITY: u64 = 1_000;
const TRADES_BATCH_MAX_HASHES: usize = 50;

type TradesByAddressCache =
    AppCache<(Address, u32, u32, Option<u64>, Option<u64>), TradesByAddressResponse>;
type TradesByTxCache = AppCache<B256, TradesByTxResponse>;
type TradesByOrderHashCache = AppCache<B256, Vec<OrderTradeEntry>>;
type TakerTradesTxHashCache = AppCache<Address, Vec<(B256, u64)>>;

const TAKER_TX_HASH_CACHE_TTL: Duration = Duration::from_secs(15);

pub(crate) fn trades_by_address_cache() -> TradesByAddressCache {
    AppCache::new(TRADES_CACHE_CAPACITY, TRADES_BY_ADDRESS_CACHE_TTL)
}

pub(crate) fn trades_by_tx_cache() -> TradesByTxCache {
    AppCache::new(TRADES_CACHE_CAPACITY, TRADES_BY_TX_CACHE_TTL)
}

pub(crate) fn trades_by_order_hash_cache() -> TradesByOrderHashCache {
    AppCache::new(TRADES_CACHE_CAPACITY, TRADES_BY_ORDER_HASH_CACHE_TTL)
}

pub(crate) fn taker_trades_tx_hash_cache() -> TakerTradesTxHashCache {
    AppCache::new(TRADES_CACHE_CAPACITY, TAKER_TX_HASH_CACHE_TTL)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxIndexState {
    Indexed,
    NotYetIndexed,
}

struct TradeWithOwner {
    owner: Address,
    trade: RaindexTrade,
}

#[async_trait]
trait TradesDataSource: Send + Sync {
    async fn get_orders(
        &self,
        filters: GetOrdersFilters,
        page: Option<u16>,
        page_size: Option<u16>,
    ) -> Result<(Vec<RaindexOrder>, u32), ApiError>;

    async fn get_order_trades(
        &self,
        order: &RaindexOrder,
        start_time: Option<u64>,
        end_time: Option<u64>,
    ) -> Result<Vec<RaindexTrade>, ApiError>;

    async fn get_trades_by_tx(
        &self,
        tx_hash: B256,
        known_order_hashes: Option<Vec<B256>>,
    ) -> Result<Vec<TradeWithOwner>, ApiError> {
        let orders = match known_order_hashes {
            Some(hashes) if !hashes.is_empty() => {
                // Targeted lookup: find only the specific orders we know about
                let mut found = Vec::with_capacity(hashes.len());
                for hash in hashes {
                    if let Some(order) = self.find_order_by_hash(hash).await? {
                        found.push(order);
                    }
                }
                found
            }
            _ => {
                // Fallback: fetch all active orders (expensive)
                fetch_all_orders(
                    self,
                    GetOrdersFilters {
                        active: Some(true),
                        ..Default::default()
                    },
                )
                .await?
            }
        };
        let trades_with_owner = load_trades_with_owners(self, &orders, None, None).await?;

        Ok(trades_with_owner
            .into_iter()
            .filter(|trade_with_owner| trade_with_owner.trade.transaction().id() == tx_hash)
            .collect())
    }

    async fn find_order_by_hash(&self, hash: B256) -> Result<Option<RaindexOrder>, ApiError>;

    async fn check_tx_index_state(&self, tx_hash: B256) -> Result<TxIndexState, ApiError>;
}

struct RaindexTradesDataSource<'a> {
    client: &'a RaindexClient,
}

#[async_trait]
impl TradesDataSource for RaindexTradesDataSource<'_> {
    async fn get_orders(
        &self,
        filters: GetOrdersFilters,
        page: Option<u16>,
        page_size: Option<u16>,
    ) -> Result<(Vec<RaindexOrder>, u32), ApiError> {
        let result = self
            .client
            .get_orders(None, Some(filters), page, page_size)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query orders");
                ApiError::Internal("failed to query orders".into())
            })?;
        Ok((result.orders().to_vec(), result.total_count()))
    }

    async fn get_order_trades(
        &self,
        order: &RaindexOrder,
        start_time: Option<u64>,
        end_time: Option<u64>,
    ) -> Result<Vec<RaindexTrade>, ApiError> {
        order
            .get_trades_list(start_time, end_time, None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, order_hash = ?order.order_hash(), "failed to query order trades");
                ApiError::Internal("failed to query order trades".into())
            })
    }

    async fn find_order_by_hash(&self, hash: B256) -> Result<Option<RaindexOrder>, ApiError> {
        let orderbooks = self.client.get_all_orderbooks().map_err(|e| {
            tracing::error!(error = %e, "failed to get orderbooks");
            ApiError::Internal("failed to get orderbooks".into())
        })?;

        for orderbook in orderbooks.values() {
            let ob_id = OrderbookIdentifier::new(orderbook.network.chain_id, orderbook.address);
            match self.client.get_order_by_hash(&ob_id, hash).await {
                Ok(order) => return Ok(Some(order)),
                Err(RaindexError::OrderNotFound(..)) => continue,
                Err(e) => {
                    tracing::warn!(
                        order_hash = %hash,
                        error = %e,
                        "error looking up order by hash"
                    );
                    continue;
                }
            }
        }
        Ok(None)
    }

    async fn check_tx_index_state(&self, tx_hash: B256) -> Result<TxIndexState, ApiError> {
        let orderbooks = self.client.get_all_orderbooks().map_err(|e| {
            tracing::error!(error = %e, "failed to get orderbooks");
            ApiError::Internal("failed to get orderbooks".into())
        })?;

        let mut saw_timeout = false;

        for orderbook in orderbooks.values() {
            match self
                .client
                .get_transaction(
                    orderbook.network.chain_id,
                    orderbook.address,
                    tx_hash,
                    Some(FAST_INDEX_CHECK_ATTEMPTS),
                    Some(FAST_INDEX_CHECK_INTERVAL_MS),
                )
                .await
            {
                Ok(_) => return Ok(TxIndexState::Indexed),
                Err(RaindexError::TransactionIndexingTimeout { .. }) => {
                    saw_timeout = true;
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        tx_hash = %tx_hash,
                        chain_id = orderbook.network.chain_id,
                        orderbook = %orderbook.address,
                        "failed to query transaction status"
                    );
                    return Err(ApiError::Internal("failed to query transaction".into()));
                }
            }
        }

        if saw_timeout {
            return Ok(TxIndexState::NotYetIndexed);
        }
        Ok(TxIndexState::Indexed)
    }
}

fn to_u64(value: alloy::primitives::U256, field: &'static str) -> Result<u64, ApiError> {
    value.try_into().map_err(|_| {
        tracing::error!(field, "value does not fit into u64");
        ApiError::Internal(format!("{field} overflow"))
    })
}

fn parse_trade_order_hash(order_hash: alloy::primitives::Bytes) -> Result<B256, ApiError> {
    let hash = order_hash.to_string();
    B256::from_str(&hash).map_err(|e| {
        tracing::error!(error = %e, order_hash = %hash, "invalid trade order hash");
        ApiError::Internal("invalid trade order hash".into())
    })
}

fn maybe_parse_trade_order_hash(order_hash: alloy::primitives::Bytes) -> Option<FixedBytes<32>> {
    FixedBytes::<32>::from_str(&order_hash.to_string()).ok()
}

fn format_float(value: Float, context: &'static str) -> Result<String, ApiError> {
    value.format().map_err(|e| {
        tracing::error!(error = %e, context, "float formatting failed");
        ApiError::Internal(format!("{context} calculation failed"))
    })
}

fn positive_output(output_amount: Float) -> Result<Float, ApiError> {
    Float::zero()
        .map_err(|e| {
            tracing::error!(error = %e, "float zero construction failed");
            ApiError::Internal("io ratio calculation failed".into())
        })?
        .sub(output_amount)
        .map_err(|e| {
            tracing::error!(error = %e, "failed to negate output amount");
            ApiError::Internal("io ratio calculation failed".into())
        })
}

fn compute_io_ratio(input_amount: Float, output_amount: Float) -> Result<String, ApiError> {
    let positive_output = positive_output(output_amount)?;
    let zero = Float::zero().map_err(|e| {
        tracing::error!(error = %e, "float zero construction failed");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    if positive_output.eq(zero).unwrap_or(true) {
        return Ok("0".into());
    }
    let ratio = input_amount.div(positive_output).map_err(|e| {
        tracing::error!(error = %e, "failed to compute io ratio");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    format_float(ratio, "io ratio")
}

async fn fetch_all_orders<T: TradesDataSource + ?Sized>(
    ds: &T,
    filters: GetOrdersFilters,
) -> Result<Vec<RaindexOrder>, ApiError> {
    let mut all_orders = Vec::new();
    let mut page: u16 = 1;

    loop {
        let (orders, _total_count) = ds
            .get_orders(filters.clone(), Some(page), Some(ORDERS_SCAN_PAGE_SIZE))
            .await?;
        let batch_len = orders.len();
        all_orders.extend(orders);

        if batch_len < ORDERS_SCAN_PAGE_SIZE as usize {
            break;
        }
        page = page.saturating_add(1);
        if page == u16::MAX {
            break;
        }
    }

    Ok(all_orders)
}

async fn load_trades_with_owners<T: TradesDataSource + ?Sized>(
    ds: &T,
    orders: &[RaindexOrder],
    start_time: Option<u64>,
    end_time: Option<u64>,
) -> Result<Vec<TradeWithOwner>, ApiError> {
    let trade_results = join_all(
        orders
            .iter()
            .map(|order| ds.get_order_trades(order, start_time, end_time)),
    )
    .await;

    let mut all_trades = Vec::new();
    for (order, trades_result) in orders.iter().zip(trade_results) {
        let owner = order.owner();
        for trade in trades_result? {
            all_trades.push(TradeWithOwner { owner, trade });
        }
    }

    Ok(all_trades)
}

async fn process_get_trades_by_tx(
    ds: &dyn TradesDataSource,
    tx_hash: B256,
    known_order_hashes: Option<Vec<B256>>,
) -> Result<TradesByTxResponse, ApiError> {
    let lookup_started = Instant::now();
    let matching_trades = ds.get_trades_by_tx(tx_hash, known_order_hashes).await?;
    let lookup_duration_ms = lookup_started.elapsed().as_millis() as u64;

    if matching_trades.is_empty() {
        let index_check_started = Instant::now();
        match ds.check_tx_index_state(tx_hash).await? {
            TxIndexState::NotYetIndexed => {
                tracing::info!(
                    tx_hash = %tx_hash,
                    tx_lookup_duration_ms = lookup_duration_ms,
                    index_check_duration_ms = index_check_started.elapsed().as_millis() as u64,
                    "transaction trades lookup found no indexed results yet"
                );
                return Err(ApiError::NotYetIndexed(format!(
                    "transaction {tx_hash:#x} not yet indexed"
                )));
            }
            TxIndexState::Indexed => {
                tracing::info!(
                    tx_hash = %tx_hash,
                    tx_lookup_duration_ms = lookup_duration_ms,
                    index_check_duration_ms = index_check_started.elapsed().as_millis() as u64,
                    "transaction trades lookup found no matching trades"
                );
                return Err(ApiError::NotFound(
                    "transaction has no associated trades".into(),
                ));
            }
        }
    }

    let first_tx = matching_trades[0].trade.transaction();
    let mut total_input = Float::zero().map_err(|e| {
        tracing::error!(error = %e, "float zero construction failed");
        ApiError::Internal("trade totals calculation failed".into())
    })?;
    let mut total_output = Float::zero().map_err(|e| {
        tracing::error!(error = %e, "float zero construction failed");
        ApiError::Internal("trade totals calculation failed".into())
    })?;

    let mut entries = Vec::with_capacity(matching_trades.len());
    for trade_with_owner in matching_trades {
        let trade = trade_with_owner.trade;
        let input_change = trade.input_vault_balance_change();
        let output_change = trade.output_vault_balance_change();
        let io_ratio = compute_io_ratio(input_change.amount(), output_change.amount())?;
        let order_hash = parse_trade_order_hash(trade.order_hash())?;

        total_input = total_input.add(input_change.amount()).map_err(|e| {
            tracing::error!(error = %e, "failed to sum total input");
            ApiError::Internal("trade totals calculation failed".into())
        })?;
        total_output = total_output
            .add(positive_output(output_change.amount())?)
            .map_err(|e| {
                tracing::error!(error = %e, "failed to sum total output");
                ApiError::Internal("trade totals calculation failed".into())
            })?;

        entries.push(TradeByTxEntry {
            order_hash,
            order_owner: trade_with_owner.owner,
            request: TradeRequest {
                input_token: input_change.token().address(),
                output_token: output_change.token().address(),
                maximum_input: input_change.formatted_amount(),
                maximum_io_ratio: io_ratio.clone(),
            },
            result: TradeResult {
                input_amount: input_change.formatted_amount(),
                output_amount: output_change.formatted_amount(),
                actual_io_ratio: io_ratio,
            },
        });
    }

    let zero = Float::zero().map_err(|e| {
        tracing::error!(error = %e, "float zero construction failed");
        ApiError::Internal("trade totals calculation failed".into())
    })?;
    let average_io_ratio = if total_output.eq(zero).unwrap_or(true) {
        zero
    } else {
        total_input.div(total_output).map_err(|e| {
            tracing::error!(error = %e, "failed to compute average io ratio");
            ApiError::Internal("trade totals calculation failed".into())
        })?
    };

    tracing::info!(
        tx_hash = %tx_hash,
        trade_count = entries.len(),
        tx_lookup_duration_ms = lookup_duration_ms,
        "resolved trades by tx"
    );

    Ok(TradesByTxResponse {
        tx_hash,
        block_number: to_u64(first_tx.block_number(), "block number")?,
        timestamp: to_u64(first_tx.timestamp(), "timestamp")?,
        sender: first_tx.from(),
        trades: entries,
        totals: TradesTotals {
            total_input_amount: format_float(total_input, "trade totals")?,
            total_output_amount: format_float(total_output, "trade totals")?,
            average_io_ratio: format_float(average_io_ratio, "trade totals")?,
        },
    })
}

async fn process_get_trades_by_address(
    ds: &dyn TradesDataSource,
    direct_trades: Option<&crate::direct_trades::DirectTradesFetcher>,
    owner: Address,
    params: TradesPaginationParams,
) -> Result<TradesByAddressResponse, ApiError> {
    let start = Instant::now();

    let all_orders = fetch_all_orders(
        ds,
        GetOrdersFilters {
            owners: vec![owner],
            active: Some(true),
            ..Default::default()
        },
    )
    .await?;

    let orders_duration_ms = start.elapsed().as_millis() as u64;
    tracing::info!(
        owner = %owner,
        order_count = all_orders.len(),
        orders_duration_ms,
        "fetched orders for trades-by-address"
    );

    let trades = if let Some(fetcher) = direct_trades {
        // Fast path: batch SQLite query via DirectTradesFetcher
        let order_hashes: Vec<B256> = all_orders.iter().map(|o| o.order_hash()).collect();

        // Build order_hash → token info lookup
        let mut token_map: std::collections::HashMap<B256, (TokenRef, TokenRef)> =
            std::collections::HashMap::new();
        for order in &all_orders {
            if let Ok((input_vault, output_vault)) = super::resolve_io_vaults(order) {
                let input_token_info = input_vault.token();
                let output_token_info = output_vault.token();
                token_map.insert(
                    order.order_hash(),
                    (
                        TokenRef {
                            address: input_token_info.address(),
                            symbol: input_token_info.symbol().unwrap_or_default(),
                            decimals: input_token_info.decimals(),
                        },
                        TokenRef {
                            address: output_token_info.address(),
                            symbol: output_token_info.symbol().unwrap_or_default(),
                            decimals: output_token_info.decimals(),
                        },
                    ),
                );
            }
        }

        let batch_start = Instant::now();
        match fetcher.batch_fetch(&order_hashes).await {
            Ok(batch_result) => {
                let batch_duration_ms = batch_start.elapsed().as_millis() as u64;
                tracing::info!(
                    owner = %owner,
                    order_count = order_hashes.len(),
                    batch_duration_ms,
                    "direct batch trades completed for trades-by-address"
                );

                let mut trades = Vec::new();
                for (order_hash, entries) in &batch_result {
                    let (input_token, output_token) =
                        token_map.get(order_hash).cloned().unwrap_or_else(|| {
                            (
                                TokenRef {
                                    address: Address::ZERO,
                                    symbol: String::new(),
                                    decimals: 0,
                                },
                                TokenRef {
                                    address: Address::ZERO,
                                    symbol: String::new(),
                                    decimals: 0,
                                },
                            )
                        });

                    for entry in entries {
                        // Apply time filters if specified
                        if let Some(start_time) = params.start_time {
                            if entry.timestamp < start_time {
                                continue;
                            }
                        }
                        if let Some(end_time) = params.end_time {
                            if entry.timestamp > end_time {
                                continue;
                            }
                        }

                        trades.push(TradeByAddress {
                            tx_hash: entry.tx_hash,
                            input_amount: entry.input_amount.clone(),
                            output_amount: entry.output_amount.clone(),
                            input_token: input_token.clone(),
                            output_token: output_token.clone(),
                            order_hash: Some(*order_hash),
                            timestamp: entry.timestamp,
                            block_number: 0, // not available from DirectTradesFetcher
                        });
                    }
                }
                trades
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    owner = %owner,
                    "direct batch trades failed for trades-by-address; falling back to library"
                );
                // Fallback to slow path
                build_trades_from_library(ds, &all_orders, &params).await?
            }
        }
    } else {
        // Slow path: N individual queries via library
        build_trades_from_library(ds, &all_orders, &params).await?
    };

    let mut trades = trades;
    trades.sort_by_key(|t| (Reverse(t.timestamp), Reverse(t.block_number)));

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20);
    let total_trades = trades.len() as u64;
    let total_pages = if page_size == 0 {
        0
    } else {
        total_trades.div_ceil(u64::from(page_size))
    };

    let offset = (u64::from(page.saturating_sub(1)) * u64::from(page_size)) as usize;
    let paginated = if offset >= trades.len() {
        Vec::new()
    } else {
        let end = std::cmp::min(offset + page_size as usize, trades.len());
        trades[offset..end].to_vec()
    };

    tracing::info!(
        owner = %owner,
        page,
        page_size,
        total_trades,
        returned_trades = paginated.len(),
        total_duration_ms = start.elapsed().as_millis() as u64,
        "resolved trades by address"
    );

    Ok(TradesByAddressResponse {
        trades: paginated,
        pagination: TradesPagination {
            page,
            page_size,
            total_trades,
            total_pages,
            has_more: u64::from(page) < total_pages,
        },
    })
}

async fn build_trades_from_library(
    ds: &dyn TradesDataSource,
    orders: &[RaindexOrder],
    params: &TradesPaginationParams,
) -> Result<Vec<TradeByAddress>, ApiError> {
    let trades_with_owner =
        load_trades_with_owners(ds, orders, params.start_time, params.end_time).await?;

    let mut trades = Vec::with_capacity(trades_with_owner.len());
    for trade_with_owner in trades_with_owner {
        let trade = trade_with_owner.trade;
        let input_change = trade.input_vault_balance_change();
        let output_change = trade.output_vault_balance_change();
        let input_token = input_change.token();
        let output_token = output_change.token();
        trades.push(TradeByAddress {
            tx_hash: trade.transaction().id(),
            input_amount: input_change.formatted_amount(),
            output_amount: output_change.formatted_amount(),
            input_token: TokenRef {
                address: input_token.address(),
                symbol: input_token.symbol().unwrap_or_default(),
                decimals: input_token.decimals(),
            },
            output_token: TokenRef {
                address: output_token.address(),
                symbol: output_token.symbol().unwrap_or_default(),
                decimals: output_token.decimals(),
            },
            order_hash: maybe_parse_trade_order_hash(trade.order_hash()),
            timestamp: to_u64(trade.timestamp(), "timestamp")?,
            block_number: to_u64(trade.transaction().block_number(), "block number")?,
        });
    }
    Ok(trades)
}

async fn get_cached_trades_by_tx(
    cache: &TradesByTxCache,
    ds: &dyn TradesDataSource,
    tx_hash: B256,
    known_order_hashes: Option<Vec<B256>>,
) -> Result<TradesByTxResponse, ApiError> {
    cache
        .get_or_try_insert(tx_hash, || async move {
            process_get_trades_by_tx(ds, tx_hash, known_order_hashes).await
        })
        .await
        .map_err(ApiError::from)
}

async fn get_cached_trades_by_address(
    cache: &TradesByAddressCache,
    ds: &dyn TradesDataSource,
    direct_trades: Option<&crate::direct_trades::DirectTradesFetcher>,
    owner: Address,
    params: TradesPaginationParams,
) -> Result<TradesByAddressResponse, ApiError> {
    let cache_key = (
        owner,
        params.page.unwrap_or(1),
        params.page_size.unwrap_or(20),
        params.start_time,
        params.end_time,
    );
    cache
        .get_or_try_insert(cache_key, || async move {
            process_get_trades_by_address(ds, direct_trades, owner, params).await
        })
        .await
        .map_err(ApiError::from)
}

async fn process_get_taker_trades(
    ds: &dyn TradesDataSource,
    direct_trades: Option<&crate::direct_trades::DirectTradesFetcher>,
    trades_by_tx_cache: &TradesByTxCache,
    taker_tx_cache: &TakerTradesTxHashCache,
    sender: Address,
    params: TradesPaginationParams,
) -> Result<TakerTradesResponse, ApiError> {
    // Step 1: Get tx hashes (cached)
    let tx_hashes = match direct_trades {
        Some(fetcher) => taker_tx_cache
            .get_or_try_insert(sender, || async {
                fetcher.fetch_taker_tx_hashes(&sender).await
            })
            .await
            .map_err(ApiError::from)?,
        None => {
            tracing::warn!("direct trades fetcher unavailable; returning empty taker trades");
            return Ok(TakerTradesResponse {
                market_orders: vec![],
                pagination: TradesPagination {
                    page: 1,
                    page_size: params.page_size.unwrap_or(20),
                    total_trades: 0,
                    total_pages: 0,
                    has_more: false,
                },
            });
        }
    };

    // Step 2: Paginate
    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20);
    let total = tx_hashes.len() as u64;
    let total_pages = if page_size == 0 {
        0
    } else {
        total.div_ceil(u64::from(page_size))
    };
    let offset = (u64::from(page.saturating_sub(1)) * u64::from(page_size)) as usize;
    let page_hashes: Vec<B256> = if offset >= tx_hashes.len() {
        vec![]
    } else {
        let end = std::cmp::min(offset + page_size as usize, tx_hashes.len());
        tx_hashes[offset..end].iter().map(|(h, _)| *h).collect()
    };

    // Step 3: Resolve each tx via existing cached trade-by-tx lookup
    let mut market_orders = Vec::with_capacity(page_hashes.len());
    for tx_hash in page_hashes {
        match get_cached_trades_by_tx(trades_by_tx_cache, ds, tx_hash, None).await {
            Ok(tx_trades) => market_orders.push(tx_trades),
            Err(e) => {
                tracing::warn!(tx_hash = %tx_hash, error = %e, "failed to resolve taker tx; skipping");
            }
        }
    }

    Ok(TakerTradesResponse {
        market_orders,
        pagination: TradesPagination {
            page,
            page_size,
            total_trades: total,
            total_pages,
            has_more: u64::from(page) < total_pages,
        },
    })
}

#[utoipa::path(
    get,
    path = "/v1/trades/tx/{tx_hash}",
    tag = "Trades",
    security(("basicAuth" = [])),
    params(
        ("tx_hash" = String, Path, description = "Transaction hash"),
    ),
    responses(
        (status = 200, description = "Trades from transaction", body = TradesByTxResponse),
        (status = 202, description = "Transaction not yet indexed", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 404, description = "Transaction not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/tx/<tx_hash>")]
pub async fn get_trades_by_tx(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    trades_by_tx_cache: &State<TradesByTxCache>,
    direct_trades: &State<Option<crate::direct_trades::DirectTradesFetcher>>,
    span: TracingSpan,
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<TradesByTxResponse>, ApiError> {
    async move {
        tracing::info!(tx_hash = ?tx_hash, "request received");

        // Use DirectTradesFetcher to find which order hashes have trades in this tx,
        // so we don't need to fetch ALL orders from the subgraph.
        let raindex = shared_raindex.read().await;
        let ds = RaindexTradesDataSource {
            client: raindex.client(),
        };

        let known_order_hashes = if let Some(fetcher) = direct_trades.inner().as_ref() {
            match fetcher.fetch_by_tx_hash(&tx_hash.0).await {
                Ok(trades_map) if !trades_map.is_empty() => {
                    let hashes: Vec<B256> = trades_map.keys().copied().collect();
                    tracing::info!(
                        tx_hash = ?tx_hash,
                        order_count = hashes.len(),
                        "direct trades fetcher found order hashes for tx"
                    );
                    Some(hashes)
                }
                Ok(_) => {
                    // DirectTradesFetcher found no trades — skip expensive full scan
                    // and go directly to the index state check
                    tracing::info!(tx_hash = ?tx_hash, "direct trades fetcher found no trades for tx");
                    match ds.check_tx_index_state(tx_hash.0).await? {
                        TxIndexState::NotYetIndexed => {
                            return Err(ApiError::NotYetIndexed(format!(
                                "transaction {:#x} not yet indexed",
                                tx_hash.0
                            )));
                        }
                        TxIndexState::Indexed => {
                            return Err(ApiError::NotFound(
                                "transaction has no associated trades".into(),
                            ));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        tx_hash = ?tx_hash,
                        error = %e,
                        "direct trades fetcher failed, falling back to full scan"
                    );
                    None
                }
            }
        } else {
            None
        };

        let response =
            get_cached_trades_by_tx(trades_by_tx_cache, &ds, tx_hash.0, known_order_hashes)
                .await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/trades/{address}",
    tag = "Trades",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Owner address"),
        TradesPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of trades", body = TradesByAddressResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<address>?<params..>", rank = 2)]
pub async fn get_trades_by_address(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    trades_by_address_cache: &State<TradesByAddressCache>,
    direct_trades: &State<Option<crate::direct_trades::DirectTradesFetcher>>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        let raindex = shared_raindex.read().await;
        let ds = RaindexTradesDataSource {
            client: raindex.client(),
        };
        let response = get_cached_trades_by_address(
            trades_by_address_cache,
            &ds,
            direct_trades.inner().as_ref(),
            address.0,
            params,
        )
        .await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/trades/taker/{address}",
    tag = "Trades",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Taker address"),
        TradesPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of market orders (taker transactions)", body = TakerTradesResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/taker/<address>?<params..>")]
pub async fn get_taker_trades(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    trades_by_tx_cache: &State<TradesByTxCache>,
    taker_tx_cache: &State<TakerTradesTxHashCache>,
    direct_trades: &State<Option<crate::direct_trades::DirectTradesFetcher>>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: TradesPaginationParams,
) -> Result<Json<TakerTradesResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "taker trades request received");
        let raindex = shared_raindex.read().await;
        let ds = RaindexTradesDataSource {
            client: raindex.client(),
        };
        let response = process_get_taker_trades(
            &ds,
            direct_trades.inner().as_ref(),
            trades_by_tx_cache,
            taker_tx_cache,
            address.0,
            params,
        )
        .await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn fetch_trades_for_hash(
    ds: &dyn TradesDataSource,
    hash: B256,
) -> Result<Vec<OrderTradeEntry>, ApiError> {
    let order = match ds.find_order_by_hash(hash).await? {
        Some(o) => o,
        None => return Ok(vec![]),
    };
    let trades = ds.get_order_trades(&order, None, None).await?;
    Ok(trades.iter().map(super::order::map_trade).collect())
}

async fn process_trades_batch(
    ds: &dyn TradesDataSource,
    cache: &TradesByOrderHashCache,
    direct_trades: Option<&crate::direct_trades::DirectTradesFetcher>,
    hashes: Vec<B256>,
) -> Result<TradesBatchResponse, ApiError> {
    let total_start = Instant::now();

    let mut cached_map: std::collections::HashMap<B256, Vec<OrderTradeEntry>> =
        std::collections::HashMap::new();
    let mut uncached: Vec<B256> = Vec::new();

    for &hash in &hashes {
        if let Some(trades) = cache.get(&hash).await {
            cached_map.insert(hash, trades);
        } else {
            uncached.push(hash);
        }
    }

    tracing::info!(
        total_hashes = hashes.len(),
        cached = cached_map.len(),
        uncached = uncached.len(),
        "batch trades cache check"
    );

    if !uncached.is_empty() {
        if let Some(fetcher) = direct_trades {
            // Fast path: single batch query via direct SQLite connection
            match fetcher.batch_fetch(&uncached).await {
                Ok(batch_result) => {
                    for &hash in &uncached {
                        let trades = batch_result.get(&hash).cloned().unwrap_or_default();
                        cache.insert(hash, trades.clone()).await;
                        cached_map.insert(hash, trades);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "direct batch trades failed; falling back to library");
                    let results =
                        join_all(uncached.iter().map(|&hash| fetch_trades_for_hash(ds, hash)))
                            .await;
                    for (&hash, result) in uncached.iter().zip(results) {
                        match result {
                            Ok(trades) => {
                                cache.insert(hash, trades.clone()).await;
                                cached_map.insert(hash, trades);
                            }
                            Err(e) => {
                                tracing::warn!(order_hash = %hash, error = %e, "failed to fetch trades for order in batch");
                                cached_map.insert(hash, vec![]);
                            }
                        }
                    }
                }
            }
        } else {
            // Fallback: N parallel queries via library
            let results =
                join_all(uncached.iter().map(|&hash| fetch_trades_for_hash(ds, hash))).await;
            for (&hash, result) in uncached.iter().zip(results) {
                match result {
                    Ok(trades) => {
                        cache.insert(hash, trades.clone()).await;
                        cached_map.insert(hash, trades);
                    }
                    Err(e) => {
                        tracing::warn!(order_hash = %hash, error = %e, "failed to fetch trades for order in batch");
                        cached_map.insert(hash, vec![]);
                    }
                }
            }
        }
    }

    let entries = hashes
        .iter()
        .map(|hash| TradesBatchEntry {
            order_hash: *hash,
            trades: cached_map.remove(hash).unwrap_or_default(),
        })
        .collect();

    tracing::info!(
        total_duration_ms = total_start.elapsed().as_millis(),
        total_hashes = hashes.len(),
        "batch trades request processed"
    );

    Ok(TradesBatchResponse { orders: entries })
}

#[utoipa::path(
    post,
    path = "/v1/trades/batch",
    tag = "Trades",
    security(("basicAuth" = [])),
    request_body = TradesBatchRequest,
    responses(
        (status = 200, description = "Trades grouped by order hash", body = TradesBatchResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/batch", data = "<body>")]
pub async fn post_trades_batch(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    trades_by_order_hash_cache: &State<TradesByOrderHashCache>,
    direct_trades: &State<Option<crate::direct_trades::DirectTradesFetcher>>,
    span: TracingSpan,
    body: Json<TradesBatchRequest>,
) -> Result<Json<TradesBatchResponse>, ApiError> {
    async move {
        tracing::info!(
            hash_count = body.order_hashes.len(),
            "batch trades request received"
        );

        if body.order_hashes.is_empty() {
            return Ok(Json(TradesBatchResponse { orders: vec![] }));
        }

        if body.order_hashes.len() > TRADES_BATCH_MAX_HASHES {
            return Err(ApiError::BadRequest(format!(
                "maximum {} order hashes per batch request",
                TRADES_BATCH_MAX_HASHES
            )));
        }

        let raindex = shared_raindex.read().await;
        let ds = RaindexTradesDataSource {
            client: raindex.client(),
        };
        let response = process_trades_batch(
            &ds,
            trades_by_order_hash_cache,
            direct_trades.inner().as_ref(),
            body.into_inner().order_hashes,
        )
        .await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_trades_by_tx,
        get_taker_trades,
        get_trades_by_address,
        post_trades_batch
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::{mock_order, mock_trade};
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use rocket::http::{Header, Status};

    struct MockTradesDataSource {
        orders_result: Result<(Vec<RaindexOrder>, u32), ApiError>,
        trades_result: Result<Vec<RaindexTrade>, ApiError>,
        tx_index_state: Result<TxIndexState, ApiError>,
    }

    #[async_trait]
    impl TradesDataSource for MockTradesDataSource {
        async fn get_orders(
            &self,
            _filters: GetOrdersFilters,
            _page: Option<u16>,
            _page_size: Option<u16>,
        ) -> Result<(Vec<RaindexOrder>, u32), ApiError> {
            self.orders_result.clone()
        }

        async fn get_order_trades(
            &self,
            _order: &RaindexOrder,
            _start_time: Option<u64>,
            _end_time: Option<u64>,
        ) -> Result<Vec<RaindexTrade>, ApiError> {
            self.trades_result.clone()
        }

        async fn find_order_by_hash(&self, _hash: B256) -> Result<Option<RaindexOrder>, ApiError> {
            match &self.orders_result {
                Ok((orders, _)) => Ok(orders.first().cloned()),
                Err(_) => Err(ApiError::Internal("failed to find order".into())),
            }
        }

        async fn check_tx_index_state(&self, _tx_hash: B256) -> Result<TxIndexState, ApiError> {
            self.tx_index_state.clone()
        }
    }

    fn tx_hash() -> B256 {
        "0x0000000000000000000000000000000000000000000000000000000000000088"
            .parse()
            .unwrap()
    }

    #[rocket::async_test]
    async fn test_process_get_trades_by_tx_success() {
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![mock_order()], 1)),
            trades_result: Ok(vec![mock_trade()]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };

        let response = process_get_trades_by_tx(&ds, tx_hash(), None)
            .await
            .unwrap();

        assert_eq!(response.trades.len(), 1);
        assert_eq!(response.block_number, 100);
        assert_eq!(response.timestamp, 1700001000);
        assert_eq!(
            response.sender.to_string(),
            "0x0000000000000000000000000000000000000002"
        );
    }

    #[rocket::async_test]
    async fn test_process_get_trades_by_tx_not_yet_indexed() {
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![mock_order()], 1)),
            trades_result: Ok(vec![]),
            tx_index_state: Ok(TxIndexState::NotYetIndexed),
        };

        let result = process_get_trades_by_tx(&ds, tx_hash(), None).await;
        assert!(matches!(result, Err(ApiError::NotYetIndexed(_))));
    }

    #[rocket::async_test]
    async fn test_process_get_trades_by_address_success() {
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![mock_order()], 1)),
            trades_result: Ok(vec![mock_trade()]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };

        let response = process_get_trades_by_address(
            &ds,
            None,
            "0x0000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
            TradesPaginationParams {
                page: Some(1),
                page_size: Some(20),
                start_time: None,
                end_time: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.trades.len(), 1);
        assert_eq!(response.pagination.total_trades, 1);
        assert_eq!(response.pagination.total_pages, 1);
        assert!(!response.pagination.has_more);
    }

    #[rocket::async_test]
    async fn test_get_cached_trades_by_tx_reuses_cached_response() {
        let cache = trades_by_tx_cache();
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![mock_order()], 1)),
            trades_result: Ok(vec![mock_trade()]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };

        let first = get_cached_trades_by_tx(&cache, &ds, tx_hash(), None)
            .await
            .unwrap();
        let second = get_cached_trades_by_tx(&cache, &ds, tx_hash(), None)
            .await
            .unwrap();

        assert_eq!(first.trades.len(), 1);
        assert_eq!(second.trades.len(), 1);
        assert_eq!(cache.get(&tx_hash()).await.unwrap().trades.len(), 1);
    }

    #[rocket::async_test]
    async fn test_get_cached_trades_by_address_reuses_cached_response() {
        let cache = trades_by_address_cache();
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![mock_order()], 1)),
            trades_result: Ok(vec![mock_trade()]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };
        let owner: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
        };

        let first = get_cached_trades_by_address(&cache, &ds, None, owner, params.clone())
            .await
            .unwrap();
        let second = get_cached_trades_by_address(&cache, &ds, None, owner, params.clone())
            .await
            .unwrap();

        assert_eq!(first.trades.len(), 1);
        assert_eq!(second.trades.len(), 1);
        assert_eq!(
            cache
                .get(&(owner, 1, 20, None, None))
                .await
                .unwrap()
                .trades
                .len(),
            1
        );
    }

    #[rocket::async_test]
    async fn test_get_trades_by_tx_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/trades/tx/0x0000000000000000000000000000000000000000000000000000000000000088")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_trades_by_address_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/trades/0x0000000000000000000000000000000000000001")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_process_trades_batch_success() {
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![mock_order()], 1)),
            trades_result: Ok(vec![mock_trade()]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };
        let cache = trades_by_order_hash_cache();
        let hash: B256 = "0x000000000000000000000000000000000000000000000000000000000000abcd"
            .parse()
            .unwrap();

        let response = process_trades_batch(&ds, &cache, None, vec![hash])
            .await
            .unwrap();

        assert_eq!(response.orders.len(), 1);
        assert_eq!(response.orders[0].order_hash, hash);
        assert_eq!(response.orders[0].trades.len(), 1);
    }

    #[rocket::async_test]
    async fn test_process_trades_batch_caches_results() {
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![mock_order()], 1)),
            trades_result: Ok(vec![mock_trade()]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };
        let cache = trades_by_order_hash_cache();
        let hash: B256 = "0x000000000000000000000000000000000000000000000000000000000000abcd"
            .parse()
            .unwrap();

        let _ = process_trades_batch(&ds, &cache, None, vec![hash])
            .await
            .unwrap();
        let cached = cache.get(&hash).await;
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().len(), 1);
    }

    #[rocket::async_test]
    async fn test_process_trades_batch_empty_hashes() {
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![], 0)),
            trades_result: Ok(vec![]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };
        let cache = trades_by_order_hash_cache();

        let response = process_trades_batch(&ds, &cache, None, vec![])
            .await
            .unwrap();
        assert!(response.orders.is_empty());
    }

    #[rocket::async_test]
    async fn test_process_trades_batch_order_not_found_returns_empty_trades() {
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![], 0)),
            trades_result: Ok(vec![]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };
        let cache = trades_by_order_hash_cache();
        let hash: B256 = "0x0000000000000000000000000000000000000000000000000000000000001234"
            .parse()
            .unwrap();

        let response = process_trades_batch(&ds, &cache, None, vec![hash])
            .await
            .unwrap();

        assert_eq!(response.orders.len(), 1);
        assert!(response.orders[0].trades.is_empty());
    }

    #[rocket::async_test]
    async fn test_post_trades_batch_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/trades/batch")
            .body(r#"{"orderHashes":[]}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_trades_by_address_invalid_address_returns_422() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/trades/not-an-address")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }

    #[rocket::async_test]
    async fn test_process_get_taker_trades_without_direct_fetcher_returns_empty() {
        let ds = MockTradesDataSource {
            orders_result: Ok((vec![], 0)),
            trades_result: Ok(vec![]),
            tx_index_state: Ok(TxIndexState::Indexed),
        };
        let tx_cache = trades_by_tx_cache();
        let taker_cache = taker_trades_tx_hash_cache();
        let sender: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();

        let result = process_get_taker_trades(
            &ds,
            None, // no direct fetcher
            &tx_cache,
            &taker_cache,
            sender,
            TradesPaginationParams {
                page: Some(1),
                page_size: Some(20),
                start_time: None,
                end_time: None,
            },
        )
        .await
        .unwrap();

        assert!(result.market_orders.is_empty());
        assert_eq!(result.pagination.total_trades, 0);
        assert_eq!(result.pagination.page, 1);
        assert!(!result.pagination.has_more);
    }

    #[rocket::async_test]
    async fn test_get_taker_trades_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/trades/taker/0x0000000000000000000000000000000000000001")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }
}
