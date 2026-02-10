use crate::types::health::HealthResponse;
use rocket::serde::json::Json;
use rocket::Route;

#[utoipa::path(
    get,
    path = "/health",
    tag = "Health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse),
    )
)]
#[get("/health")]
pub async fn get_health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
    })
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_health]
}
