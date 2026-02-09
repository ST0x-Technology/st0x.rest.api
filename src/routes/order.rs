use crate::error::{ApiError, ApiErrorResponse};
use crate::types::common::ValidatedFixedBytes;
use crate::types::order::{
    CancelOrderRequest, CancelOrderResponse, DeployDcaOrderRequest, DeployOrderResponse,
    DeploySolverOrderRequest, OrderDetail,
};
use rocket::serde::json::Json;
use rocket::Route;

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
    request: Json<DeployDcaOrderRequest>,
) -> Result<Json<DeployOrderResponse>, ApiError> {
    let _ = request.into_inner();
    todo!()
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
    request: Json<DeploySolverOrderRequest>,
) -> Result<Json<DeployOrderResponse>, ApiError> {
    let _ = request.into_inner();
    todo!()
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
pub async fn get_order(order_hash: ValidatedFixedBytes) -> Result<Json<OrderDetail>, ApiError> {
    let _ = order_hash;
    todo!()
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
    request: Json<CancelOrderRequest>,
) -> Result<Json<CancelOrderResponse>, ApiError> {
    let _ = request.into_inner();
    todo!()
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        post_order_dca,
        post_order_solver,
        get_order,
        post_order_cancel
    ]
}
