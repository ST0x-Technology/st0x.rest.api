use crate::error::{ApiError, ApiErrorResponse};
use crate::types::common::{ValidatedAddress, ValidatedFixedBytes};
use crate::types::orders::{OrdersByTxResponse, OrdersListResponse, OrdersPaginationParams};
use rocket::serde::json::Json;
use rocket::Route;

#[utoipa::path(
    get,
    path = "/v1/orders/tx/{tx_hash}",
    tag = "Orders",
    params(
        ("tx_hash" = String, Path, description = "Transaction hash"),
    ),
    responses(
        (status = 200, description = "Orders from transaction", body = OrdersByTxResponse),
        (status = 202, description = "Transaction not yet indexed", body = ApiErrorResponse),
        (status = 404, description = "Transaction not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/tx/<tx_hash>")]
pub async fn get_orders_by_tx(
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<OrdersByTxResponse>, ApiError> {
    let _ = tx_hash;
    todo!()
}

#[utoipa::path(
    get,
    path = "/v1/orders/{address}",
    tag = "Orders",
    params(
        ("address" = String, Path, description = "Owner address"),
        OrdersPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of orders", body = OrdersListResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<address>?<params..>", rank = 2)]
pub async fn get_orders_by_address(
    address: ValidatedAddress,
    params: OrdersPaginationParams,
) -> Result<Json<OrdersListResponse>, ApiError> {
    let _ = (address, params);
    todo!()
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_orders_by_tx, get_orders_by_address]
}
