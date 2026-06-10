use super::{
    build_trades_list_response, trades_pagination_params, RaindexTradesDataSource, TradesDataSource,
};
use crate::app_state::ApplicationState;
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
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
    path = "/v1/trades/taker/{address}",
    tag = "Trades",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Taker address"),
        TradesPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of trades for taker", body = TradesByAddressResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 422, description = "Unprocessable entity", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[allow(clippy::too_many_arguments)]
#[get("/taker/<address>?<params..>")]
pub async fn get_trades_by_taker(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        let addr = address.0;
        if !app_state.response_caches.is_enabled() {
            let client = {
                let raindex = shared_raindex.read().await;
                raindex.client().clone()
            };
            let ds = RaindexTradesDataSource {
                client: &client,
                pool: pool.inner(),
            };
            return process_get_trades_by_taker(&ds, addr, params).await;
        }

        let cache_key = super::get_by_token::trades_cache_key("trades/taker", addr, &params);
        let response = app_state
            .response_caches
            .trades_by_taker
            .get_or_try_insert(cache_key, || async move {
                let client = {
                    let raindex = shared_raindex.read().await;
                    raindex.client().clone()
                };
                let ds = RaindexTradesDataSource {
                    client: &client,
                    pool: pool.inner(),
                };
                process_get_trades_by_taker(&ds, addr, params)
                    .await
                    .map(Json::into_inner)
            })
            .await
            .map_err(|e| (*e).clone())?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

pub(super) async fn process_get_trades_by_taker(
    ds: &dyn TradesDataSource,
    taker: Address,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    let denomination = params.denomination.unwrap_or_default();
    let (page, page_size, sdk_page, sdk_page_size, time_filter) = trades_pagination_params(params)?;

    tracing::info!(taker = ?taker, page, page_size, "querying trades by taker");
    let result = ds
        .get_trades_for_taker(taker, sdk_page, sdk_page_size, time_filter)
        .await?;

    build_trades_list_response(ds, result, page, page_size, denomination).await
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
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    struct CapturedTakerQuery {
        taker: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    }

    struct MockTradesDataSource {
        taker_result: Result<RaindexTradesListResult, ApiError>,
        captured: Arc<Mutex<Option<CapturedTakerQuery>>>,
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
            taker: Address,
            page: u16,
            page_size: u16,
            time_filter: TimeFilter,
        ) -> Result<RaindexTradesListResult, ApiError> {
            *self.captured.lock().unwrap() = Some(CapturedTakerQuery {
                taker,
                page,
                page_size,
                time_filter,
            });
            match &self.taker_result {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(e.clone()),
            }
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
        let captured = Arc::new(Mutex::new(None));
        let ds = MockTradesDataSource {
            taker_result: Ok(mock_trades_list_result()),
            captured: Arc::clone(&captured),
        };
        let taker = address!("cccccccccccccccccccccccccccccccccccccccc");
        let params = TradesPaginationParams {
            page: Some(2),
            page_size: Some(10),
            start_time: Some(1700000000),
            end_time: Some(1700002000),
            denomination: None,
        };
        let result = process_get_trades_by_taker(&ds, taker, params)
            .await
            .unwrap();

        let response = result.into_inner();
        assert_eq!(response.trades.len(), 1);
        assert_eq!(response.pagination.page, 2);
        assert_eq!(response.pagination.page_size, 10);
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

        let captured = captured.lock().unwrap().clone().unwrap();
        assert_eq!(captured.taker, taker);
        assert_eq!(captured.page, 2);
        assert_eq!(captured.page_size, 10);
        assert_eq!(captured.time_filter.start, Some(1700000000));
        assert_eq!(captured.time_filter.end, Some(1700002000));
    }

    #[rocket::async_test]
    async fn test_process_no_trades() {
        let ds = MockTradesDataSource {
            taker_result: Ok(mock_empty_trades_list_result()),
            captured: Arc::new(Mutex::new(None)),
        };
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
            denomination: None,
        };
        let result = process_get_trades_by_taker(
            &ds,
            address!("cccccccccccccccccccccccccccccccccccccccc"),
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
            taker_result: Err(ApiError::Internal("subgraph error".into())),
            captured: Arc::new(Mutex::new(None)),
        };
        let params = TradesPaginationParams {
            page: Some(1),
            page_size: Some(20),
            start_time: None,
            end_time: None,
            denomination: None,
        };
        let result = process_get_trades_by_taker(
            &ds,
            address!("cccccccccccccccccccccccccccccccccccccccc"),
            params,
        )
        .await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/trades/taker/0xcccccccccccccccccccccccccccccccccccccccc")
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
            .get("/v1/trades/taker/not-an-address")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }

    #[test]
    fn test_route_is_registered() {
        let routes = crate::routes::trades::routes();
        assert!(routes
            .iter()
            .any(|route| route.uri.path() == "/taker/<address>"));
    }
}
