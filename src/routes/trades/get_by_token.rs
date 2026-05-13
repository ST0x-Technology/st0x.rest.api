use super::{
    build_trades_list_response, trades_pagination_params, RaindexTradesDataSource, TradesDataSource,
};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedAddress;
use crate::types::trades::{TradesByAddressResponse, TradesPaginationParams};
use alloy::primitives::Address;
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/v1/trades/token/{address}",
    tag = "Trades",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Token address"),
        TradesPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of trades for token", body = TradesByAddressResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 422, description = "Unprocessable entity", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/token/<address>?<params..>")]
pub async fn get_trades_by_token(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
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
        process_get_trades_by_token(&ds, address.0, params).await
    }
    .instrument(span.0)
    .await
}

pub(super) async fn process_get_trades_by_token(
    ds: &dyn TradesDataSource,
    token: Address,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    let (page, page_size, sdk_page, sdk_page_size, time_filter) = trades_pagination_params(params)?;

    tracing::info!(token = ?token, page, page_size, "querying trades by token");
    let result = ds
        .get_trades_for_token(token, sdk_page, sdk_page_size, time_filter)
        .await?;

    build_trades_list_response(result, page, page_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;
    use crate::routes::order::test_fixtures::{
        mock_empty_trades_list_result, mock_trades_list_result,
    };
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use alloy::primitives::{address, B256};
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::trades::RaindexTradesListResult;
    use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
    use rocket::http::{Header, Status};

    struct MockTradesDataSource {
        token_result: Result<RaindexTradesListResult, ApiError>,
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
            match &self.token_result {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(e.clone()),
            }
        }
    }

    #[rocket::async_test]
    async fn test_process_success() {
        let ds = MockTradesDataSource {
            token_result: Ok(mock_trades_list_result()),
        };
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
        };
        let result = process_get_trades_by_token(
            &ds,
            address!("833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
            params,
        )
        .await
        .unwrap();

        let response = result.into_inner();
        assert_eq!(response.trades.len(), 1);
        assert_eq!(response.pagination.total_trades, 1);
        assert_eq!(response.pagination.total_pages, 1);
        assert!(!response.pagination.has_more);

        let t = &response.trades[0];
        assert_eq!(t.timestamp, 1700001000);
        assert_eq!(t.block_number, 100);
        assert_eq!(t.input_amount, "0.500000");
        assert_eq!(t.output_amount, "-0.250000000000000000");
        assert_eq!(t.input_token.symbol, "USDC");
        assert_eq!(t.output_token.symbol, "WETH");
    }

    #[rocket::async_test]
    async fn test_process_no_trades() {
        let ds = MockTradesDataSource {
            token_result: Ok(mock_empty_trades_list_result()),
        };
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
        };
        let result = process_get_trades_by_token(
            &ds,
            address!("833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
            params,
        )
        .await
        .unwrap();

        let response = result.into_inner();
        assert!(response.trades.is_empty());
        assert_eq!(response.pagination.total_trades, 0);
        assert_eq!(response.pagination.total_pages, 0);
        assert!(!response.pagination.has_more);
    }

    #[rocket::async_test]
    async fn test_process_query_failure() {
        let ds = MockTradesDataSource {
            token_result: Err(ApiError::Internal("subgraph error".into())),
        };
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
        };
        let result = process_get_trades_by_token(
            &ds,
            address!("833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
            params,
        )
        .await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/trades/token/0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_invalid_address_returns_422() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/trades/token/not-an-address")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }
}
