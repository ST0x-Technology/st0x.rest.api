use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::TracingSpan;
use crate::types::common::ValidatedFixedBytes;
use crate::types::order::{
    CancelOrderRequest, CancelOrderResponse, DeployDcaOrderRequest, DeployOrderResponse,
    DeploySolverOrderRequest, OrderDetail,
};
use rocket::serde::json::Json;
use rocket::Route;
use tracing::Instrument;

#[utoipa::path(
    post,
    path = "/v1/order/dca",
    tag = "Order",
    request_body = DeployDcaOrderRequest,
    responses(
        (status = 200, description = "DCA order deployment result", body = DeployOrderResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/dca", data = "<request>")]
pub async fn post_order_dca(
    span: TracingSpan,
    request: Json<DeployDcaOrderRequest>,
) -> Result<Json<DeployOrderResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        todo!()
    }.instrument(span.0).await
}

#[utoipa::path(
    post,
    path = "/v1/order/solver",
    tag = "Order",
    request_body = DeploySolverOrderRequest,
    responses(
        (status = 200, description = "Solver order deployment result", body = DeployOrderResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/solver", data = "<request>")]
pub async fn post_order_solver(
    span: TracingSpan,
    request: Json<DeploySolverOrderRequest>,
) -> Result<Json<DeployOrderResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        todo!()
    }.instrument(span.0).await
}

#[utoipa::path(
    get,
    path = "/v1/order/{order_hash}",
    tag = "Order",
    params(
        ("order_hash" = String, Path, description = "The order hash"),
    ),
    responses(
        (status = 200, description = "Order details", body = OrderDetail),
        (status = 404, description = "Order not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<order_hash>")]
pub async fn get_order(
    span: TracingSpan,
    order_hash: ValidatedFixedBytes,
) -> Result<Json<OrderDetail>, ApiError> {
    async move {
        tracing::info!(order_hash = ?order_hash, "request received");
        todo!()
    }.instrument(span.0).await
}

#[utoipa::path(
    post,
    path = "/v1/order/cancel",
    tag = "Order",
    request_body = CancelOrderRequest,
    responses(
        (status = 200, description = "Cancel order result", body = CancelOrderResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 404, description = "Order not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/cancel", data = "<request>")]
pub async fn post_order_cancel(
    span: TracingSpan,
    request: Json<CancelOrderRequest>,
) -> Result<Json<CancelOrderResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        todo!()
    }.instrument(span.0).await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        post_order_dca,
        post_order_solver,
        get_order,
        post_order_cancel
    ]
}
