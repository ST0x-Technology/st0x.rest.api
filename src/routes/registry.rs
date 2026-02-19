use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::TracingSpan;
use crate::raindex::SharedRaindexProvider;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegistryResponse {
    pub registry_url: String,
}

#[utoipa::path(
    get,
    path = "/registry",
    tag = "Registry",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "Current registry URL", body = RegistryResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/registry")]
pub async fn get_registry(
    _key: AuthenticatedKey,
    shared_raindex: &State<SharedRaindexProvider>,
    span: TracingSpan,
) -> Result<Json<RegistryResponse>, ApiError> {
    async move {
        tracing::info!("request received");
        let raindex = shared_raindex.read().await;
        Ok(Json(RegistryResponse {
            registry_url: raindex.registry_url(),
        }))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_registry]
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn test_get_registry_with_valid_auth() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/registry")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert!(body["registry_url"]
            .as_str()
            .unwrap()
            .contains("registry.txt"));
    }

    #[rocket::async_test]
    async fn test_get_registry_without_auth_returns_401() {
        let client = TestClientBuilder::new().build().await;
        let response = client.get("/registry").dispatch().await;
        assert_eq!(response.status(), Status::Unauthorized);
    }
}
