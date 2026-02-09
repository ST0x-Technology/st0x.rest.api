use crate::error::{ApiError, ApiErrorResponse};
use crate::types::common::{ValidatedAddress, ValidatedFixedBytes};
use crate::types::trades::{TradesByAddressResponse, TradesByTxResponse, TradesPaginationParams};
use rocket::serde::json::Json;
use rocket::Route;

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
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<TradesByTxResponse>, ApiError> {
    let _ = tx_hash;
    todo!()
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
    address: ValidatedAddress,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    let _ = (address, params);
    todo!()
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_trades_by_tx, get_trades_by_address]
}
