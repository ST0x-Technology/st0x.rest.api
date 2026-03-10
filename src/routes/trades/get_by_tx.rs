use super::{RaindexTradesDataSource, TradesDataSource};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedFixedBytes;
use crate::types::trades::{
    TradeByTxEntry, TradeRequest, TradeResult, TradesByTxResponse, TradesTotals,
};
use alloy::primitives::{Address, B256};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

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
    span: TracingSpan,
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<TradesByTxResponse>, ApiError> {
    async move {
        tracing::info!(tx_hash = ?tx_hash, "request received");
        let raindex = shared_raindex.read().await;
        raindex
            .run_with_client(move |client| async move {
                let trades_ds = RaindexTradesDataSource { client: &client };
                process_get_trades_by_tx(&trades_ds, tx_hash.0).await
            })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}

pub(super) async fn process_get_trades_by_tx(
    trades_ds: &dyn TradesDataSource,
    tx_hash: B256,
) -> Result<Json<TradesByTxResponse>, ApiError> {
    let result = trades_ds.get_trades_by_tx(tx_hash).await?;
    let trades = result.trades();

    if trades.is_empty() {
        return Err(ApiError::NotFound(
            "transaction has no associated trades".into(),
        ));
    }

    let first_tx = trades[0].transaction();
    let block_number: u64 = first_tx.block_number().try_into().map_err(|_| {
        tracing::error!("block number does not fit in u64");
        ApiError::Internal("block number overflow".into())
    })?;
    let timestamp: u64 = first_tx.timestamp().try_into().map_err(|_| {
        tracing::error!("timestamp does not fit in u64");
        ApiError::Internal("timestamp overflow".into())
    })?;
    let sender: Address = first_tx.from();

    let trade_entries: Vec<TradeByTxEntry> = trades
        .iter()
        .map(|trade| {
            let input_vc = trade.input_vault_balance_change();
            let output_vc = trade.output_vault_balance_change();
            let io_ratio = trade.formatted_io_ratio().to_string();
            let input_amount = input_vc.formatted_amount();

            TradeByTxEntry {
                order_hash: trade.order_hash(),
                order_owner: trade.owner(),
                request: TradeRequest {
                    input_token: input_vc.token().address(),
                    output_token: output_vc.token().address(),
                    maximum_input: input_amount.clone(),
                    maximum_io_ratio: io_ratio.clone(),
                },
                result: TradeResult {
                    input_amount,
                    output_amount: output_vc.formatted_amount(),
                    actual_io_ratio: io_ratio,
                },
            }
        })
        .collect();

    let summary = result.summary().and_then(|s| s.first()).ok_or_else(|| {
        tracing::error!("no pair summary in trades result");
        ApiError::Internal("missing pair summary".into())
    })?;

    Ok(Json(TradesByTxResponse {
        tx_hash,
        block_number,
        timestamp,
        sender,
        trades: trade_entries,
        totals: TradesTotals {
            total_input_amount: summary.formatted_total_input().to_string(),
            total_output_amount: summary.formatted_total_output().to_string(),
            average_io_ratio: summary.formatted_average_io_ratio().to_string(),
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
    use alloy::primitives::address;
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::trades::RaindexTradesListResult;
    use rocket::http::{Header, Status};

    struct MockTradesDataSource {
        result: Result<RaindexTradesListResult, ApiError>,
    }

    #[async_trait(?Send)]
    impl TradesDataSource for MockTradesDataSource {
        async fn get_trades_by_tx(
            &self,
            _tx_hash: B256,
        ) -> Result<RaindexTradesListResult, ApiError> {
            match &self.result {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(e.clone()),
            }
        }
    }

    #[rocket::async_test]
    async fn test_process_success() {
        let trades_ds = MockTradesDataSource {
            result: Ok(mock_trades_list_result()),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000088"
                .parse()
                .unwrap(),
        )
        .await
        .unwrap();

        let response = result.into_inner();
        assert_eq!(response.trades.len(), 1);
        assert_eq!(
            response.sender,
            address!("0000000000000000000000000000000000000002")
        );
        assert_eq!(response.block_number, 100);
        assert_eq!(response.timestamp, 1700001000);
        assert_eq!(
            response.trades[0].order_owner,
            address!("0000000000000000000000000000000000000001")
        );
    }

    #[rocket::async_test]
    async fn test_process_tx_not_found() {
        let trades_ds = MockTradesDataSource {
            result: Ok(mock_empty_trades_list_result()),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
        )
        .await;
        assert!(matches!(result, Err(ApiError::NotFound(_))));
    }

    #[rocket::async_test]
    async fn test_process_tx_not_indexed() {
        let trades_ds = MockTradesDataSource {
            result: Err(ApiError::NotYetIndexed("not indexed".into())),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
        )
        .await;
        assert!(matches!(result, Err(ApiError::NotYetIndexed(_))));
    }

    #[rocket::async_test]
    async fn test_process_query_failure() {
        let trades_ds = MockTradesDataSource {
            result: Err(ApiError::Internal("subgraph error".into())),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
        )
        .await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
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
    async fn test_get_trades_by_tx_500_on_bad_raindex_config() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/trades/tx/0x0000000000000000000000000000000000000000000000000000000000000088")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
    }
}
