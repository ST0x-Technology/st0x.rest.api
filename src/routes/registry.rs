use crate::auth::AuthenticatedKey;
use crate::db::{registry_history, DbPool};
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use utoipa::ToSchema;

pub(crate) const REGISTRY_TYPE_PRIVATE_ARTIFACT: &str = "private_artifact";
pub(crate) const REGISTRY_TYPE_PUBLIC_URL: &str = "public_url";

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegistryMetadataResponse {
    pub registry_type: String,
    pub source_commit: Option<String>,
    pub payload_sha256: Option<String>,
    pub changed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegistryHistoryEntryResponse {
    pub source_commit: String,
    pub payload_sha256: String,
    pub actor_key_id: String,
    pub actor_label: String,
    pub actor_owner: String,
    pub validation_status: String,
    pub validation_error: Option<String>,
    pub changed_at: String,
}

impl From<registry_history::PrivateRegistryHistoryRow> for RegistryHistoryEntryResponse {
    fn from(value: registry_history::PrivateRegistryHistoryRow) -> Self {
        Self {
            source_commit: value.source_commit,
            payload_sha256: value.payload_sha256,
            actor_key_id: value.actor_key_id,
            actor_label: value.actor_label,
            actor_owner: value.actor_owner,
            validation_status: value.validation_status,
            validation_error: value.validation_error,
            changed_at: value.changed_at,
        }
    }
}

#[utoipa::path(
    get,
    path = "/registry",
    tag = "Registry",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "Current registry metadata", body = RegistryMetadataResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/registry")]
pub async fn get_registry(
    _global: GlobalRateLimit,
    key: AuthenticatedKey,
    pool: &State<DbPool>,
    span: TracingSpan,
) -> Result<Json<RegistryMetadataResponse>, ApiError> {
    async move {
        tracing::info!(auth_key_id = %key.key_id, auth_key_row_id = key.id, "request received");
        let latest = registry_history::latest_successful_private_registry(pool)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query latest private registry history");
                ApiError::Internal("failed to retrieve registry metadata".into())
            })?;

        if let Some(row) = latest {
            return Ok(Json(RegistryMetadataResponse {
                registry_type: REGISTRY_TYPE_PRIVATE_ARTIFACT.to_string(),
                source_commit: Some(row.source_commit),
                payload_sha256: Some(row.payload_sha256),
                changed_at: Some(row.changed_at),
            }));
        }

        Ok(Json(RegistryMetadataResponse {
            registry_type: REGISTRY_TYPE_PUBLIC_URL.to_string(),
            source_commit: None,
            payload_sha256: None,
            changed_at: None,
        }))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/registry/history",
    tag = "Registry",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "Registry artifact metadata history", body = Vec<RegistryHistoryEntryResponse>),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/registry/history")]
pub async fn get_registry_history(
    _global: GlobalRateLimit,
    key: AuthenticatedKey,
    pool: &State<DbPool>,
    span: TracingSpan,
) -> Result<Json<Vec<RegistryHistoryEntryResponse>>, ApiError> {
    async move {
        tracing::info!(auth_key_id = %key.key_id, auth_key_row_id = key.id, "request received");

        let history = registry_history::list_private_registry_history(pool)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query registry history");
                ApiError::Internal("failed to retrieve registry history".into())
            })?
            .into_iter()
            .map(RegistryHistoryEntryResponse::from)
            .collect::<Vec<_>>();

        tracing::info!(count = history.len(), "returning registry history");
        Ok(Json(history))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_registry, get_registry_history]
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{
        basic_auth_header, mock_raindex_registry_artifact, seed_admin_key, seed_api_key,
        TestClientBuilder,
    };
    use rocket::http::{Header, Status};
    use serde_json::json;

    const COMMIT_ONE: &str = "1111111111111111111111111111111111111111";
    const COMMIT_TWO: &str = "2222222222222222222222222222222222222222";

    fn upload_body(artifact: &str, commit: &str) -> String {
        json!({
            "registry_artifact": artifact,
            "source_commit": commit
        })
        .to_string()
    }

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
        assert_eq!(body["registry_type"], "public_url");
        assert!(body.get("registry_url").is_none());
    }

    #[rocket::async_test]
    async fn test_get_registry_without_auth_returns_401() {
        let client = TestClientBuilder::new().build().await;
        let response = client.get("/registry").dispatch().await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_registry_history_with_valid_auth_returns_empty_array() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/registry/history")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body, serde_json::json!([]));
    }

    #[rocket::async_test]
    async fn test_get_registry_history_without_auth_returns_401() {
        let client = TestClientBuilder::new().build().await;
        let response = client.get("/registry/history").dispatch().await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_registry_history_returns_entries_newest_first() {
        let client = TestClientBuilder::new().build().await;
        let (admin_key_id, admin_secret) = seed_admin_key(&client).await;
        let admin_header = basic_auth_header(&admin_key_id, &admin_secret);
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let artifact = mock_raindex_registry_artifact();

        let first_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", admin_header.clone()))
            .header(rocket::http::ContentType::JSON)
            .body(upload_body(&artifact, COMMIT_ONE))
            .dispatch()
            .await;
        assert_eq!(first_response.status(), Status::Ok);

        let second_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", admin_header))
            .header(rocket::http::ContentType::JSON)
            .body(upload_body(&artifact, COMMIT_TWO))
            .dispatch()
            .await;
        assert_eq!(second_response.status(), Status::Ok);

        let response = client
            .get("/registry/history")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let history = body.as_array().expect("history is an array");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["source_commit"], COMMIT_TWO);
        assert_eq!(history[0]["validation_status"], "success");
        assert!(history[0]["payload_sha256"].as_str().is_some());
        assert!(!history[0]["payload_sha256"]
            .as_str()
            .expect("payload sha is a string")
            .contains("data:"));
        assert_eq!(history[0]["actor_key_id"], admin_key_id);
        assert_eq!(history[0]["actor_label"], "admin-key");
        assert_eq!(history[0]["actor_owner"], "admin-owner");
        assert!(!history[0]["changed_at"]
            .as_str()
            .expect("changed_at is a string")
            .is_empty());
        assert!(history[0].get("id").is_none());
        assert_eq!(history[1]["source_commit"], COMMIT_ONE);
    }
}
