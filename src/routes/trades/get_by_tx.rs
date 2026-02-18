use super::{RaindexTradesTxDataSource, TradesTxDataSource};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::routes::order::OrderDataSource;
use crate::types::common::ValidatedFixedBytes;
use crate::types::trades::{
    TradeByTxEntry, TradeRequest, TradeResult, TradesByTxResponse, TradesTotals,
};
use alloy::primitives::{Address, B256};
use rain_math_float::Float;
use rocket::serde::json::Json;
use rocket::State;
use std::collections::HashMap;
use std::str::FromStr;
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
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<TradesByTxResponse>, ApiError> {
    async move {
        tracing::info!(tx_hash = ?tx_hash, "request received");
        raindex
            .run_with_client(move |client| async move {
                let trades_ds = RaindexTradesTxDataSource { client: &client };
                let order_ds = crate::routes::order::RaindexOrderDataSource { client: &client };
                process_get_trades_by_tx(&trades_ds, &order_ds, tx_hash.0).await
            })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}

pub(super) async fn process_get_trades_by_tx(
    trades_ds: &dyn TradesTxDataSource,
    order_ds: &dyn OrderDataSource,
    tx_hash: B256,
) -> Result<Json<TradesByTxResponse>, ApiError> {
    let trades = trades_ds.get_trades_by_tx(tx_hash).await?;

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

    let unique_hashes: Vec<B256> = {
        let mut seen = std::collections::HashSet::new();
        trades
            .iter()
            .map(|t| {
                B256::from_str(&t.order_hash().to_string()).map_err(|e| {
                    tracing::error!(error = %e, "failed to parse order hash");
                    ApiError::Internal("failed to parse order hash".into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|h| seen.insert(*h))
            .collect()
    };

    let order_results: Vec<Result<_, ApiError>> =
        futures::future::join_all(unique_hashes.iter().map(|hash| {
            let hash = *hash;
            async move {
                let orders = order_ds.get_orders_by_hash(hash).await?;
                Ok((hash, orders.into_iter().next()))
            }
        }))
        .await;

    let order_cache: HashMap<
        String,
        Option<rain_orderbook_common::raindex_client::orders::RaindexOrder>,
    > = order_results
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(hash, order)| (hash.to_string(), order))
        .collect();

    let mut trade_entries = Vec::new();
    let mut total_input = Float::parse("0".to_string()).map_err(|e| {
        tracing::error!(error = %e, "float parse error");
        ApiError::Internal("float parse error".into())
    })?;
    let mut total_output = Float::parse("0".to_string()).map_err(|e| {
        tracing::error!(error = %e, "float parse error");
        ApiError::Internal("float parse error".into())
    })?;

    for trade in &trades {
        let order_hash_str = trade.order_hash().to_string();
        let order_owner = order_cache
            .get(&order_hash_str)
            .and_then(|o| o.as_ref())
            .map(|o| o.owner())
            .ok_or_else(|| {
                tracing::error!(order_hash = %order_hash_str, "order not found for trade");
                ApiError::Internal("order not found for trade".into())
            })?;

        let input_vc = trade.input_vault_balance_change();
        let output_vc = trade.output_vault_balance_change();

        total_input = (total_input + input_vc.amount()).map_err(|e| {
            tracing::error!(error = %e, "float add error");
            ApiError::Internal("float arithmetic error".into())
        })?;
        total_output = (total_output + output_vc.amount()).map_err(|e| {
            tracing::error!(error = %e, "float add error");
            ApiError::Internal("float arithmetic error".into())
        })?;

        let zero = Float::parse("0".to_string()).map_err(|e| {
            tracing::error!(error = %e, "float parse error");
            ApiError::Internal("float parse error".into())
        })?;
        let abs_output = (zero - output_vc.amount()).map_err(|e| {
            tracing::error!(error = %e, "float sub error");
            ApiError::Internal("float arithmetic error".into())
        })?;
        let actual_io_ratio = (input_vc.amount() / abs_output)
            .map_err(|e| {
                tracing::error!(error = %e, "float div error");
                ApiError::Internal("float arithmetic error".into())
            })?
            .format()
            .map_err(|e| {
                tracing::error!(error = %e, "float format error");
                ApiError::Internal("float format error".into())
            })?;

        let order_hash_fixed = B256::from_str(&order_hash_str).map_err(|e| {
            tracing::error!(error = %e, order_hash = %order_hash_str, "failed to parse order hash");
            ApiError::Internal("failed to parse order hash".into())
        })?;

        trade_entries.push(TradeByTxEntry {
            order_hash: order_hash_fixed,
            order_owner,
            request: TradeRequest {
                input_token: input_vc.token().address(),
                output_token: output_vc.token().address(),
                maximum_input: input_vc.formatted_amount(),
                maximum_io_ratio: actual_io_ratio.clone(),
            },
            result: TradeResult {
                input_amount: input_vc.formatted_amount(),
                output_amount: output_vc.formatted_amount(),
                actual_io_ratio,
            },
        });
    }

    let zero = Float::parse("0".to_string()).map_err(|e| {
        tracing::error!(error = %e, "float parse error");
        ApiError::Internal("float parse error".into())
    })?;
    let abs_total_output = (zero - total_output).map_err(|e| {
        tracing::error!(error = %e, "float sub error");
        ApiError::Internal("float arithmetic error".into())
    })?;
    let average_io_ratio = (total_input / abs_total_output)
        .map_err(|e| {
            tracing::error!(error = %e, "float div error");
            ApiError::Internal("float arithmetic error".into())
        })?
        .format()
        .map_err(|e| {
            tracing::error!(error = %e, "float format error");
            ApiError::Internal("float format error".into())
        })?;
    let total_input_amount = total_input.format().map_err(|e| {
        tracing::error!(error = %e, "float format error");
        ApiError::Internal("float format error".into())
    })?;
    let total_output_amount = total_output.format().map_err(|e| {
        tracing::error!(error = %e, "float format error");
        ApiError::Internal("float format error".into())
    })?;

    Ok(Json(TradesByTxResponse {
        tx_hash,
        block_number,
        timestamp,
        sender,
        trades: trade_entries,
        totals: TradesTotals {
            total_input_amount,
            total_output_amount,
            average_io_ratio,
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
    use alloy::primitives::{address, Bytes};
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::trades::RaindexTrade;
    use rocket::http::{Header, Status};

    struct MockTradesTxDataSource {
        result: Result<Vec<RaindexTrade>, ApiError>,
    }

    #[async_trait(?Send)]
    impl TradesTxDataSource for MockTradesTxDataSource {
        async fn get_trades_by_tx(&self, _tx_hash: B256) -> Result<Vec<RaindexTrade>, ApiError> {
            match &self.result {
                Ok(trades) => Ok(trades.clone()),
                Err(e) => Err(e.clone()),
            }
        }
    }

    #[rocket::async_test]
    async fn test_process_success() {
        let trades_ds = MockTradesTxDataSource {
            result: Ok(vec![mock_trade()]),
        };
        let order_ds = MockOrderDataSource {
            orders: Ok(vec![mock_order()]),
            trades: vec![],
            quotes: vec![],
            calldata: Ok(Bytes::new()),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            &order_ds,
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
        let trades_ds = MockTradesTxDataSource { result: Ok(vec![]) };
        let order_ds = MockOrderDataSource {
            orders: Ok(vec![]),
            trades: vec![],
            quotes: vec![],
            calldata: Ok(Bytes::new()),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            &order_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
        )
        .await;
        assert!(matches!(result, Err(ApiError::NotFound(_))));
    }

    #[rocket::async_test]
    async fn test_process_tx_not_indexed() {
        let trades_ds = MockTradesTxDataSource {
            result: Err(ApiError::NotYetIndexed("not indexed".into())),
        };
        let order_ds = MockOrderDataSource {
            orders: Ok(vec![]),
            trades: vec![],
            quotes: vec![],
            calldata: Ok(Bytes::new()),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            &order_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
        )
        .await;
        assert!(matches!(result, Err(ApiError::NotYetIndexed(_))));
    }

    #[rocket::async_test]
    async fn test_process_query_failure() {
        let trades_ds = MockTradesTxDataSource {
            result: Err(ApiError::Internal("subgraph error".into())),
        };
        let order_ds = MockOrderDataSource {
            orders: Ok(vec![]),
            trades: vec![],
            quotes: vec![],
            calldata: Ok(Bytes::new()),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            &order_ds,
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
