use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::SharedRaindexProvider;
use rain_orderbook_app_settings::token::TokenCfg;
use rain_orderbook_common::raindex_client::RaindexError;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::Serialize;
use serde_json::Value;
use tracing::Instrument;

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    #[serde(flatten)]
    token: TokenCfg,
    name: Option<String>,
    isin: Option<String>,
}

impl From<TokenCfg> for TokenResponse {
    fn from(token: TokenCfg) -> Self {
        let name = token.label.clone();
        let isin = token
            .extensions
            .as_ref()
            .and_then(|extensions| extract_extension_string(extensions, "isin"))
            .or_else(|| {
                token
                    .extensions
                    .as_ref()
                    .and_then(|extensions| extract_extension_string(extensions, "ISIN"))
            });

        Self { token, name, isin }
    }
}

fn extract_extension_string(
    extensions: &std::collections::HashMap<String, Value>,
    key: &str,
) -> Option<String> {
    match extensions.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Null) | None => None,
        Some(value) => Some(value.to_string()),
    }
}

#[utoipa::path(
    get,
    path = "/v1/tokens",
    tag = "Tokens",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "List of supported tokens", body = Vec<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/")]
pub async fn get_tokens(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
) -> Result<Json<Vec<TokenResponse>>, ApiError> {
    async move {
        tracing::info!("request received");

        let raindex = shared_raindex.read().await;
        let tokens = raindex
            .client()
            .get_all_tokens()
            .map_err(|e: RaindexError| {
                tracing::error!(error = %e, "failed to get tokens from raindex");
                ApiError::Internal("failed to retrieve token list".into())
            })?;

        let result: Vec<TokenResponse> = tokens.into_values().map(TokenResponse::from).collect();
        tracing::info!(count = result.len(), "returning tokens");
        Ok(Json(result))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_tokens]
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{
        basic_auth_header, mock_raindex_registry_url_with_settings_and_tokens, seed_api_key,
        TestClientBuilder,
    };
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn test_get_tokens_returns_token_list() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let tokens = body.as_array().expect("tokens is an array");
        assert_eq!(tokens.len(), 1);
        let first = &tokens[0];
        assert_eq!(
            first["address"],
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
        );
    }

    #[rocket::async_test]
    async fn test_get_tokens_returns_multiple_tokens() {
        let settings = r#"version: 5
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
orderbooks:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
rainlangs:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
tokens:
  usdc:
    address: 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
    network: base
    decimals: 6
    label: USD Coin
    symbol: USDC
  weth:
    address: 0x4200000000000000000000000000000000000006
    network: base
    decimals: 18
    label: Wrapped Ether
    symbol: WETH
"#;
        let registry_url =
            crate::test_helpers::mock_raindex_registry_url_with_settings(settings).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load raindex config");
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let tokens = body.as_array().expect("tokens is an array");
        assert_eq!(tokens.len(), 2);
    }

    #[rocket::async_test]
    async fn test_get_tokens_adds_name_and_isin_from_remote_tokens() {
        let settings = r#"version: 5
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
orderbooks:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
rainlangs:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
using-tokens-from:
  - __TOKENS_URL__
"#;
        let remote_tokens = r#"{
  "name": "ST0x Base Token List",
  "timestamp": "2026-03-20T00:00:00.000Z",
  "version": {
    "major": 1,
    "minor": 0,
    "patch": 0
  },
  "tokens": [
    {
      "chainId": 8453,
      "address": "0x8AFba81DEc38DE0A18E2Df5E1967a7493651eebf",
      "decimals": 18,
      "name": "Wrapped Circle Internet Group Inc ST0x",
      "symbol": "wtCRCL",
      "logoURI": "https://tokens.st0x.com/images/CRCL.png",
      "extensions": {
        "category": "ST0x",
        "isin": "US1725731079"
      }
    }
  ]
}"#;
        let registry_url =
            mock_raindex_registry_url_with_settings_and_tokens(settings, remote_tokens).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load raindex config");
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let tokens = body.as_array().expect("tokens is an array");
        assert_eq!(tokens.len(), 1);

        let first = &tokens[0];
        assert_eq!(
            first["label"],
            serde_json::Value::String("Wrapped Circle Internet Group Inc ST0x".to_string())
        );
        assert_eq!(
            first["name"],
            serde_json::Value::String("Wrapped Circle Internet Group Inc ST0x".to_string())
        );
        assert_eq!(
            first["isin"],
            serde_json::Value::String("US1725731079".to_string())
        );
    }
}
