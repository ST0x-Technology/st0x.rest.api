use crate::auth::AdminKey;
use crate::cache::CacheGroup;
use crate::db::{settings, DbPool};
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
    _admin: AdminKey,
    shared_raindex: &State<SharedRaindexProvider>,
    pool: &State<DbPool>,
    span: TracingSpan,
    request: Json<UpdateRegistryRequest>,
    registry_caches: &State<CacheGroup>,
) -> Result<Json<RegistryResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(registry_url = %req.registry_url, "request received");

        if req.registry_url.is_empty() {
            return Err(ApiError::BadRequest(
                "registry_url must not be empty".into(),
            ));
        }

        let new_provider = RaindexProvider::load(&req.registry_url)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to load new registry");
                ApiError::BadRequest(format!("failed to load registry: {e}"))
            })?;

        let mut guard = shared_raindex.write().await;

        settings::set_setting(pool, "registry_url", &req.registry_url)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to persist registry_url");
                ApiError::Internal("failed to persist setting".into())
            })?;

        *guard = new_provider;
        drop(guard);

        registry_caches.invalidate_all();

        tracing::info!(registry_url = %req.registry_url, "registry updated, caches invalidated");

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
    use crate::test_helpers::{
        basic_auth_header, mock_raindex_registry_url, seed_admin_key, seed_api_key,
        TestClientBuilder,
    };
    use rocket::http::{ContentType, Header, Status};

    #[rocket::async_test]
    async fn test_put_registry_with_admin_key() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
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
    }

    #[rocket::async_test]
    async fn test_put_registry_persists_to_db() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let new_url = mock_raindex_registry_url().await;

        client
            .put("/admin/registry")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{new_url}"}}"#))
            .dispatch()
            .await;

        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        let stored: Option<String> = crate::db::settings::get_setting(pool, "registry_url")
            .await
            .expect("query setting");
        assert_eq!(stored.unwrap(), new_url);
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
    }

    #[rocket::async_test]
    async fn test_put_registry_invalidates_caches() {
        use crate::routes::order::OrderDetailCache;
        use crate::routes::swap::SwapQuoteCache;
        use crate::types::common::TokenRef;
        use crate::types::order::{OrderDetail, OrderDetailsInfo, OrderType};
        use crate::types::swap::SwapQuoteResponse;
        use alloy::primitives::{address, Address, U256};

        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let order_hash = "0x000000000000000000000000000000000000000000000000000000000000abcd"
            .parse()
            .unwrap();
        let dummy_order = OrderDetail {
            order_hash,
            owner: Address::ZERO,
            order_details: OrderDetailsInfo {
                type_: OrderType::Solver,
                io_ratio: "1.0".into(),
            },
            input_token: TokenRef {
                address: Address::ZERO,
                symbol: "USDC".into(),
                decimals: 6,
            },
            output_token: TokenRef {
                address: Address::ZERO,
                symbol: "WETH".into(),
                decimals: 18,
            },
            input_vault_id: U256::ZERO,
            output_vault_id: U256::ZERO,
            input_vault_balance: "0".into(),
            output_vault_balance: "0".into(),
            io_ratio: "1.0".into(),
            created_at: 0,
            orderbook_id: Address::ZERO,
            trades: vec![],
        };
        let order_cache = client
            .rocket()
            .state::<OrderDetailCache>()
            .expect("OrderDetailCache in state");
        order_cache.insert(order_hash, dummy_order).await;
        assert!(order_cache.get(&order_hash).await.is_some());

        let usdc = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        let weth = address!("4200000000000000000000000000000000000006");
        let cache_key = (usdc, weth, "100".to_string());
        let dummy_quote = SwapQuoteResponse {
            input_token: usdc,
            output_token: weth,
            output_amount: "100".into(),
            estimated_output: "100".into(),
            estimated_input: "150".into(),
            estimated_io_ratio: "1.5".into(),
        };
        let swap_cache = client
            .rocket()
            .state::<SwapQuoteCache>()
            .expect("SwapQuoteCache in state");
        swap_cache.insert(cache_key.clone(), dummy_quote).await;
        assert!(swap_cache.get(&cache_key).await.is_some());

        let new_url = mock_raindex_registry_url().await;
        let response = client
            .put("/admin/registry")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(format!(r#"{{"registry_url":"{new_url}"}}"#))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);

        tokio::task::yield_now().await;
        assert!(
            order_cache.get(&order_hash).await.is_none(),
            "order cache must be empty after registry update"
        );
        assert!(
            swap_cache.get(&cache_key).await.is_none(),
            "swap cache must be empty after registry update"
        );
    }
}
