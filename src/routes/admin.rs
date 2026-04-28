use crate::auth::AdminKey;
use crate::db::{registry_history, settings, DbPool};
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::{RaindexProvider, SharedRaindexProvider};
use crate::routes::registry::RegistryResponse;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateRegistryRequest {
    pub registry_url: String,
}

#[utoipa::path(
    put,
    path = "/admin/registry",
    tag = "Admin",
    security(("basicAuth" = [])),
    request_body = UpdateRegistryRequest,
    responses(
        (status = 200, description = "Registry updated", body = RegistryResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[put("/registry", data = "<request>")]
pub async fn put_registry(
    _global: GlobalRateLimit,
    admin: AdminKey,
    shared_raindex: &State<SharedRaindexProvider>,
    pool: &State<DbPool>,
    span: TracingSpan,
    request: Json<UpdateRegistryRequest>,
) -> Result<Json<RegistryResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(
            registry_url = %req.registry_url,
            admin_key_id = %admin.0.key_id,
            "request received"
        );

        if req.registry_url.is_empty() {
            return Err(ApiError::BadRequest(
                "registry_url must not be empty".into(),
            ));
        }

        let (previous_url, db_path) = {
            let guard = shared_raindex.read().await;
            (guard.registry_url(), guard.db_path())
        };

        if previous_url == req.registry_url {
            tracing::info!(
                registry_url = %req.registry_url,
                admin_key_id = %admin.0.key_id,
                "registry unchanged"
            );
            return Ok(Json(RegistryResponse {
                registry_url: req.registry_url,
            }));
        }

        let rpc_overrides = {
            let guard = shared_raindex.read().await;
            guard.rpc_overrides()
        };
        let new_provider = RaindexProvider::load(&req.registry_url, db_path, rpc_overrides)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to load new registry");
                ApiError::BadRequest(format!("failed to load registry: {e}"))
            })?;

        let mut guard = shared_raindex.write().await;
        let previous_url = guard.registry_url();

        if previous_url == req.registry_url {
            tracing::info!(
                registry_url = %req.registry_url,
                admin_key_id = %admin.0.key_id,
                "registry unchanged"
            );
            return Ok(Json(RegistryResponse {
                registry_url: req.registry_url,
            }));
        }

        let mut tx = pool.begin().await.map_err(|e| {
            tracing::error!(error = %e, "failed to begin registry update transaction");
            ApiError::Internal("failed to persist setting".into())
        })?;

        settings::set_setting_in_tx(&mut tx, "registry_url", &req.registry_url)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to persist registry_url");
                ApiError::Internal("failed to persist setting".into())
            })?;

        registry_history::insert_registry_url_change(
            &mut tx,
            &registry_history::NewRegistryUrlHistory {
                previous_url: &previous_url,
                new_url: &req.registry_url,
                actor_key_id: &admin.0.key_id,
                actor_label: &admin.0.label,
                actor_owner: &admin.0.owner,
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to persist registry_url history");
            ApiError::Internal("failed to persist setting".into())
        })?;

        tx.commit().await.map_err(|e| {
            tracing::error!(error = %e, "failed to commit registry update transaction");
            ApiError::Internal("failed to persist setting".into())
        })?;

        *guard = new_provider;
        drop(guard);

        tracing::info!(
            previous_registry_url = %previous_url,
            registry_url = %req.registry_url,
            admin_key_id = %admin.0.key_id,
            "registry updated"
        );

        Ok(Json(RegistryResponse {
            registry_url: req.registry_url,
        }))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![put_registry]
}

#[cfg(test)]
mod tests {
    use crate::db::registry_history::{self, RegistryUrlHistoryRow};
    use crate::test_helpers::{
        basic_auth_header, mock_raindex_registry_url, seed_admin_key, seed_api_key,
        TestClientBuilder,
    };
    use rocket::http::{ContentType, Header, Status};

    async fn history_rows(
        client: &rocket::local::asynchronous::Client,
    ) -> Vec<RegistryUrlHistoryRow> {
        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        registry_history::list_registry_url_history(pool)
            .await
            .expect("query registry history")
    }

