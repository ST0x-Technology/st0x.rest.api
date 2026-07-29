use super::{
    current_wrap_ratios_for_trades, map_trade_for_list, RaindexTradesDataSource, TradesDataSource,
};
use crate::app_state::ApplicationState;
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::routes::batch_query::{
    parse_canonical_addresses, require_single_subgraph_for_pagination, validate_configured_chain,
};
use crate::types::common::Denomination;
use crate::types::trades::{
    TradesByOrderHashEntry, TradesByOrderHashesResponse, TradesQueryRequest, TradesQueryResponse,
};
use alloy::primitives::{Address, B256};
use rain_orderbook_common::raindex_client::trades::{
    GetTradesByOrderHashesFilters, GetTradesFilters, GetTradesTokenFilter, RaindexTrade,
};
use rain_orderbook_common::raindex_client::types::TimeFilter;
use rocket::serde::json::Json;
use rocket::State;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use tracing::Instrument;

const MAX_TOKEN_ADDRESSES: usize = 64;
const MAX_ORDER_HASHES: usize = 64;
const MAX_PAGE_SIZE: u16 = 500;
const MAX_PAGE: u16 = 1_000;
const MAX_TOKEN_TIME_RANGE_SECONDS: u64 = 90 * 24 * 60 * 60;
const MAX_CACHED_GROUPED_TRADES: u64 = 5_000;

#[derive(Debug, Clone)]
struct ValidatedOrderHashTradesQuery {
    chain_id: Option<u32>,
    token_addresses: Vec<Address>,
    canonical_order_hashes: Vec<B256>,
    response_order_hashes: Vec<B256>,
    start_time: Option<u64>,
    end_time: Option<u64>,
    denomination: Denomination,
}

#[derive(Debug, Clone)]
struct ValidatedTokenTradesQuery {
    chain_id: u32,
    token_addresses: Vec<Address>,
    start_time: u64,
    end_time: u64,
    page: u16,
    page_size: u16,
    denomination: Denomination,
}

#[derive(Debug, Clone)]
enum ValidatedTradesQuery {
    OrderHashes(ValidatedOrderHashTradesQuery),
    Tokens(ValidatedTokenTradesQuery),
}

impl ValidatedTradesQuery {
    fn mode(&self) -> &'static str {
        match self {
            Self::OrderHashes(_) => "order-hashes",
            Self::Tokens(_) => "tokens",
        }
    }

    fn chain_id(&self) -> Option<u32> {
        match self {
            Self::OrderHashes(query) => query.chain_id,
            Self::Tokens(query) => Some(query.chain_id),
        }
    }

    fn batch_size(&self) -> usize {
        match self {
            Self::OrderHashes(query) => query.canonical_order_hashes.len(),
            Self::Tokens(query) => query.token_addresses.len(),
        }
    }

    fn response_order_hashes(&self) -> &[B256] {
        match self {
            Self::OrderHashes(query) => &query.response_order_hashes,
            Self::Tokens(_) => &[],
        }
    }
}

