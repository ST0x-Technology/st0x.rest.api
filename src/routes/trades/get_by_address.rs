use super::{
    build_trades_list_response, trades_pagination_params, RaindexTradesDataSource, TradesDataSource,
};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedAddress;
use crate::types::trades::{TradesByAddressResponse, TradesPaginationParams};
use alloy::primitives::Address;
use rain_orderbook_common::raindex_client::types::PaginationParams;
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

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
        process_get_trades_by_address(&ds, address.0, params).await
    }
    .instrument(span.0)
    .await
}

pub(super) async fn process_get_trades_by_address(
    ds: &dyn TradesDataSource,
    owner: Address,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    let (page, page_size, sdk_page, sdk_page_size, time_filter) = trades_pagination_params(params)?;

    let result = ds
        .get_trades_for_owner(
            owner,
            PaginationParams {
                page: Some(sdk_page),
                page_size: Some(sdk_page_size),
            },
            time_filter,
        )
        .await?;

    build_trades_list_response(result, page, page_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;
    use crate::routes::order::test_fixtures::*;
    use crate::test_helpers::TestClientBuilder;
    use alloy::primitives::{address, B256};
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::trades::RaindexTradesListResult;
    use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
    use rocket::http::Status;

    struct MockTradesDataSource {
        owner_result: Result<RaindexTradesListResult, ApiError>,
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
            match &self.owner_result {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(e.clone()),
            }
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

        async fn get_trades_by_order_hashes(
            &self,
            _order_hashes: Vec<B256>,
            _time_filter: TimeFilter,
        ) -> Result<
            rain_orderbook_common::raindex_client::trades::RaindexTradesByOrderHashResult,
            ApiError,
        > {
            unimplemented!()
        }
    }

    #[rocket::async_test]
    async fn test_process_success() {
        let ds = MockTradesDataSource {
            owner_result: Ok(mock_trades_list_result()),
        };
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
        };
        let result = process_get_trades_by_address(
            &ds,
            address!("0000000000000000000000000000000000000001"),
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
            owner_result: Ok(mock_empty_trades_list_result()),
        };
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
        };
        let result = process_get_trades_by_address(
            &ds,
            address!("0000000000000000000000000000000000000001"),
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
            owner_result: Err(ApiError::Internal("subgraph error".into())),
        };
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
        };
        let result = process_get_trades_by_address(
            &ds,
            address!("0000000000000000000000000000000000000001"),
            params,
        )
        .await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/trades/0x0000000000000000000000000000000000000001")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }
}
