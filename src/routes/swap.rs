use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::TracingSpan;
use crate::types::swap::{
    SwapCalldataRequest, SwapCalldataResponse, SwapQuoteRequest, SwapQuoteResponse,
};
use rocket::serde::json::Json;
use rocket::Route;
use tracing::Instrument;

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
    span: TracingSpan,
    request: Json<SwapQuoteRequest>,
) -> Result<Json<SwapQuoteResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        todo!()
    }.instrument(span.0).await
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
    span: TracingSpan,
    request: Json<SwapCalldataRequest>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        todo!()
    }.instrument(span.0).await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![post_swap_quote, post_swap_calldata]
}
