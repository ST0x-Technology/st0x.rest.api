use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::order::{DeployDcaOrderRequest, DeployOrderResponse};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    post,
    path = "/v1/order/dca",
    tag = "Order",
    security(("basicAuth" = [])),
    request_body = DeployDcaOrderRequest,
    responses(
        (status = 200, description = "DCA order deployment result", body = DeployOrderResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/dca", data = "<request>")]
pub async fn post_order_dca(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    span: TracingSpan,
    request: Json<DeployDcaOrderRequest>,
) -> Result<Json<DeployOrderResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let raindex = shared_raindex.read().await;
        raindex
            .run_with_client(move |_client| async move { todo!() })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}
