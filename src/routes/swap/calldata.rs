use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::swap::{SwapCalldataRequest, SwapCalldataResponse};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    post,
    path = "/v1/swap/calldata",
    tag = "Swap",
    security(("basicAuth" = [])),
    request_body = SwapCalldataRequest,
    responses(
        (status = 200, description = "Swap calldata", body = SwapCalldataResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/calldata", data = "<request>")]
pub async fn post_swap_calldata(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    request: Json<SwapCalldataRequest>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
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
