use crate::error::{ApiError, ApiErrorResponse};
use crate::types::tokens::TokenListResponse;
use rocket::serde::json::Json;
use rocket::Route;

#[utoipa::path(
    get,
    path = "/v1/tokens",
    tag = "Tokens",
    responses(
        (status = 200, description = "List of supported tokens", body = TokenListResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/")]
pub async fn get_tokens() -> Result<Json<TokenListResponse>, ApiError> {
    todo!()
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_tokens]
}
