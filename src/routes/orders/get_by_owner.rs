use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedAddress;
use crate::types::orders::{OrdersListResponse, OrdersPaginationParams};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/v1/orders/owner/{address}",
    tag = "Orders",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Owner address"),
        OrdersPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of orders", body = OrdersListResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/owner/<address>?<params..>")]
pub async fn get_orders_by_address(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: OrdersPaginationParams,
) -> Result<Json<OrdersListResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        let _raindex = shared_raindex.read().await;
        todo!()
    }
    .instrument(span.0)
    .await
}
