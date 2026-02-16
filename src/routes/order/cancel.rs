use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::order::{CancelOrderRequest, CancelOrderResponse};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    post,
    path = "/v1/order/cancel",
    tag = "Order",
    security(("basicAuth" = [])),
    request_body = CancelOrderRequest,
    responses(
        (status = 200, description = "Cancel order result", body = CancelOrderResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 404, description = "Order not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/cancel", data = "<request>")]
pub async fn post_order_cancel(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    request: Json<CancelOrderRequest>,
) -> Result<Json<CancelOrderResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        raindex
            .run_with_client(move |_client| async move { todo!() })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}
