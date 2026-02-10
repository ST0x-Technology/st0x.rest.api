use crate::error::{ApiError, ApiErrorResponse};
use crate::types::swap::{
    SwapCalldataRequest, SwapCalldataResponse, SwapQuoteRequest, SwapQuoteResponse,
};
use rocket::serde::json::Json;
use rocket::Route;

#[utoipa::path(
    post,
    path = "/v1/swap/quote",
    tag = "Swap",
    request_body = SwapQuoteRequest,
    responses(
        (status = 200, description = "Swap quote", body = SwapQuoteResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/quote", data = "<request>")]
pub async fn post_swap_quote(
    request: Json<SwapQuoteRequest>,
) -> Result<Json<SwapQuoteResponse>, ApiError> {
    let _ = request.into_inner();
    todo!()
}

#[utoipa::path(
    post,
    path = "/v1/swap/calldata",
    tag = "Swap",
    request_body = SwapCalldataRequest,
    responses(
        (status = 200, description = "Swap calldata", body = SwapCalldataResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/calldata", data = "<request>")]
pub async fn post_swap_calldata(
    request: Json<SwapCalldataRequest>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
    let _ = request.into_inner();
    todo!()
}

pub fn routes() -> Vec<Route> {
    rocket::routes![post_swap_quote, post_swap_calldata]
}
