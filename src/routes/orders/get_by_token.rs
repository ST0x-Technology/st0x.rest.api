use super::{
    build_order_summary, build_pagination, OrdersListDataSource, RaindexOrdersListDataSource,
    DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedAddress;
use crate::types::orders::{OrderSide, OrdersByTokenParams, OrdersListResponse};
use alloy::primitives::Address;
use futures::future::join_all;
use rain_orderbook_common::raindex_client::orders::GetOrdersFilters;
use rain_orderbook_common::raindex_client::orders::GetOrdersTokenFilter;
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

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

    let page_num = page.unwrap_or(1);
    let effective_page_size = page_size
        .unwrap_or(DEFAULT_PAGE_SIZE as u16)
        .min(MAX_PAGE_SIZE);
    let (orders, total_count) = ds
        .get_orders_list(filters, Some(page_num), Some(effective_page_size))
        .await?;

    let quote_futures: Vec<_> = orders.iter().map(|o| ds.get_order_quotes(o)).collect();
    let quote_results = join_all(quote_futures).await;

    let mut summaries = Vec::with_capacity(orders.len());
    for (order, quotes_result) in orders.iter().zip(quote_results) {
        let io_ratio = match quotes_result {
            Ok(quotes) => quotes
                .first()
                .and_then(|q| q.data.as_ref())
                .map(|d| d.formatted_ratio.clone())
                .unwrap_or_else(|| "-".into()),
            Err(err) => {
                tracing::warn!(
                    order_hash = ?order.order_hash(),
                    error = ?err,
                    "quote fetch failed; using fallback io_ratio"
                );
                "-".into()
            }
        };
        summaries.push(build_order_summary(order, &io_ratio)?);
    }

    let pagination = build_pagination(total_count, page_num.into(), effective_page_size.into());
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
    span: TracingSpan,
    address: ValidatedAddress,
    params: OrdersByTokenParams,
) -> Result<Json<OrdersListResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        let addr = address.0;
        let side = params.side;
        let page = params.page;
        let page_size = params.page_size;
        let raindex = shared_raindex.read().await;
        let response = raindex
            .run_with_client(move |client| async move {
                let ds = RaindexOrdersListDataSource { client: &client };
                process_get_orders_by_token(&ds, addr, side, page, page_size).await
            })
            .await
            .map_err(ApiError::from)??;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::{mock_order, mock_quote};
    use crate::routes::orders::test_fixtures::MockOrdersListDataSource;
    use crate::test_helpers::{
        basic_auth_header, mock_invalid_raindex_config, seed_api_key, TestClientBuilder,
    };
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
    async fn test_get_orders_by_token_500_when_client_init_fails() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/orders/token/0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
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
