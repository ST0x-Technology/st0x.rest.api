use super::{
    current_wrap_ratios_for_trades, trade_block_number, wrap_ratio_map_for_trade,
    RaindexTradesDataSource, TradesDataSource,
};
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::{Denomination, ValidatedFixedBytes};
use crate::types::trades::{
    TradeByTxEntry, TradeRequest, TradeResult, TradesByTxParams, TradesByTxResponse, TradesTotals,
};
use alloy::primitives::{Address, B256};
use rain_math_float::Float;
use rocket::serde::json::Json;
use rocket::State;
use std::ops::{Add, Div, Sub};
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/v1/trades/tx/{tx_hash}",
    tag = "Trades",
    security(("basicAuth" = [])),
    params(
        ("tx_hash" = String, Path, description = "Transaction hash"),
        TradesByTxParams,
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
#[get("/tx/<tx_hash>?<params..>")]
pub async fn get_trades_by_tx(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    pool: &State<DbPool>,
    span: TracingSpan,
    tx_hash: ValidatedFixedBytes,
    params: TradesByTxParams,
) -> Result<Json<TradesByTxResponse>, ApiError> {
    async move {
        tracing::info!(tx_hash = ?tx_hash, params = ?params, "request received");
        let raindex = shared_raindex.read().await;
        let trades_ds = RaindexTradesDataSource {
            client: raindex.client(),
            pool: pool.inner(),
        };
        process_get_trades_by_tx(
            &trades_ds,
            tx_hash.0,
            params.denomination.unwrap_or_default(),
        )
        .await
    }
    .instrument(span.0)
    .await
}

pub(super) async fn process_get_trades_by_tx(
    trades_ds: &dyn TradesDataSource,
    tx_hash: B256,
    denomination: Denomination,
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
    let trade_wrap_ratios = current_wrap_ratios_for_trades(trades_ds, denomination, trades).await?;

    let trade_entries: Vec<TradeByTxEntry> = trades
        .iter()
        .map(|trade| {
            let input_vc = trade.input_vault_balance_change();
            let output_vc = trade.output_vault_balance_change();
            let input_token = input_vc.token().address();
            let output_token = output_vc.token().address();
            let block_number = trade_block_number(trade)?;
            let wrap_ratios = if denomination == Denomination::Unwrapped {
                wrap_ratio_map_for_trade(
                    input_token,
                    output_token,
                    block_number,
                    &trade_wrap_ratios,
                )
            } else {
                Default::default()
            };
            let io_ratio = if denomination == Denomination::Unwrapped {
                crate::denomination::convert_wrapped_io_ratio(
                    trade.formatted_io_ratio().to_string(),
                    input_token,
                    output_token,
                    &wrap_ratios,
                )?
            } else {
                trade.formatted_io_ratio().to_string()
            };
            let input_amount = if denomination == Denomination::Unwrapped {
                crate::denomination::convert_wrapped_amount_for_token(
                    input_vc.formatted_amount(),
                    input_token,
                    &wrap_ratios,
                )?
            } else {
                input_vc.formatted_amount()
            };
            let output_amount = if denomination == Denomination::Unwrapped {
                crate::denomination::convert_wrapped_amount_for_token(
                    output_vc.formatted_amount(),
                    output_token,
                    &wrap_ratios,
                )?
            } else {
                output_vc.formatted_amount()
            };

            Ok(TradeByTxEntry {
                order_hash: trade.order_hash(),
                order_owner: trade.owner(),
                request: TradeRequest {
                    input_token,
                    output_token,
                    maximum_input: input_amount.clone(),
                    maximum_io_ratio: io_ratio.clone(),
                },
                result: TradeResult {
                    input_amount,
                    output_amount,
                    actual_io_ratio: io_ratio,
                },
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    let summary = result.summary().and_then(|s| s.first()).ok_or_else(|| {
        tracing::error!("no pair summary in trades result");
        ApiError::Internal("missing pair summary".into())
    })?;

    let totals = if denomination == Denomination::Unwrapped {
        totals_from_trade_entries(&trade_entries)?
    } else {
        TradesTotals {
            total_input_amount: summary.formatted_total_input().to_string(),
            total_output_amount: summary.formatted_total_output().to_string(),
            average_io_ratio: summary.formatted_average_io_ratio().to_string(),
        }
    };

    Ok(Json(TradesByTxResponse {
        tx_hash,
        block_number,
        timestamp,
        sender,
        trades: trade_entries,
        totals,
    }))
}

fn totals_from_trade_entries(trades: &[TradeByTxEntry]) -> Result<TradesTotals, ApiError> {
    let mut total_input = Float::zero().map_err(|error| {
        tracing::error!(error = %error, "failed to create zero float");
        ApiError::Internal("failed to calculate trade totals".into())
    })?;
    let mut total_output = Float::zero().map_err(|error| {
        tracing::error!(error = %error, "failed to create zero float");
        ApiError::Internal("failed to calculate trade totals".into())
    })?;
    let zero = Float::zero().map_err(|error| {
        tracing::error!(error = %error, "failed to create zero float");
        ApiError::Internal("failed to calculate trade totals".into())
    })?;

    for trade in trades {
        let input =
            crate::denomination::parse_decimal_float(trade.result.input_amount.clone(), "input")?;
        let output =
            crate::denomination::parse_decimal_float(trade.result.output_amount.clone(), "output")?;
        let normalized_output = zero.sub(output).map_err(|error| {
            tracing::error!(error = %error, "failed to normalize output amount");
            ApiError::Internal("failed to calculate trade totals".into())
        })?;
        total_input = total_input.add(input).map_err(|error| {
            tracing::error!(error = %error, "failed to calculate total input");
            ApiError::Internal("failed to calculate trade totals".into())
        })?;
        total_output = total_output.add(normalized_output).map_err(|error| {
            tracing::error!(error = %error, "failed to calculate total output");
            ApiError::Internal("failed to calculate trade totals".into())
        })?;
    }

    let average_io_ratio = if total_output
        .eq(Float::zero().map_err(|error| {
            tracing::error!(error = %error, "failed to create zero float");
            ApiError::Internal("failed to calculate trade totals".into())
        })?)
        .map_err(|error| {
            tracing::error!(error = %error, "failed to compare total output");
            ApiError::Internal("failed to calculate trade totals".into())
        })? {
        Float::zero().map_err(|error| {
            tracing::error!(error = %error, "failed to create zero float");
            ApiError::Internal("failed to calculate trade totals".into())
        })?
    } else {
        total_input.div(total_output).map_err(|error| {
            tracing::error!(error = %error, "failed to calculate average IO ratio");
            ApiError::Internal("failed to calculate trade totals".into())
        })?
    };

    Ok(TradesTotals {
        total_input_amount: crate::denomination::format_decimal_float(total_input, "total_input")?,
        total_output_amount: crate::denomination::format_decimal_float(
            total_output,
            "total_output",
        )?,
        average_io_ratio: crate::denomination::format_decimal_float(
            average_io_ratio,
            "average_io_ratio",
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;
    use crate::routes::order::test_fixtures::*;
    use crate::test_helpers::TestClientBuilder;
    use crate::wrap_ratio::WrapRatioValue;
    use alloy::primitives::address;
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::trades::RaindexTradesListResult;
    use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
    use rocket::http::Status;
    use std::collections::HashMap;

    struct MockTradesDataSource {
        result: Result<RaindexTradesListResult, ApiError>,
        current_wrap_ratios: HashMap<Address, WrapRatioValue>,
    }

    #[async_trait]
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

        async fn get_current_wrap_ratios_for_tokens(
            &self,
            _token_addresses: &[Address],
        ) -> Result<HashMap<Address, WrapRatioValue>, ApiError> {
            Ok(self.current_wrap_ratios.clone())
        }
    }

    #[rocket::async_test]
    async fn test_process_success() {
        let trades_ds = MockTradesDataSource {
            result: Ok(mock_trades_list_result()),
            current_wrap_ratios: Default::default(),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000088"
                .parse()
                .unwrap(),
            Denomination::Wrapped,
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
    async fn test_process_converts_unwrapped_amounts_and_totals() {
        let wrapped_output = address!("4200000000000000000000000000000000000006");
        let trades_ds = MockTradesDataSource {
            result: Ok(mock_trades_list_result()),
            current_wrap_ratios: HashMap::from([(
                wrapped_output,
                WrapRatioValue {
                    share_address: wrapped_output,
                    assets_per_share: "2".to_string(),
                },
            )]),
        };

        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000088"
                .parse()
                .unwrap(),
            Denomination::Unwrapped,
        )
        .await
        .unwrap();

        let response = result.into_inner();
        assert_eq!(response.trades[0].request.maximum_input, "0.500000");
        assert_eq!(response.trades[0].request.maximum_io_ratio, "1");
        assert_eq!(response.trades[0].result.input_amount, "0.500000");
        assert_eq!(response.trades[0].result.output_amount, "-0.5");
        assert_eq!(response.trades[0].result.actual_io_ratio, "1");
        assert_eq!(response.totals.total_input_amount, "0.5");
        assert_eq!(response.totals.total_output_amount, "0.5");
        assert_eq!(response.totals.average_io_ratio, "1");
    }

    #[rocket::async_test]
    async fn test_process_tx_not_found() {
        let trades_ds = MockTradesDataSource {
            result: Ok(mock_empty_trades_list_result()),
            current_wrap_ratios: Default::default(),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
            Denomination::Wrapped,
        )
        .await;
        assert!(matches!(result, Err(ApiError::NotFound(_))));
    }

    #[rocket::async_test]
    async fn test_process_tx_not_indexed() {
        let trades_ds = MockTradesDataSource {
            result: Err(ApiError::NotYetIndexed("not indexed".into())),
            current_wrap_ratios: Default::default(),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
            Denomination::Wrapped,
        )
        .await;
        assert!(matches!(result, Err(ApiError::NotYetIndexed(_))));
    }

    #[rocket::async_test]
    async fn test_process_query_failure() {
        let trades_ds = MockTradesDataSource {
            result: Err(ApiError::Internal("subgraph error".into())),
            current_wrap_ratios: Default::default(),
        };
        let result = process_get_trades_by_tx(
            &trades_ds,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap(),
            Denomination::Wrapped,
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
}
