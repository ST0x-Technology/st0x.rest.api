use crate::auth::AuthenticatedKey;
use crate::db::{registry_history, DbPool};
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
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

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegistryHistoryEntryResponse {
    pub previous_url: String,
    pub new_url: String,
    pub actor_key_id: String,
    pub actor_label: String,
    pub actor_owner: String,
    pub changed_at: String,
}

impl From<registry_history::RegistryUrlHistoryRow> for RegistryHistoryEntryResponse {
    fn from(value: registry_history::RegistryUrlHistoryRow) -> Self {
        Self {
            previous_url: value.previous_url,
            new_url: value.new_url,
            actor_key_id: value.actor_key_id,
            actor_label: value.actor_label,
            actor_owner: value.actor_owner,
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
        (status = 200, description = "Current registry URL", body = RegistryResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/registry")]
pub async fn get_registry(
    _global: GlobalRateLimit,
    key: AuthenticatedKey,
    shared_raindex: &State<SharedRaindexProvider>,
    span: TracingSpan,
) -> Result<Json<RegistryResponse>, ApiError> {
    async move {
        tracing::info!(auth_key_id = %key.key_id, auth_key_row_id = key.id, "request received");
        let raindex = shared_raindex.read().await;
        Ok(Json(RegistryResponse {
            registry_url: raindex.registry_url(),
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
        (status = 200, description = "Registry URL change history", body = Vec<RegistryHistoryEntryResponse>),
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

        let history = registry_history::list_registry_url_history(pool)
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
        basic_auth_header, mock_raindex_registry_url, seed_admin_key, seed_api_key,
        TestClientBuilder,
    };
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
    async fn test_get_registry_history_returns_entries_for_non_admin_key() {
        let client = TestClientBuilder::new().build().await;
        let (admin_key_id, admin_secret) = seed_admin_key(&client).await;
        let admin_header = basic_auth_header(&admin_key_id, &admin_secret);
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let new_url = mock_raindex_registry_url().await;

        let update_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", admin_header))
            .header(rocket::http::ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{new_url}"}}"#))
            .dispatch()
            .await;
        assert_eq!(update_response.status(), Status::Ok);

        let response = client
            .get("/registry/history")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let history = body.as_array().expect("history is an array");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["new_url"], new_url);
        assert_eq!(history[0]["actor_key_id"], admin_key_id);
        assert_eq!(history[0]["actor_label"], "admin-key");
        assert_eq!(history[0]["actor_owner"], "admin-owner");
        assert!(history[0]["previous_url"]
            .as_str()
            .expect("previous_url is a string")
            .contains("registry.txt"));
        assert!(!history[0]["changed_at"]
            .as_str()
            .expect("changed_at is a string")
            .is_empty());
        assert!(history[0].get("id").is_none());
    }

    #[rocket::async_test]
    async fn test_get_registry_history_returns_entries_newest_first() {
        let client = TestClientBuilder::new().build().await;
        let (admin_key_id, admin_secret) = seed_admin_key(&client).await;
        let admin_header = basic_auth_header(&admin_key_id, &admin_secret);
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let first_url = mock_raindex_registry_url().await;
        let second_url = mock_raindex_registry_url().await;

        let first_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", admin_header.clone()))
            .header(rocket::http::ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{first_url}"}}"#))
            .dispatch()
            .await;
        assert_eq!(first_response.status(), Status::Ok);

        let second_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", admin_header))
            .header(rocket::http::ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{second_url}"}}"#))
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
        assert_eq!(history[0]["new_url"], second_url);
        assert_eq!(history[0]["previous_url"], first_url);
        assert_eq!(history[1]["new_url"], first_url);
    }
}
