use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::TracingSpan;
use crate::types::common::{ValidatedAddress, ValidatedFixedBytes};
use crate::types::orders::{OrdersByTxResponse, OrdersListResponse, OrdersPaginationParams};
use rocket::serde::json::Json;
use rocket::Route;
use tracing::Instrument;

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
    span: TracingSpan,
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<OrdersByTxResponse>, ApiError> {
    async move {
        tracing::info!(tx_hash = ?tx_hash, "request received");
        todo!()
    }
    .instrument(span.0)
    .await
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
    span: TracingSpan,
    address: ValidatedAddress,
    params: OrdersPaginationParams,
) -> Result<Json<OrdersListResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        todo!()
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_orders_by_tx, get_orders_by_address]
}
