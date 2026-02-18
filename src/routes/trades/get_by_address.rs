use super::{RaindexTradesDataSource, TradesDataSource};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::{TokenRef, ValidatedAddress};
use crate::types::trades::{
    TradeByAddress, TradesByAddressResponse, TradesPagination, TradesPaginationParams,
};
use alloy::primitives::{Address, FixedBytes};
use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
use rocket::serde::json::Json;
use rocket::State;
use std::str::FromStr;
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
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        raindex
            .run_with_client(move |client| async move {
                let ds = RaindexTradesDataSource { client: &client };
                process_get_trades_by_address(&ds, address.0, params).await
            })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}

pub(super) async fn process_get_trades_by_address(
    ds: &dyn TradesDataSource,
    address: Address,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20);

    let pagination = PaginationParams::new(
        Some(
            page.try_into()
                .map_err(|_| ApiError::BadRequest("page value too large".into()))?,
        ),
        Some(
            page_size
                .try_into()
                .map_err(|_| ApiError::BadRequest("page_size value too large".into()))?,
        ),
    );
    let time_filter = TimeFilter::new(params.start_time, params.end_time);

    let result = ds
        .get_trades_for_owner(address, pagination, time_filter)
        .await?;

    let trades: Vec<TradeByAddress> = result
        .trades()
        .iter()
        .map(|trade| {
            let tx_hash = trade.transaction().id();
            let input_vc = trade.input_vault_balance_change();
            let output_vc = trade.output_vault_balance_change();

            let input_token_data = input_vc.token();
            let output_token_data = output_vc.token();

            let order_hash = FixedBytes::<32>::from_str(&trade.order_hash().to_string()).ok();

            let timestamp: u64 = trade.timestamp().try_into().map_err(|_| {
                tracing::error!("timestamp does not fit in u64");
                ApiError::Internal("timestamp overflow".into())
            })?;
            let block_number: u64 =
                trade.transaction().block_number().try_into().map_err(|_| {
                    tracing::error!("block number does not fit in u64");
                    ApiError::Internal("block number overflow".into())
                })?;

            Ok(TradeByAddress {
                tx_hash,
                input_amount: input_vc.formatted_amount(),
                output_amount: output_vc.formatted_amount(),
                input_token: TokenRef {
                    address: input_token_data.address(),
                    symbol: input_token_data.symbol().unwrap_or_default(),
                    decimals: input_token_data.decimals(),
                },
                output_token: TokenRef {
                    address: output_token_data.address(),
                    symbol: output_token_data.symbol().unwrap_or_default(),
                    decimals: output_token_data.decimals(),
                },
                order_hash,
                timestamp,
                block_number,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    let total_trades = result.total_count();
    let total_pages = if page_size > 0 {
        total_trades.div_ceil(u64::from(page_size))
    } else {
        0
    };
    let has_more = u64::from(page) < total_pages;

    Ok(Json(TradesByAddressResponse {
        trades,
        pagination: TradesPagination {
            page,
            page_size,
            total_trades,
            total_pages,
            has_more,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;
    use crate::routes::order::test_fixtures::*;
    use crate::test_helpers::{
        basic_auth_header, mock_invalid_raindex_config, seed_api_key, TestClientBuilder,
    };
    use alloy::primitives::{address, B256};
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::trades::{RaindexTrade, RaindexTradesListResult};
    use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
    use rocket::http::{Header, Status};

    struct MockTradesDataSource {
        owner_result: Result<RaindexTradesListResult, ApiError>,
    }

    #[async_trait(?Send)]
    impl TradesDataSource for MockTradesDataSource {
        async fn get_trades_by_tx(&self, _tx_hash: B256) -> Result<Vec<RaindexTrade>, ApiError> {
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
    }

    #[rocket::async_test]
    async fn test_process_success() {
        let trade = mock_trade();
        let ds = MockTradesDataSource {
            owner_result: Ok(RaindexTradesListResult::new(vec![trade], 1)),
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
            owner_result: Ok(RaindexTradesListResult::new(vec![], 0)),
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

    #[rocket::async_test]
    async fn test_500_on_bad_config() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/trades/0x0000000000000000000000000000000000000001")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
    }
}