#[utoipa::path(
    post,
    path = "/v1/trades/query",
    tag = "Trades",
    security(("basicAuth" = [])),
    request_body = TradesQueryRequest,
    responses(
        (status = 200, description = "Legacy grouped order-hash response or paginated token-set response, selected by request mode", body = TradesQueryResponse),
        (status = 400, description = "Invalid batch filters or bounds", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/query", data = "<request>")]
pub async fn post_trades_query(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    pool: &State<DbPool>,
    app_state: &State<ApplicationState>,
    span: TracingSpan,
    request: Json<TradesQueryRequest>,
) -> Result<Json<TradesQueryResponse>, ApiError> {
    async move {
        let request = request.into_inner();
        tracing::info!(
            chain_id = request.chain_id,
            token_addresses_count = request.token_addresses.len(),
            order_hashes_count = request.order_hashes.as_ref().map_or(0, Vec::len),
            has_order_hashes_field = request.order_hashes.is_some(),
            start_time = request.start_time,
            end_time = request.end_time,
            "batch trades query request received"
        );

        let client = {
            let raindex = shared_raindex.read().await;
            raindex.client().clone()
        };
        if let Some(chain_id) = request.chain_id {
            validate_configured_chain(&client, chain_id)?;
        }
        let ds = RaindexTradesDataSource {
            client: &client,
            pool: pool.inner(),
        };
        process_trades_query(&ds, &app_state.response_caches, request)
            .await
            .map(Json)
    }
    .instrument(span.0)
    .await
}

pub(crate) async fn process_trades_query(
    ds: &dyn TradesDataSource,
    caches: &crate::cache::RouteResponseCaches,
    request: TradesQueryRequest,
) -> Result<TradesQueryResponse, ApiError> {
    let query = validate_trades_query(request)?;
    let cache_key = trades_query_cache_key(&query);

    let subgraph_count = ds.query_subgraph_count(query.chain_id())?;
    let cache_safe = match &query {
        ValidatedTradesQuery::Tokens(query) => {
            require_single_subgraph_for_pagination("trades", query.chain_id, subgraph_count)?;
            true
        }
        ValidatedTradesQuery::OrderHashes(_) => subgraph_count <= 1,
    };
    let response = if !caches.is_enabled() || !cache_safe {
        tracing::info!(
            mode = query.mode(),
            batch_size = query.batch_size(),
            cache_enabled = caches.is_enabled(),
            cache_safe,
            subgraph_count,
            "batch trades response cache bypassed"
        );
        compute_trades_query(ds, &query).await?
    } else if let Some(response) = caches.trades_query.get(&cache_key).await {
        tracing::info!(
            mode = query.mode(),
            batch_size = query.batch_size(),
            cache_hit = true,
            "batch trades response cache hit"
        );
        response
    } else {
        tracing::info!(
            mode = query.mode(),
            batch_size = query.batch_size(),
            cache_hit = false,
            "batch trades response cache miss"
        );
        caches
            .trades_query
            .get_or_try_insert(cache_key.clone(), || async {
                compute_trades_query(ds, &query).await
            })
            .await
            .map_err(|error| (*error).clone())?
    };

    if !trades_query_cache_admissible(&response) {
        caches.trades_query.invalidate(&cache_key).await;
        tracing::info!(
            mode = query.mode(),
            batch_size = query.batch_size(),
            "batch trades response exceeded cache admission bound"
        );
    }

    Ok(restore_legacy_hash_order(
        response,
        query.response_order_hashes(),
    ))
}

fn validate_trades_query(request: TradesQueryRequest) -> Result<ValidatedTradesQuery, ApiError> {
    if request
        .order_hashes
        .as_ref()
        .is_some_and(|hashes| hashes.len() > MAX_ORDER_HASHES)
    {
        return validation_error(format!(
            "orderHashes must contain at most {MAX_ORDER_HASHES} entries"
        ));
    }
    if request.chain_id == Some(0) {
        return validation_error("chainId must be greater than zero");
    }
    if request
        .start_time
        .zip(request.end_time)
        .is_some_and(|(start, end)| start > end)
    {
        return validation_error("startTime must be less than or equal to endTime");
    }

    let token_addresses = parse_canonical_addresses(
        "tokenAddresses",
        request.token_addresses,
        MAX_TOKEN_ADDRESSES,
    )
    .map_err(log_trades_validation_error)?;
    let denomination = request.denomination.unwrap_or_default();

    match request.order_hashes {
        Some(order_hashes) => {
            if request.page.is_some() || request.page_size.is_some() {
                return validation_error(
                    "page and pageSize are only supported in token-only query mode",
                );
            }
            let response_order_hashes = parse_order_hashes_preserving_order(order_hashes)?;
            let mut canonical_order_hashes = response_order_hashes.clone();
            canonical_order_hashes.sort_unstable();
            Ok(ValidatedTradesQuery::OrderHashes(
                ValidatedOrderHashTradesQuery {
                    chain_id: request.chain_id,
                    token_addresses,
                    canonical_order_hashes,
                    response_order_hashes,
                    start_time: request.start_time,
                    end_time: request.end_time,
                    denomination,
                },
            ))
        }
        None => {
            if token_addresses.is_empty() {
                return validation_error("tokenAddresses or orderHashes is required");
            }
            let chain_id = request.chain_id.ok_or_else(|| {
                log_trades_validation_error(ApiError::BadRequest(
                    "chainId is required in token-only query mode".into(),
                ))
            })?;
            let (Some(start), Some(end)) = (request.start_time, request.end_time) else {
                return validation_error(
                    "startTime and endTime are required in token-only query mode",
                );
            };
            if end - start > MAX_TOKEN_TIME_RANGE_SECONDS {
                return validation_error("time window must not exceed 90 days");
            }
            let page = request.page.unwrap_or(1);
            let page_size = request.page_size.unwrap_or(20);
            if page == 0 || page > MAX_PAGE {
                return validation_error("page must be between 1 and 1000");
            }
            if page_size == 0 || page_size > MAX_PAGE_SIZE {
                return validation_error("pageSize must be between 1 and 500");
            }
            Ok(ValidatedTradesQuery::Tokens(ValidatedTokenTradesQuery {
                chain_id,
                token_addresses,
                start_time: start,
                end_time: end,
                page,
                page_size,
                denomination,
            }))
        }
    }
}

fn parse_order_hashes_preserving_order(values: Vec<String>) -> Result<Vec<B256>, ApiError> {
    let original_len = values.len();
    let mut seen = HashSet::new();
    let mut hashes = Vec::with_capacity(values.len());
    for value in values {
        let hash = B256::from_str(&value).map_err(|error| {
            tracing::warn!(input = %value, %error, "invalid order hash");
            ApiError::BadRequest("invalid order hash".into())
        })?;
        if seen.insert(hash) {
            hashes.push(hash);
        }
    }
    if original_len != hashes.len() {
        tracing::info!(
            supplied_count = original_len,
            canonical_count = hashes.len(),
            "deduplicated batch order hashes"
        );
    }
    Ok(hashes)
}

fn validation_error<T>(message: impl Into<String>) -> Result<T, ApiError> {
    let message = message.into();
    tracing::warn!(%message, "invalid batch trades query");
    Err(ApiError::BadRequest(message))
}

fn log_trades_validation_error(error: ApiError) -> ApiError {
    tracing::warn!(%error, "invalid batch trades query");
    error
}

fn trades_query_cache_key(query: &ValidatedTradesQuery) -> String {
    let addresses = |values: &[Address]| {
        values
            .iter()
            .map(|address| format!("{address:#x}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let denomination = |value| match value {
        Denomination::Wrapped => "wrapped",
        Denomination::Unwrapped => "unwrapped",
    };

    match query {
        ValidatedTradesQuery::OrderHashes(query) => {
            let hashes = query
                .canonical_order_hashes
                .iter()
                .map(|hash| format!("{hash:#x}"))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "trades-query/v2/order-hashes/{}/{}/{}/{start_time:?}/{end_time:?}/{}",
                query.chain_id.unwrap_or_default(),
                addresses(&query.token_addresses),
                hashes,
                denomination(query.denomination),
                start_time = query.start_time,
                end_time = query.end_time,
            )
        }
        ValidatedTradesQuery::Tokens(query) => format!(
            "trades-query/v2/tokens/{}/{}/{}/{}/{}/{}/{}",
            query.chain_id,
            addresses(&query.token_addresses),
            query.start_time,
            query.end_time,
            query.page,
            query.page_size,
            denomination(query.denomination)
        ),
    }
}

async fn compute_trades_query(
    ds: &dyn TradesDataSource,
    query: &ValidatedTradesQuery,
) -> Result<TradesQueryResponse, ApiError> {
    match query {
        ValidatedTradesQuery::OrderHashes(query) => compute_order_hash_trades(ds, query).await,
        ValidatedTradesQuery::Tokens(query) => compute_token_trades(ds, query).await,
    }
}

fn token_filter(tokens: &[Address]) -> Option<GetTradesTokenFilter> {
    (!tokens.is_empty()).then(|| GetTradesTokenFilter {
        inputs: Some(tokens.to_vec()),
        outputs: Some(tokens.to_vec()),
    })
}

async fn compute_order_hash_trades(
    ds: &dyn TradesDataSource,
    query: &ValidatedOrderHashTradesQuery,
) -> Result<TradesQueryResponse, ApiError> {
    tracing::info!(
        chain_id = query.chain_id,
        order_hashes_count = query.canonical_order_hashes.len(),
        token_addresses_count = query.token_addresses.len(),
        "executing one SDK grouped trades query"
    );
    let result = ds
        .get_trades_by_order_hashes_query(
            query.chain_id,
            query.canonical_order_hashes.clone(),
            GetTradesByOrderHashesFilters {
                tokens: token_filter(&query.token_addresses),
                time_filter: Some(TimeFilter {
                    start: query.start_time,
                    end: query.end_time,
                }),
                ..Default::default()
            },
        )
        .await?;

    let mut grouped_trades = result
        .trades_by_order_hash()
        .iter()
        .map(|entry| {
            let mut trades = entry.trades().to_vec();
            deduplicate_and_sort_trades(&mut trades);
            (entry.order_hash(), trades)
        })
        .collect::<Vec<_>>();
    grouped_trades.sort_by_key(|(order_hash, _)| *order_hash);
    let wrap_ratios = current_wrap_ratios_for_trades(
        ds,
        query.denomination,
        grouped_trades.iter().flat_map(|(_, trades)| trades.iter()),
    )
    .await?;
    let trades_by_order_hash = grouped_trades
        .into_iter()
        .map(|(order_hash, trades)| {
            let trades = trades
                .iter()
                .map(|trade| map_trade_for_list(trade, query.denomination, &wrap_ratios))
                .collect::<Result<Vec<_>, ApiError>>()?;
            Ok(TradesByOrderHashEntry { order_hash, trades })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let total_count = trades_by_order_hash
        .iter()
        .map(|entry| entry.trades.len() as u64)
        .sum();

    Ok(TradesQueryResponse::ByOrderHashes(
        TradesByOrderHashesResponse {
            trades_by_order_hash,
            total_count,
        },
    ))
}

async fn compute_token_trades(
    ds: &dyn TradesDataSource,
    query: &ValidatedTokenTradesQuery,
) -> Result<TradesQueryResponse, ApiError> {
    tracing::info!(
        chain_id = query.chain_id,
        batch_size = query.token_addresses.len(),
        page = query.page,
        page_size = query.page_size,
        "executing one SDK token-set trades query"
    );
    let result = ds
        .get_trades_query(
            query.chain_id,
            GetTradesFilters {
                tokens: token_filter(&query.token_addresses),
                time_filter: Some(TimeFilter {
                    start: Some(query.start_time),
                    end: Some(query.end_time),
                }),
                ..Default::default()
            },
            query.page,
            query.page_size,
        )
        .await?;
    let mut trades = result.trades().to_vec();
    let returned_count = trades.len();
    deduplicate_and_sort_trades(&mut trades);
    let duplicate_count = returned_count.saturating_sub(trades.len()) as u64;
    let total_trades = result.total_count().saturating_sub(duplicate_count);
    super::build_trades_list_response_from_parts(
        ds,
        &trades,
        total_trades,
        query.page.into(),
        query.page_size.into(),
        query.denomination,
    )
    .await
    .map(TradesQueryResponse::ByTokens)
}

fn deduplicate_and_sort_trades(trades: &mut Vec<RaindexTrade>) {
    let original_len = trades.len();
    let mut seen = HashSet::new();
    trades.retain(|trade| seen.insert((trade.chain_id(), trade.id())));
    trades.sort_by_cached_key(|trade| (std::cmp::Reverse(trade.timestamp()), trade.id()));
    tracing::info!(
        returned_count = original_len,
        deduplicated_count = trades.len(),
        "canonicalized batch trades result"
    );
}

fn trades_query_cache_admissible(response: &TradesQueryResponse) -> bool {
    match response {
        TradesQueryResponse::ByOrderHashes(response) => {
            response.total_count <= MAX_CACHED_GROUPED_TRADES
        }
        TradesQueryResponse::ByTokens(_) => true,
    }
}

fn restore_legacy_hash_order(
    response: TradesQueryResponse,
    requested_order: &[B256],
) -> TradesQueryResponse {
    let TradesQueryResponse::ByOrderHashes(mut response) = response else {
        return response;
    };
    let mut entries = response
        .trades_by_order_hash
        .into_iter()
        .map(|entry| (entry.order_hash, entry))
        .collect::<HashMap<_, _>>();
    response.trades_by_order_hash = requested_order
        .iter()
        .filter_map(|order_hash| entries.remove(order_hash))
        .collect();
    TradesQueryResponse::ByOrderHashes(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::RouteResponseCaches;
    use crate::routes::order::test_fixtures::trade_json;
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::trades::{
        RaindexTradesByOrderHashResult, RaindexTradesListResult,
    };
    use rain_orderbook_common::raindex_client::types::PaginationParams;
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug, Clone)]
    struct CapturedTokenQuery {
        chain_id: u32,
        filters: GetTradesFilters,
        page: u16,
        page_size: u16,
    }

    struct MockTradesDataSource {
        list_result: Result<RaindexTradesListResult, ApiError>,
        grouped_result: Result<RaindexTradesByOrderHashResult, ApiError>,
        calls: AtomicUsize,
        token_query: Mutex<Option<CapturedTokenQuery>>,
        grouped_hashes: Mutex<Vec<B256>>,
        delay: Duration,
        cache_safe: bool,
    }

    #[async_trait]
    impl TradesDataSource for MockTradesDataSource {
        async fn get_trades_by_tx(
            &self,
            _tx_hash: B256,
        ) -> Result<RaindexTradesListResult, ApiError> {
            unimplemented!()
        }

        async fn get_trades_for_owner(
            &self,
            _owner: Address,
            _pagination: PaginationParams,
            _time_filter: TimeFilter,
        ) -> Result<RaindexTradesListResult, ApiError> {
            unimplemented!()
        }

        async fn get_trades_for_token(
            &self,
            _token: Address,
            _page: u16,
            _page_size: u16,
            _time_filter: TimeFilter,
        ) -> Result<RaindexTradesListResult, ApiError> {
            unimplemented!()
        }

        async fn get_trades_for_taker(
            &self,
            _taker: Address,
            _page: u16,
            _page_size: u16,
            _time_filter: TimeFilter,
        ) -> Result<RaindexTradesListResult, ApiError> {
            unimplemented!()
        }

        async fn get_trades_query(
            &self,
            chain_id: u32,
            filters: GetTradesFilters,
            page: u16,
            page_size: u16,
        ) -> Result<RaindexTradesListResult, ApiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.token_query.lock().unwrap() = Some(CapturedTokenQuery {
                chain_id,
                filters,
                page,
                page_size,
            });
            tokio::time::sleep(self.delay).await;
            self.list_result.clone()
        }

        async fn get_trades_by_order_hashes_query(
            &self,
            _chain_id: Option<u32>,
            order_hashes: Vec<B256>,
            _filters: GetTradesByOrderHashesFilters,
        ) -> Result<RaindexTradesByOrderHashResult, ApiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.grouped_hashes.lock().unwrap() = order_hashes;
            tokio::time::sleep(self.delay).await;
            self.grouped_result.clone()
        }

        fn query_subgraph_count(&self, _chain_id: Option<u32>) -> Result<usize, ApiError> {
            Ok(if self.cache_safe { 1 } else { 2 })
        }
    }

    fn hash_a() -> B256 {
        B256::from_str("0x000000000000000000000000000000000000000000000000000000000000abcd")
            .unwrap()
    }

    fn hash_b() -> B256 {
        B256::from_str("0x000000000000000000000000000000000000000000000000000000000000beef")
            .unwrap()
    }

    fn empty_list_result() -> RaindexTradesListResult {
        serde_json::from_value(json!({"trades": [], "totalCount": 0, "summary": null})).unwrap()
    }

    fn empty_grouped_result() -> RaindexTradesByOrderHashResult {
        serde_json::from_value(json!({"tradesByOrderHash": [], "totalCount": 0})).unwrap()
    }

    fn mock_ds() -> MockTradesDataSource {
        MockTradesDataSource {
            list_result: Ok(empty_list_result()),
            grouped_result: Ok(empty_grouped_result()),
            calls: AtomicUsize::new(0),
            token_query: Mutex::new(None),
            grouped_hashes: Mutex::new(vec![]),
            delay: Duration::ZERO,
            cache_safe: true,
        }
    }

    fn token_request(tokens: Vec<String>) -> TradesQueryRequest {
        TradesQueryRequest {
            order_hashes: None,
            token_addresses: tokens,
            chain_id: Some(8453),
            start_time: Some(1_700_000_000),
            end_time: Some(1_700_003_600),
            page: None,
            page_size: None,
            denomination: None,
        }
    }

    fn grouped_result() -> RaindexTradesByOrderHashResult {
        serde_json::from_value(json!({
            "tradesByOrderHash": [
                {"orderHash": hash_a(), "trades": [trade_json()]},
                {"orderHash": hash_b(), "trades": []}
            ],
            "totalCount": 1
        }))
        .unwrap()
    }

    fn trade(id: &str, timestamp: u64) -> serde_json::Value {
        let mut trade = trade_json();
        trade["id"] = json!(id);
        trade["timestamp"] = json!(format!("0x{timestamp:064x}"));
        trade
    }

    #[test]
    fn token_cache_key_is_case_and_order_insensitive() {
        let first = validate_trades_query(token_request(vec![
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            "0x4200000000000000000000000000000000000006".into(),
        ]))
        .unwrap();
        let second = validate_trades_query(token_request(vec![
            "0x4200000000000000000000000000000000000006".into(),
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913".into(),
            "0x4200000000000000000000000000000000000006".into(),
        ]))
        .unwrap();
        assert_eq!(
            trades_query_cache_key(&first),
            trades_query_cache_key(&second)
        );
    }

    #[test]
    fn order_hash_cache_key_is_order_insensitive() {
        let request = |hashes: Vec<String>| TradesQueryRequest {
            order_hashes: Some(hashes),
            token_addresses: vec![],
            chain_id: None,
            start_time: Some(1),
            end_time: Some(2),
            page: None,
            page_size: None,
            denomination: None,
        };
        let first =
            validate_trades_query(request(vec![hash_a().to_string(), hash_b().to_string()]))
                .unwrap();
        let second =
            validate_trades_query(request(vec![hash_b().to_string(), hash_a().to_string()]))
                .unwrap();
        assert_eq!(
            trades_query_cache_key(&first),
            trades_query_cache_key(&second)
        );
    }

    #[test]
    fn oversized_grouped_response_is_not_cache_admissible() {
        let response = TradesQueryResponse::ByOrderHashes(TradesByOrderHashesResponse {
            trades_by_order_hash: vec![],
            total_count: MAX_CACHED_GROUPED_TRADES + 1,
        });
        assert!(!trades_query_cache_admissible(&response));
    }

    #[test]
    fn token_mode_validates_time_and_page_bounds() {
        let mut request = token_request(vec!["0x4200000000000000000000000000000000000006".into()]);
        request.end_time = Some(request.start_time.unwrap() + MAX_TOKEN_TIME_RANGE_SECONDS + 1);
        assert!(validate_trades_query(request).is_err());

        let mut request = token_request(vec!["0x4200000000000000000000000000000000000006".into()]);
        request.page_size = Some(MAX_PAGE_SIZE);
        assert!(validate_trades_query(request).is_ok());

        let mut request = token_request(vec!["0x4200000000000000000000000000000000000006".into()]);
        request.page_size = Some(MAX_PAGE_SIZE + 1);
        assert!(validate_trades_query(request).is_err());
    }

    #[rocket::async_test]
    async fn token_mode_uses_one_sdk_query_and_returns_empty_page() {
        let ds = mock_ds();
        let caches = RouteResponseCaches::new(0, Duration::ZERO);
        let response = process_trades_query(
            &ds,
            &caches,
            token_request(vec!["0x4200000000000000000000000000000000000006".into()]),
        )
        .await
        .unwrap();
        let TradesQueryResponse::ByTokens(response) = response else {
            panic!("expected token response");
        };
        assert!(response.trades.is_empty());
        assert_eq!(response.pagination.total_trades, 0);
        let captured = ds.token_query.lock().unwrap().clone().unwrap();
        assert_eq!(captured.chain_id, 8453);
        assert_eq!(captured.page, 1);
        assert_eq!(captured.page_size, 20);
        let tokens = captured.filters.tokens.unwrap();
        assert_eq!(tokens.inputs, tokens.outputs);
        assert_eq!(ds.calls.load(Ordering::SeqCst), 1);
    }

    #[rocket::async_test]
    async fn legacy_order_hash_mode_preserves_requested_group_order() {
        let ds = MockTradesDataSource {
            grouped_result: Ok(grouped_result()),
            ..mock_ds()
        };
        let caches = RouteResponseCaches::new(0, Duration::ZERO);
        let request = TradesQueryRequest {
            order_hashes: Some(vec![hash_b().to_string(), hash_a().to_string()]),
            token_addresses: vec![],
            chain_id: None,
            start_time: Some(1),
            end_time: Some(2),
            page: None,
            page_size: None,
            denomination: None,
        };
        let response = process_trades_query(&ds, &caches, request).await.unwrap();
        let TradesQueryResponse::ByOrderHashes(response) = response else {
            panic!("expected grouped response");
        };
        assert_eq!(response.trades_by_order_hash[0].order_hash, hash_b());
        assert_eq!(response.trades_by_order_hash[1].order_hash, hash_a());
        assert_eq!(*ds.grouped_hashes.lock().unwrap(), vec![hash_a(), hash_b()]);
    }

    #[rocket::async_test]
    async fn legacy_explicit_empty_order_hashes_stays_grouped() {
        let ds = mock_ds();
        let request = TradesQueryRequest {
            order_hashes: Some(vec![]),
            token_addresses: vec![],
            chain_id: None,
            start_time: None,
            end_time: None,
            page: None,
            page_size: None,
            denomination: None,
        };
        let response =
            process_trades_query(&ds, &RouteResponseCaches::new(0, Duration::ZERO), request)
                .await
                .unwrap();
        let TradesQueryResponse::ByOrderHashes(response) = response else {
            panic!("expected grouped response");
        };
        assert!(response.trades_by_order_hash.is_empty());
        assert_eq!(response.total_count, 0);
    }

    #[rocket::async_test]
    async fn duplicate_trades_are_removed_before_pagination_metadata() {
        let result: RaindexTradesListResult = serde_json::from_value(json!({
            "trades": [trade_json(), trade_json()],
            "totalCount": 2,
            "summary": null
        }))
        .unwrap();
        let ds = MockTradesDataSource {
            list_result: Ok(result),
            ..mock_ds()
        };
        let response = process_trades_query(
            &ds,
            &RouteResponseCaches::new(0, Duration::ZERO),
            token_request(vec!["0x4200000000000000000000000000000000000006".into()]),
        )
        .await
        .unwrap();
        let TradesQueryResponse::ByTokens(response) = response else {
            panic!("expected token response");
        };
        assert_eq!(response.trades.len(), 1);
        assert_eq!(response.pagination.total_trades, 1);
    }

    #[rocket::async_test]
    async fn token_results_use_deterministic_timestamp_ordering() {
        let older_id = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let newer_id = "0x0000000000000000000000000000000000000000000000000000000000000002";
        let result: RaindexTradesListResult = serde_json::from_value(json!({
            "trades": [trade(older_id, 1), trade(newer_id, 2)],
            "totalCount": 2,
            "summary": null
        }))
        .unwrap();
        let ds = MockTradesDataSource {
            list_result: Ok(result),
            ..mock_ds()
        };
        let response = process_trades_query(
            &ds,
            &RouteResponseCaches::new(0, Duration::ZERO),
            token_request(vec!["0x4200000000000000000000000000000000000006".into()]),
        )
        .await
        .unwrap();
        let TradesQueryResponse::ByTokens(response) = response else {
            panic!("expected token response");
        };
        assert_eq!(
            response
                .trades
                .iter()
                .map(|trade| trade.timestamp)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[rocket::async_test]
    async fn concurrent_identical_cold_requests_compute_once() {
        let ds = Arc::new(MockTradesDataSource {
            delay: Duration::from_millis(25),
            ..mock_ds()
        });
        let caches = Arc::new(RouteResponseCaches::new(100, Duration::from_secs(60)));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let ds = Arc::clone(&ds);
            let caches = Arc::clone(&caches);
            tasks.spawn(async move {
                process_trades_query(
                    ds.as_ref(),
                    caches.as_ref(),
                    token_request(vec!["0x4200000000000000000000000000000000000006".into()]),
                )
                .await
            });
        }
        while let Some(result) = tasks.join_next().await {
            assert!(result.unwrap().is_ok());
        }
        assert_eq!(ds.calls.load(Ordering::SeqCst), 1);
    }

    #[rocket::async_test]
    async fn failures_are_not_cached() {
        let ds = MockTradesDataSource {
            list_result: Err(ApiError::Internal("sdk error".into())),
            ..mock_ds()
        };
        let caches = RouteResponseCaches::new(100, Duration::from_secs(60));
        for _ in 0..2 {
            assert!(process_trades_query(
                &ds,
                &caches,
                token_request(vec!["0x4200000000000000000000000000000000000006".into(),]),
            )
            .await
            .is_err());
        }
        assert_eq!(ds.calls.load(Ordering::SeqCst), 2);
    }

    #[rocket::async_test]
    async fn multi_subgraph_token_scope_is_rejected_before_querying_sdk() {
        let ds = MockTradesDataSource {
            cache_safe: false,
            ..mock_ds()
        };
        let caches = RouteResponseCaches::new(100, Duration::from_secs(60));
        assert!(process_trades_query(
            &ds,
            &caches,
            token_request(vec!["0x4200000000000000000000000000000000000006".into(),]),
        )
        .await
        .is_err());
        assert_eq!(ds.calls.load(Ordering::SeqCst), 0);
    }

    #[rocket::async_test]
    async fn potentially_partial_legacy_results_bypass_response_cache() {
        let ds = MockTradesDataSource {
            cache_safe: false,
            ..mock_ds()
        };
        let caches = RouteResponseCaches::new(100, Duration::from_secs(60));
        let request = || TradesQueryRequest {
            order_hashes: Some(vec![]),
            token_addresses: vec![],
            chain_id: None,
            start_time: None,
            end_time: None,
            page: None,
            page_size: None,
            denomination: None,
        };
        for _ in 0..2 {
            assert!(process_trades_query(&ds, &caches, request()).await.is_ok());
        }
        assert_eq!(ds.calls.load(Ordering::SeqCst), 2);
    }

    #[rocket::async_test]
    async fn invalid_hash_returns_400_from_route() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let response = client
            .post("/v1/trades/query")
            .header(Header::new(
                "Authorization",
                basic_auth_header(&key_id, &secret),
            ))
            .header(ContentType::JSON)
            .body(r#"{"orderHashes":["not-a-hash"]}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
    }
}
