use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::tokens::TokenListResponse;
use rocket::serde::json::Json;
use rocket::Route;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/v1/tokens",
    tag = "Tokens",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "List of supported tokens", body = TokenListResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/")]
pub async fn get_tokens(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
) -> Result<Json<TokenListResponse>, ApiError> {
    async move {
        tracing::info!("request received");
        todo!()
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_tokens]
}
