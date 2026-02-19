use crate::error::ApiError;
use crate::fairings::TracingSpan;
use crate::types::health::HealthResponse;
use rocket::serde::json::Json;
use rocket::Route;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/health",
    tag = "Health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse),
    )
)]
#[get("/health")]
pub async fn get_health(span: TracingSpan) -> Result<Json<HealthResponse>, ApiError> {
    async move {
        tracing::info!("request received");
        Ok(Json(HealthResponse {
            status: "ok".into(),
        }))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_health]
}
