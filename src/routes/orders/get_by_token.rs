use super::{
    build_order_summary, build_pagination, OrdersListDataSource, RaindexOrdersListDataSource,
    DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
use crate::auth::AuthenticatedKey;
use crate::cache::AppCache;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedAddress;
use crate::types::orders::{OrderSide, OrdersByTokenParams, OrdersListResponse};
use alloy::primitives::Address;
use rain_orderbook_common::raindex_client::orders::GetOrdersFilters;
use rain_orderbook_common::raindex_client::orders::GetOrdersTokenFilter;
use rocket::serde::json::Json;
use rocket::State;
use std::time::{Duration, Instant};
use tracing::Instrument;

const ORDERS_CACHE_TTL: Duration = Duration::from_secs(15);
const ORDERS_CACHE_CAPACITY: u64 = 1_000;

pub(crate) type OrdersByTokenCache =
    AppCache<(Address, Option<OrderSide>, u16, u16), OrdersListResponse>;

pub(crate) fn orders_by_token_cache() -> OrdersByTokenCache {
    AppCache::new(ORDERS_CACHE_CAPACITY, ORDERS_CACHE_TTL)
}

pub(crate) async fn process_get_orders_by_token(
    ds: &dyn OrdersListDataSource,
    address: Address,
    side: Option<OrderSide>,
    page: Option<u16>,
    page_size: Option<u16>,
) -> Result<OrdersListResponse, ApiError> {
    let token_filter = match side {
        Some(OrderSide::Input) => GetOrdersTokenFilter {
            inputs: Some(vec![address]),
            outputs: None,
        },
        Some(OrderSide::Output) => GetOrdersTokenFilter {
            inputs: None,
            outputs: Some(vec![address]),
        },
        None => GetOrdersTokenFilter {
            inputs: Some(vec![address]),
            outputs: Some(vec![address]),
        },
    };

    let filters = GetOrdersFilters {
        active: Some(true),
        tokens: Some(token_filter),
        ..Default::default()
    };

    let total_start = Instant::now();
    let page_num = page.unwrap_or(1);
    let effective_page_size = page_size
        .unwrap_or(DEFAULT_PAGE_SIZE as u16)
        .min(MAX_PAGE_SIZE);

    let orders_stage_start = Instant::now();
    let (orders, total_count) = ds
        .get_orders_list(filters, Some(page_num), Some(effective_page_size))
        .await?;
    let orders_stage_duration_ms = orders_stage_start.elapsed().as_millis();

    // Separate orders with non-zero output balance (worth quoting) from empty ones
    let mut quotable_indices: Vec<usize> = Vec::new();
    let mut quotable_orders: Vec<rain_orderbook_common::raindex_client::orders::RaindexOrder> =
        Vec::new();
    for (i, order) in orders.iter().enumerate() {
        let has_balance = crate::routes::resolve_io_vaults(order)
            .map(|(_, output)| {
                output
                    .formatted_balance()
                    .parse::<f64>()
                    .is_ok_and(|b| b > 0.0)
            })
            .unwrap_or(false);
        if has_balance {
            quotable_indices.push(i);
            quotable_orders.push(order.clone());
        }
    }

    let quotes_stage_start = Instant::now();
    tracing::info!(
        total_orders = orders.len(),
        quotable_orders = quotable_orders.len(),
        skipped_zero_balance = orders.len() - quotable_orders.len(),
        "fetching batched quotes for orders by token"
    );
    let quote_results = ds.get_order_quotes_batch(&quotable_orders).await;
    let quotes_stage_duration_ms = quotes_stage_start.elapsed().as_millis();

    // Map quote results back to original order positions
    let mut io_ratios: Vec<String> = vec!["-".into(); orders.len()];
    for (qi, &original_idx) in quotable_indices.iter().enumerate() {
        io_ratios[original_idx] = super::quote_result_to_io_ratio(
            &orders[original_idx],
            quote_results
                .get(qi)
                .cloned()
                .unwrap_or_else(|| Err(ApiError::Internal("missing quote".into()))),
        );
    }

    let mut summaries = Vec::with_capacity(orders.len());
    for (order, io_ratio) in orders.iter().zip(io_ratios.iter()) {
        summaries.push(build_order_summary(order, io_ratio)?);
    }

    let pagination = build_pagination(total_count, page_num.into(), effective_page_size.into());
    tracing::info!(
        page = page_num,
        page_size = effective_page_size,
        returned_orders = summaries.len(),
        total_orders = total_count,
        orders_stage_duration_ms,
        quotes_stage_duration_ms,
        total_duration_ms = total_start.elapsed().as_millis(),
        "orders by token request processed"
    );
    Ok(OrdersListResponse {
        orders: summaries,
        pagination,
    })
}

#[utoipa::path(
    get,
    path = "/v1/orders/token/{address}",
    tag = "Orders",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Token address"),
        OrdersByTokenParams,
    ),
    responses(
        (status = 200, description = "Paginated list of orders for token", body = OrdersListResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 422, description = "Unprocessable entity", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/token/<address>?<params..>")]
pub async fn get_orders_by_token(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    orders_cache: &State<OrdersByTokenCache>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: OrdersByTokenParams,
) -> Result<Json<OrdersListResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        let addr = address.0;
        let side = params.side;
        let page = params.page.unwrap_or(1);
        let page_size = params
            .page_size
            .unwrap_or(DEFAULT_PAGE_SIZE as u16)
            .min(MAX_PAGE_SIZE);
        let cache_key = (addr, side, page, page_size);

        let response = orders_cache
            .get_or_try_insert(cache_key, || async {
                let raindex = shared_raindex.read().await;
                let ds = RaindexOrdersListDataSource {
                    client: raindex.client(),
                };
                process_get_orders_by_token(&ds, addr, side, Some(page), Some(page_size)).await
            })
            .await
            .map_err(ApiError::from)?;

        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::{
        mock_order, mock_order_with_shared_vaults, mock_quote,
    };
    use crate::routes::orders::test_fixtures::MockOrdersListDataSource;
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_success() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![mock_order()]),
            total_count: 1,
            quotes: Ok(vec![mock_quote("1.5")]),
        };
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();
        let result = process_get_orders_by_token(&ds, addr, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.orders.len(), 1);
        assert_eq!(result.orders[0].input_token.symbol, "USDC");
        assert_eq!(result.orders[0].output_token.symbol, "WETH");
        assert_eq!(result.orders[0].io_ratio, "1.5");
        assert_eq!(result.pagination.total_orders, 1);
        assert_eq!(result.pagination.page, 1);
        assert!(!result.pagination.has_more);
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_empty() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![]),
            total_count: 0,
            quotes: Ok(vec![]),
        };
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();
        let result = process_get_orders_by_token(&ds, addr, Some(OrderSide::Input), None, None)
            .await
            .unwrap();

        assert!(result.orders.is_empty());
        assert_eq!(result.pagination.total_orders, 0);
        assert_eq!(result.pagination.total_pages, 0);
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_quote_failure_shows_dash() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![mock_order()]),
            total_count: 1,
            quotes: Err(ApiError::Internal("quote error".into())),
        };
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();
        let result = process_get_orders_by_token(&ds, addr, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.orders[0].io_ratio, "-");
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_query_failure() {
        let ds = MockOrdersListDataSource {
            orders: Err(ApiError::Internal("failed".into())),
            total_count: 0,
            quotes: Ok(vec![]),
        };
        let addr: Address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
            .parse()
            .unwrap();
        let result = process_get_orders_by_token(&ds, addr, None, None, None).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_token_shared_vaults() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![mock_order_with_shared_vaults()]),
            total_count: 1,
            quotes: Ok(vec![mock_quote("200.0")]),
        };
        let addr: Address = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2"
            .parse()
            .unwrap();
        let result = process_get_orders_by_token(&ds, addr, None, None, None)
            .await
            .unwrap();

        assert_eq!(result.orders.len(), 1);
        assert_eq!(result.orders[0].input_token.symbol, "wtMSTR");
        assert_eq!(result.orders[0].output_token.symbol, "wtMSTR");
    }

    #[rocket::async_test]
    async fn test_pagination_math() {
        use super::super::build_pagination;

        let p = build_pagination(250, 1, 100);
        assert_eq!(p.total_orders, 250);
        assert_eq!(p.total_pages, 3);
        assert!(p.has_more);

        let p = build_pagination(250, 3, 100);
        assert_eq!(p.total_pages, 3);
        assert!(!p.has_more);

        let p = build_pagination(0, 1, 100);
        assert_eq!(p.total_pages, 0);
        assert!(!p.has_more);

        let p = build_pagination(100, 1, 100);
        assert_eq!(p.total_pages, 1);
        assert!(!p.has_more);

        let p = build_pagination(101, 1, 100);
        assert_eq!(p.total_pages, 2);
        assert!(p.has_more);
    }

    #[rocket::async_test]
    async fn test_get_orders_by_token_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/orders/token/0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_orders_by_token_invalid_address_returns_404() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/orders/token/not-an-address")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }
}
