use super::{
    active_filter_for_state, build_orders_list_response, current_wrap_ratios_for_orders,
    get_order_quotes_for_summaries, quote_set_is_complete, BatchOrdersDataSource,
    RaindexOrdersListDataSource, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
use crate::app_state::ApplicationState;
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorCode, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::routes::batch_query::{parse_canonical_addresses, validate_configured_chain};
use crate::types::common::Denomination;
use crate::types::orders::{OrderSide, OrderState, OrdersListResponse, OrdersQueryRequest};
use alloy::primitives::{Address, B256};
use rain_orderbook_common::raindex_client::orders::{
    GetOrdersFilters, GetOrdersTokenFilter, RaindexOrder,
};
use rocket::serde::json::Json;
use rocket::State;
use std::collections::HashSet;
use std::str::FromStr;
use tracing::Instrument;

const MAX_ADDRESS_FILTERS: usize = 64;
const MAX_PAGE: u16 = 1_000;

#[derive(Debug, Clone)]
struct ValidatedOrdersQuery {
    chain_id: u32,
    token_addresses: Vec<Address>,
    owner_addresses: Vec<Address>,
    raindex_addresses: Vec<Address>,
    order_hash: Option<B256>,
    state: Option<OrderState>,
    side: Option<OrderSide>,
    page: u16,
    page_size: u16,
    denomination: Denomination,
}

#[utoipa::path(
    post,
    path = "/v2/orders/query",
    tag = "Orders",
    security(("basicAuth" = [])),
    request_body = OrdersQueryRequest,
    responses(
        (status = 200, description = "Bounded orders page matching the batch filters", body = OrdersListResponse),
        (status = 400, description = "Invalid batch filters or bounds", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 422, description = "Request body could not be deserialized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 502, description = "Order source or live quote query failed", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/query", data = "<request>")]
pub async fn post_orders_query(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    pool: &State<DbPool>,
    app_state: &State<ApplicationState>,
    span: TracingSpan,
    request: Json<OrdersQueryRequest>,
) -> Result<Json<OrdersListResponse>, ApiError> {
    async move {
        let request = request.into_inner();
        tracing::info!(
            chain_id = request.chain_id,
            token_addresses_count = request.token_addresses.len(),
            owner_addresses_count = request.owner_addresses.len(),
            raindex_addresses_count = request.raindex_addresses.len(),
            has_order_hash = request.order_hash.is_some(),
            "batch orders query request received"
        );

        let raindex = shared_raindex.read().await;
        validate_configured_chain(raindex.client(), request.chain_id)?;

        let ds = RaindexOrdersListDataSource {
            client: raindex.client(),
            caches: &app_state.response_caches,
            pool: pool.inner(),
        };
        process_orders_query(&ds, &app_state.response_caches, request)
            .await
            .map(Json)
    }
    .instrument(span.0)
    .await
}

pub(crate) async fn process_orders_query(
    ds: &dyn BatchOrdersDataSource,
    caches: &crate::cache::RouteResponseCaches,
    request: OrdersQueryRequest,
) -> Result<OrdersListResponse, ApiError> {
    let query = validate_orders_query(request)?;
    let cache_key = orders_query_cache_key(&query);

    if !caches.is_enabled() {
        tracing::info!(
            chain_id = query.chain_id,
            token_addresses_count = query.token_addresses.len(),
            has_order_hash = query.order_hash.is_some(),
            "batch orders response cache bypassed"
        );
        return compute_orders_query(ds, &query).await;
    }

    if let Some(response) = caches.orders_query.get(&cache_key).await {
        tracing::info!(
            chain_id = query.chain_id,
            token_addresses_count = query.token_addresses.len(),
            has_order_hash = query.order_hash.is_some(),
            cache_hit = true,
            "batch orders response cache hit"
        );
        return Ok(response);
    }

    tracing::info!(
        chain_id = query.chain_id,
        token_addresses_count = query.token_addresses.len(),
        has_order_hash = query.order_hash.is_some(),
        cache_hit = false,
        "batch orders response cache miss"
    );
    caches
        .orders_query
        .get_or_try_insert(cache_key, || compute_orders_query(ds, &query))
        .await
        .map_err(|error| error.as_ref().clone())
}

fn validate_orders_query(request: OrdersQueryRequest) -> Result<ValidatedOrdersQuery, ApiError> {
    if request.chain_id == 0 {
        return validation_error("chainId must be greater than zero");
    }

    let token_addresses = parse_canonical_addresses(
        "tokenAddresses",
        request.token_addresses,
        MAX_ADDRESS_FILTERS,
    )
    .map_err(log_orders_validation_error)?;
    let owner_addresses = parse_canonical_addresses(
        "ownerAddresses",
        request.owner_addresses,
        MAX_ADDRESS_FILTERS,
    )
    .map_err(log_orders_validation_error)?;
    let raindex_addresses = parse_canonical_addresses(
        "raindexAddresses",
        request.raindex_addresses,
        MAX_ADDRESS_FILTERS,
    )
    .map_err(log_orders_validation_error)?;
    let order_hash = request
        .order_hash
        .map(|hash| {
            B256::from_str(&hash).map_err(|error| {
                tracing::warn!(input = %hash, %error, "invalid order hash");
                ApiError::BadRequest("invalid orderHash".into())
            })
        })
        .transpose()?;

    if token_addresses.is_empty() && order_hash.is_none() {
        return validation_error("tokenAddresses or orderHash is required");
    }

    let page = request.page.unwrap_or(1);
    if page == 0 || page > MAX_PAGE {
        return validation_error(format!("page must be between 1 and {MAX_PAGE}"));
    }
    let page_size = request.page_size.unwrap_or(DEFAULT_PAGE_SIZE as u16);
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        return validation_error(format!("pageSize must be between 1 and {MAX_PAGE_SIZE}"));
    }

    Ok(ValidatedOrdersQuery {
        chain_id: request.chain_id,
        token_addresses,
        owner_addresses,
        raindex_addresses,
        order_hash,
        state: request.state,
        side: request.side,
        page,
        page_size,
        denomination: request.denomination.unwrap_or_default(),
    })
}

