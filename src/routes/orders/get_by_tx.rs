use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedFixedBytes;
use crate::types::orders::{OrdersByTxParams, OrdersByTxResponse};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/v2/orders/tx/{tx_hash}",
    tag = "Orders",
    security(("basicAuth" = [])),
    params(
        ("tx_hash" = String, Path, description = "Transaction hash"),
        OrdersByTxParams,
    ),
    responses(
        (status = 200, description = "Orders from transaction", body = OrdersByTxResponse),
        (status = 202, description = "Transaction not yet indexed", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 404, description = "Transaction not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/tx/<tx_hash>?<params..>")]
pub async fn get_orders_by_tx(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    span: TracingSpan,
    tx_hash: ValidatedFixedBytes,
    params: OrdersByTxParams,
) -> Result<Json<OrdersByTxResponse>, ApiError> {
    async move {
        tracing::info!(tx_hash = ?tx_hash, "request received");
        let raindex = shared_raindex.read().await;
        let _chain_id =
            crate::routes::resolve_required_chain_id(raindex.raindex_yaml(), params.chain_id)?;
        todo!()
    }
    .instrument(span.0)
    .await
}
