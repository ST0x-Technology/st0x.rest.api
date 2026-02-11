use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::TracingSpan;
use crate::types::common::{ValidatedAddress, ValidatedFixedBytes};
use crate::types::trades::{TradesByAddressResponse, TradesByTxResponse, TradesPaginationParams};
use rocket::serde::json::Json;
use rocket::Route;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/v1/trades/tx/{tx_hash}",
    tag = "Trades",
    params(
        ("tx_hash" = String, Path, description = "Transaction hash"),
    ),
    responses(
        (status = 200, description = "Trades from transaction", body = TradesByTxResponse),
        (status = 202, description = "Transaction not yet indexed", body = ApiErrorResponse),
        (status = 404, description = "Transaction not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/tx/<tx_hash>")]
pub async fn get_trades_by_tx(
    span: TracingSpan,
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<TradesByTxResponse>, ApiError> {
    async move {
        tracing::info!(tx_hash = ?tx_hash, "request received");
        todo!()
    }.instrument(span.0).await
}

#[utoipa::path(
    get,
    path = "/v1/trades/{address}",
    tag = "Trades",
    params(
        ("address" = String, Path, description = "Owner address"),
        TradesPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of trades", body = TradesByAddressResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<address>?<params..>", rank = 2)]
pub async fn get_trades_by_address(
    span: TracingSpan,
    address: ValidatedAddress,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        todo!()
    }.instrument(span.0).await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_trades_by_tx, get_trades_by_address]
}