fn validation_error<T>(message: impl Into<String>) -> Result<T, ApiError> {
    let message = message.into();
    tracing::warn!(%message, "invalid batch orders query");
    Err(ApiError::BadRequest(message))
}

fn log_orders_validation_error(error: ApiError) -> ApiError {
    tracing::warn!(%error, "invalid batch orders query");
    error
}

fn orders_query_cache_key(query: &ValidatedOrdersQuery) -> String {
    let addresses = |values: &[Address]| {
        values
            .iter()
            .map(|address| format!("{address:#x}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let state = match query.state.unwrap_or(OrderState::Active) {
        OrderState::Active => "active",
        OrderState::Inactive => "inactive",
        OrderState::All => "all",
    };
    let side = match query.side {
        Some(OrderSide::Input) => "input",
        Some(OrderSide::Output) => "output",
        None => "any",
    };
    let denomination = match query.denomination {
        Denomination::Wrapped => "wrapped",
        Denomination::Unwrapped => "unwrapped",
    };
    let order_hash = query
        .order_hash
        .map(|hash| format!("{hash:#x}"))
        .unwrap_or_default();

    format!(
        "orders-query/v1/{}/{}/{}/{}/{}/{}/{}/{}/{}/{}",
        query.chain_id,
        addresses(&query.token_addresses),
        addresses(&query.owner_addresses),
        addresses(&query.raindex_addresses),
        order_hash,
        state,
        side,
        query.page,
        query.page_size,
        denomination
    )
}

async fn compute_orders_query(
    ds: &dyn BatchOrdersDataSource,
    query: &ValidatedOrdersQuery,
) -> Result<OrdersListResponse, ApiError> {
    let token_filter = if query.token_addresses.is_empty() {
        None
    } else {
        Some(match query.side {
            Some(OrderSide::Input) => GetOrdersTokenFilter {
                inputs: Some(query.token_addresses.clone()),
                outputs: None,
            },
            Some(OrderSide::Output) => GetOrdersTokenFilter {
                inputs: None,
                outputs: Some(query.token_addresses.clone()),
            },
            None => GetOrdersTokenFilter {
                inputs: Some(query.token_addresses.clone()),
                outputs: Some(query.token_addresses.clone()),
            },
        })
    };
    let active = active_filter_for_state(query.state);
    let filters = GetOrdersFilters {
        owners: query.owner_addresses.clone(),
        active,
        order_hash: query.order_hash,
        tokens: token_filter,
        raindex_addresses: (!query.raindex_addresses.is_empty())
            .then(|| query.raindex_addresses.clone()),
        has_positive_output_vault_balance: (active == Some(true)).then_some(true),
    };

    tracing::info!(
        chain_id = query.chain_id,
        batch_size = query.token_addresses.len(),
        page = query.page,
        page_size = query.page_size,
        "executing one SDK batch orders query"
    );
    let (mut orders, total_count) = ds
        .get_orders_query(query.chain_id, filters, query.page, query.page_size)
        .await?;
    deduplicate_and_sort_orders(&mut orders);

    let quote_results = get_order_quotes_for_summaries(ds, &orders).await;
    let failed_quote_count = orders
        .iter()
        .zip(&quote_results)
        .filter(|(order, result)| {
            order.active()
                && !result
                    .as_ref()
                    .is_ok_and(|quotes| quote_set_is_complete(quotes))
        })
        .count();
    if failed_quote_count > 0 {
        tracing::error!(
            chain_id = query.chain_id,
            batch_size = query.token_addresses.len(),
            failed_quote_count,
            code = %ApiErrorCode::OrdersQueryFailed,
            "batch order live quote query failed"
        );
        return Err(ApiError::coded(
            ApiErrorCode::OrdersQueryFailed,
            "the live order quotes could not be computed",
        ));
    }
    let wrap_ratios = current_wrap_ratios_for_orders(ds, query.denomination, &orders).await?;
    build_orders_list_response(
        &orders,
        total_count,
        query.page.into(),
        query.page_size.into(),
        quote_results,
        query.denomination,
        &wrap_ratios,
    )
}

fn deduplicate_and_sort_orders(orders: &mut Vec<RaindexOrder>) {
    let original_len = orders.len();
    let mut seen = HashSet::new();
    orders.retain(|order| seen.insert((order.chain_id(), order.raindex(), order.order_hash())));
    orders.sort_by(|left, right| {
        right
            .timestamp_added()
            .cmp(&left.timestamp_added())
            .then_with(|| left.order_hash().cmp(&right.order_hash()))
            .then_with(|| left.raindex().cmp(&right.raindex()))
    });
    tracing::info!(
        returned_count = original_len,
        deduplicated_count = orders.len(),
        "canonicalized batch orders result"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::RouteResponseCaches;
    use crate::routes::order::test_fixtures::{mock_failed_quote, mock_quote, order_json};
    use crate::routes::orders::OrdersListDataSource;
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::order_quotes::RaindexOrderQuote;
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct MockDataSource {
        orders: Vec<RaindexOrder>,
        quotes: Result<Vec<RaindexOrderQuote>, ApiError>,
        calls: AtomicUsize,
        filters: Mutex<Vec<GetOrdersFilters>>,
        delay: Duration,
    }

    #[async_trait]
    impl OrdersListDataSource for MockDataSource {
        async fn get_orders_list(
            &self,
            _chain_ids: Option<Vec<u32>>,
            _filters: GetOrdersFilters,
            _page: Option<u16>,
            _page_size: Option<u16>,
        ) -> Result<(Vec<RaindexOrder>, u32), ApiError> {
            unreachable!("batch tests use get_orders_query")
        }

        async fn get_order_quotes(
            &self,
            _order: &RaindexOrder,
        ) -> Result<Vec<RaindexOrderQuote>, ApiError> {
            self.quotes.clone()
        }
    }

    #[async_trait]
    impl BatchOrdersDataSource for MockDataSource {
        async fn get_orders_query(
            &self,
            _chain_id: u32,
            filters: GetOrdersFilters,
            _page: u16,
            _page_size: u16,
        ) -> Result<(Vec<RaindexOrder>, u32), ApiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.filters.lock().unwrap().push(filters);
            tokio::time::sleep(self.delay).await;
            Ok((self.orders.clone(), self.orders.len() as u32))
        }
    }

    fn request(tokens: Vec<String>) -> OrdersQueryRequest {
        OrdersQueryRequest {
            chain_id: 8453,
            token_addresses: tokens,
            owner_addresses: vec![],
            raindex_addresses: vec![],
            order_hash: None,
            state: None,
            side: None,
            page: None,
            page_size: None,
            denomination: None,
        }
    }

    fn order(hash: &str, created_at: u64) -> RaindexOrder {
        let mut value = order_json();
        value["orderHash"] = json!(hash);
        value["timestampAdded"] = json!(format!("0x{created_at:x}"));
        serde_json::from_value(value).unwrap()
    }

    fn mock_ds(orders: Vec<RaindexOrder>) -> MockDataSource {
        MockDataSource {
            orders,
            quotes: Ok(vec![mock_quote("2")]),
            calls: AtomicUsize::new(0),
            filters: Mutex::new(vec![]),
            delay: Duration::ZERO,
        }
    }

    #[test]
    fn cache_key_is_address_case_and_order_insensitive() {
        let first = validate_orders_query(request(vec![
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            "0x4200000000000000000000000000000000000006".into(),
        ]))
        .unwrap();
        let second = validate_orders_query(request(vec![
            "0x4200000000000000000000000000000000000006".into(),
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913".into(),
            "0x4200000000000000000000000000000000000006".into(),
        ]))
        .unwrap();
        assert_eq!(
            orders_query_cache_key(&first),
            orders_query_cache_key(&second)
        );
    }

    #[test]
    fn validation_enforces_bounds_and_required_filter() {
        assert!(validate_orders_query(request(vec![])).is_err());
        let mut too_many = request(vec![
            "0x4200000000000000000000000000000000000006".into();
            MAX_ADDRESS_FILTERS + 1
        ]);
        assert!(validate_orders_query(too_many.clone()).is_err());
        too_many.token_addresses.truncate(1);
        too_many.page_size = Some(MAX_PAGE_SIZE + 1);
        assert!(validate_orders_query(too_many).is_err());
    }

    #[rocket::async_test]
    async fn query_deduplicates_orders_and_sorts_deterministically() {
        let older_hash = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let newer_hash = "0x0000000000000000000000000000000000000000000000000000000000000002";
        let newer = order(newer_hash, 2);
        let ds = mock_ds(vec![order(older_hash, 1), newer.clone(), newer]);
        let caches = RouteResponseCaches::new(0, Duration::ZERO);
        let response = process_orders_query(
            &ds,
            &caches,
            request(vec!["0x4200000000000000000000000000000000000006".into()]),
        )
        .await
        .unwrap();

        assert_eq!(response.orders.len(), 2);
        assert_eq!(
            response.orders[0].order_hash,
            B256::from_str(newer_hash).unwrap()
        );
        let filters = ds.filters.lock().unwrap();
        let tokens = filters[0].tokens.as_ref().unwrap();
        assert_eq!(tokens.inputs, tokens.outputs);
    }

    #[rocket::async_test]
    async fn identical_concurrent_cold_requests_compute_once() {
        let ds = Arc::new(MockDataSource {
            delay: Duration::from_millis(25),
            ..mock_ds(vec![])
        });
        let caches = Arc::new(RouteResponseCaches::new(100, Duration::from_secs(60)));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let ds = Arc::clone(&ds);
            let caches = Arc::clone(&caches);
            tasks.spawn(async move {
                process_orders_query(
                    ds.as_ref(),
                    caches.as_ref(),
                    request(vec!["0x4200000000000000000000000000000000000006".into()]),
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
    async fn sequential_identical_requests_use_cached_response() {
        let ds = mock_ds(vec![]);
        let caches = RouteResponseCaches::new(100, Duration::from_secs(60));
        let query = request(vec!["0x4200000000000000000000000000000000000006".into()]);

        assert!(process_orders_query(&ds, &caches, query.clone())
            .await
            .is_ok());
        assert!(process_orders_query(&ds, &caches, query).await.is_ok());
        assert_eq!(ds.calls.load(Ordering::SeqCst), 1);
    }

    #[rocket::async_test]
    async fn order_hash_only_request_is_valid() {
        let ds = mock_ds(vec![]);
        let caches = RouteResponseCaches::new(0, Duration::ZERO);
        let mut query = request(vec![]);
        let order_hash =
            B256::from_str("0x0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        query.order_hash = Some(order_hash.to_string());

        assert!(process_orders_query(&ds, &caches, query).await.is_ok());
        assert_eq!(ds.calls.load(Ordering::SeqCst), 1);
        assert_eq!(ds.filters.lock().unwrap()[0].order_hash, Some(order_hash));
    }

    #[rocket::async_test]
    async fn incomplete_quote_returns_coded_error_and_is_not_cached() {
        let order = order(
            "0x0000000000000000000000000000000000000000000000000000000000000002",
            2,
        );
        let ds = MockDataSource {
            quotes: Ok(vec![mock_failed_quote()]),
            ..mock_ds(vec![order])
        };
        let caches = RouteResponseCaches::new(100, Duration::from_secs(60));
        for _ in 0..2 {
            let result = process_orders_query(
                &ds,
                &caches,
                request(vec!["0x4200000000000000000000000000000000000006".into()]),
            )
            .await;
            assert!(matches!(
                result,
                Err(ApiError::Coded {
                    code: ApiErrorCode::OrdersQueryFailed,
                    ..
                })
            ));
        }
        assert_eq!(ds.calls.load(Ordering::SeqCst), 2);
    }

    #[rocket::async_test]
    async fn quote_query_error_returns_coded_error() {
        let order = order(
            "0x0000000000000000000000000000000000000000000000000000000000000002",
            2,
        );
        let ds = MockDataSource {
            quotes: Err(ApiError::Internal("quote query failed".into())),
            ..mock_ds(vec![order])
        };
        let caches = RouteResponseCaches::new(0, Duration::ZERO);

        let result = process_orders_query(
            &ds,
            &caches,
            request(vec!["0x4200000000000000000000000000000000000006".into()]),
        )
        .await;
        assert!(matches!(
            result,
            Err(ApiError::Coded {
                code: ApiErrorCode::OrdersQueryFailed,
                ..
            })
        ));
    }

    #[rocket::async_test]
    async fn concurrent_quote_failures_are_coalesced_but_not_cached() {
        let order = order(
            "0x0000000000000000000000000000000000000000000000000000000000000002",
            2,
        );
        let ds = Arc::new(MockDataSource {
            quotes: Ok(vec![mock_failed_quote()]),
            delay: Duration::from_millis(25),
            ..mock_ds(vec![order])
        });
        let caches = Arc::new(RouteResponseCaches::new(100, Duration::from_secs(60)));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let ds = Arc::clone(&ds);
            let caches = Arc::clone(&caches);
            tasks.spawn(async move {
                process_orders_query(
                    ds.as_ref(),
                    caches.as_ref(),
                    request(vec!["0x4200000000000000000000000000000000000006".into()]),
                )
                .await
            });
        }
        while let Some(result) = tasks.join_next().await {
            assert!(matches!(
                result.unwrap(),
                Err(ApiError::Coded {
                    code: ApiErrorCode::OrdersQueryFailed,
                    ..
                })
            ));
        }
        assert_eq!(ds.calls.load(Ordering::SeqCst), 1);

        let result = process_orders_query(
            ds.as_ref(),
            caches.as_ref(),
            request(vec!["0x4200000000000000000000000000000000000006".into()]),
        )
        .await;
        assert!(matches!(
            result,
            Err(ApiError::Coded {
                code: ApiErrorCode::OrdersQueryFailed,
                ..
            })
        ));
        assert_eq!(ds.calls.load(Ordering::SeqCst), 2);
    }

    #[rocket::async_test]
    async fn route_rejects_empty_batch_before_querying_sdk() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let response = client
            .post("/v1/orders/query")
            .header(ContentType::JSON)
            .header(Header::new(
                "Authorization",
                basic_auth_header(&key_id, &secret),
            ))
            .body(json!({"chainId": 8453, "tokenAddresses": []}).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn route_rejects_unconfigured_chain() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let response = client
            .post("/v1/orders/query")
            .header(ContentType::JSON)
            .header(Header::new(
                "Authorization",
                basic_auth_header(&key_id, &secret),
            ))
            .body(
                json!({
                    "chainId": 1,
                    "tokenAddresses": ["0x4200000000000000000000000000000000000006"]
                })
                .to_string(),
            )
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
        let body = response.into_string().await.unwrap();
        assert!(body.contains("unsupported chainId"));
    }

    #[rocket::async_test]
    async fn one_batch_request_consumes_one_per_key_rate_limit_slot() {
        let client = TestClientBuilder::new()
            .rate_limiter(crate::fairings::RateLimiter::new(100, 1))
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let authorization = basic_auth_header(&key_id, &secret);
        let body = json!({"chainId": 8453, "tokenAddresses": []}).to_string();

        let first = client
            .post("/v1/orders/query")
            .header(ContentType::JSON)
            .header(Header::new("Authorization", authorization.clone()))
            .body(body.clone())
            .dispatch()
            .await;
        assert_eq!(first.status(), Status::BadRequest);

        let second = client
            .post("/v1/orders/query")
            .header(ContentType::JSON)
            .header(Header::new("Authorization", authorization))
            .body(body)
            .dispatch()
            .await;
        assert_eq!(second.status(), Status::TooManyRequests);
    }
}
