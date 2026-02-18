use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedAddress;
use crate::types::trades::{TradesByAddressResponse, TradesPaginationParams};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/v1/trades/{address}",
    tag = "Trades",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Owner address"),
        TradesPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of trades", body = TradesByAddressResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<address>?<params..>", rank = 2)]
pub async fn get_trades_by_address(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: TradesPaginationParams,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "request received");
        raindex
            .run_with_client(move |_client| async move { todo!() })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}