    #[rocket::async_test]
    async fn test_put_registry_with_admin_key() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let original_url = {
            let get_response = client
                .get("/registry")
                .header(Header::new("Authorization", header.clone()))
                .dispatch()
                .await;
            let get_body: serde_json::Value =
                serde_json::from_str(&get_response.into_string().await.unwrap()).unwrap();
            get_body["registry_url"].as_str().unwrap().to_string()
        };
        let new_url = mock_raindex_registry_url().await;

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header.clone()))
            .header(ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{new_url}"}}"#))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["registry_url"], new_url);

        let get_response = client
            .get("/registry")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(get_response.status(), Status::Ok);
        let get_body: serde_json::Value =
            serde_json::from_str(&get_response.into_string().await.unwrap()).unwrap();
        assert_eq!(get_body["registry_url"], new_url);

        let history = history_rows(&client).await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].previous_url, original_url);
        assert_eq!(history[0].new_url, new_url);
        assert_eq!(history[0].actor_key_id, key_id);
        assert_eq!(history[0].actor_label, "admin-key");
        assert_eq!(history[0].actor_owner, "admin-owner");
        assert!(!history[0].changed_at.is_empty());
    }

    #[rocket::async_test]
    async fn test_put_registry_with_non_admin_key_returns_403() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(r#"{"registry_url":"http://example.com/registry.txt"}"#)
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Forbidden);
        assert!(history_rows(&client).await.is_empty());
    }

    #[rocket::async_test]
    async fn test_put_registry_without_auth_returns_401() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .put("/admin/registry")
            .header(ContentType::JSON)
            .body(r#"{"registry_url":"http://example.com/registry.txt"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
        assert!(history_rows(&client).await.is_empty());
    }

    #[rocket::async_test]
    async fn test_put_registry_with_bad_url_returns_400() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let get_before = client
            .get("/registry")
            .header(Header::new("Authorization", header.clone()))
            .dispatch()
            .await;
        let before_body: serde_json::Value =
            serde_json::from_str(&get_before.into_string().await.unwrap()).unwrap();
        let original_url = before_body["registry_url"].as_str().unwrap().to_string();

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header.clone()))
            .header(ContentType::JSON)
            .body(r#"{"registry_url":"http://127.0.0.1:1/bad-registry.txt"}"#)
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::BadRequest);

        let get_after = client
            .get("/registry")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        let after_body: serde_json::Value =
            serde_json::from_str(&get_after.into_string().await.unwrap()).unwrap();
        assert_eq!(after_body["registry_url"], original_url);
        assert!(history_rows(&client).await.is_empty());
    }

    #[rocket::async_test]
    async fn test_put_registry_persists_to_db() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let new_url = mock_raindex_registry_url().await;

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{new_url}"}}"#))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);

        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        let stored: Option<String> = crate::db::settings::get_setting(pool, "registry_url")
            .await
            .expect("query setting");
        assert_eq!(stored.unwrap(), new_url);

        let history = history_rows(&client).await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].new_url, new_url);
    }

    #[rocket::async_test]
    async fn test_put_registry_empty_url_returns_400() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(r#"{"registry_url":""}"#)
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::BadRequest);
        assert!(history_rows(&client).await.is_empty());
    }

    #[rocket::async_test]
    async fn test_put_registry_same_url_is_noop() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let get_response = client
            .get("/registry")
            .header(Header::new("Authorization", header.clone()))
            .dispatch()
            .await;
        assert_eq!(get_response.status(), Status::Ok);
        let get_body: serde_json::Value =
            serde_json::from_str(&get_response.into_string().await.unwrap()).unwrap();
        let current_url = get_body["registry_url"].as_str().unwrap().to_string();

        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{current_url}"}}"#))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        let stored: Option<String> = crate::db::settings::get_setting(pool, "registry_url")
            .await
            .expect("query setting");
        assert!(stored.is_none());
        assert!(history_rows(&client).await.is_empty());
    }

    #[rocket::async_test]
    async fn test_put_registry_records_multiple_updates_in_order() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let first_url = mock_raindex_registry_url().await;
        let second_url = mock_raindex_registry_url().await;

        let first_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header.clone()))
            .header(ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{first_url}"}}"#))
            .dispatch()
            .await;
        assert_eq!(first_response.status(), Status::Ok);

        let second_response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{second_url}"}}"#))
            .dispatch()
            .await;
        assert_eq!(second_response.status(), Status::Ok);

        let history = history_rows(&client).await;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].new_url, second_url);
        assert_eq!(history[0].previous_url, first_url);
        assert_eq!(history[0].actor_key_id, key_id);
        assert_eq!(history[1].new_url, first_url);
    }
}
