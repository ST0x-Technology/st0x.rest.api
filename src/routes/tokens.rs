use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::tokens::{RemoteTokenList, TokenInfo, TokenListResponse};
use rocket::fairing::AdHoc;
use rocket::serde::json::Json;
use rocket::{Route, State};
use std::time::Duration;
use tracing::Instrument;

const TOKEN_LIST_URL: &str = "https://raw.githubusercontent.com/S01-Issuer/st0x-tokens/ad1a637a79d5a220ad089aecdc5b7239d3473f6e/src/st0xTokens.json";
const TARGET_CHAIN_ID: u32 = crate::CHAIN_ID;
const TOKEN_LIST_TIMEOUT_SECS: u64 = 10;

pub(crate) struct TokensConfig {
    pub(crate) url: String,
    pub(crate) client: reqwest::Client,
}

impl Default for TokensConfig {
    fn default() -> Self {
        Self {
            url: TOKEN_LIST_URL.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

impl TokensConfig {
    #[cfg(test)]
    pub(crate) fn with_url(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            client: reqwest::Client::new(),
        }
    }
}

pub(crate) fn fairing() -> AdHoc {
    AdHoc::on_ignite("Tokens Config", |rocket| async {
        if rocket.state::<TokensConfig>().is_some() {
            tracing::info!("TokensConfig already managed; skipping default initialization");
            rocket
        } else {
            tracing::info!(url = %TOKEN_LIST_URL, "initializing default TokensConfig");
            rocket.manage(TokensConfig::default())
        }
    })
}

#[derive(Debug, thiserror::Error)]
enum TokenError {
    #[error("failed to fetch token list: {0}")]
    Fetch(reqwest::Error),
    #[error("failed to deserialize token list: {0}")]
    Deserialize(reqwest::Error),
    #[error("token list returned non-200 status: {0}")]
    BadStatus(reqwest::StatusCode),
}

impl From<TokenError> for ApiError {
    fn from(e: TokenError) -> Self {
        tracing::error!(error = %e, "token list fetch failed");
        ApiError::Internal("failed to retrieve token list".into())
    }
}

#[utoipa::path(
    get,
    path = "/v1/tokens",
    tag = "Tokens",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "List of supported tokens", body = TokenListResponse),
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
    tokens_config: &State<TokensConfig>,
) -> Result<Json<TokenListResponse>, ApiError> {
    let url = tokens_config.url.clone();
    let client = tokens_config.client.clone();
    async move {
        tracing::info!("request received");

        tracing::info!(url = %url, timeout_secs = TOKEN_LIST_TIMEOUT_SECS, "fetching token list");

        let response = client
            .get(&url)
            .timeout(Duration::from_secs(TOKEN_LIST_TIMEOUT_SECS))
            .send()
            .await
            .map_err(TokenError::Fetch)?;

        let status = response.status();
        if !status.is_success() {
            return Err(TokenError::BadStatus(status).into());
        }

        let remote: RemoteTokenList = response.json().await.map_err(TokenError::Deserialize)?;

        let tokens: Vec<TokenInfo> = remote
            .tokens
            .into_iter()
            .filter(|t| t.chain_id == TARGET_CHAIN_ID)
            .map(|t| {
                let isin = t
                    .extensions
                    .get("isin")
                    .or_else(|| t.extensions.get("ISIN"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                TokenInfo {
                    address: t.address,
                    symbol: t.symbol,
                    name: t.name,
                    isin,
                    decimals: t.decimals,
                }
            })
            .collect();

        tracing::info!(count = tokens.len(), "returning tokens");
        Ok(Json(TokenListResponse { tokens }))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_tokens]
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use rocket::http::{Header, Status};

    async fn mock_server(response: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                tokio::io::AsyncWriteExt::write_all(&mut socket, response)
                    .await
                    .ok();
            }
        });
        format!("http://{addr}")
    }

    #[rocket::async_test]
    async fn test_get_tokens_returns_token_list() {
        let body = r#"{"tokens":[{"chainId":8453,"address":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","name":"USD Coin","symbol":"USDC","decimals":6,"extensions":{"isin":"US1234567890"}}]}"#;
        let response_bytes = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response_bytes: &'static [u8] =
            Box::leak(response_bytes.into_bytes().into_boxed_slice());
        let url = mock_server(response_bytes).await;
        let client = TestClientBuilder::new().token_list_url(&url).build().await;
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
        let tokens = body["tokens"].as_array().expect("tokens is an array");
        assert_eq!(tokens.len(), 1);
        let first = &tokens[0];
        assert_eq!(
            first["address"],
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
        );
        assert_eq!(first["symbol"], "USDC");
        assert_eq!(first["name"], "USD Coin");
        assert_eq!(first["decimals"], 6);
        assert_eq!(first["ISIN"], "US1234567890");
    }

    #[rocket::async_test]
    async fn test_get_tokens_omits_isin_when_not_in_extensions() {
        let body = r#"{"tokens":[{"chainId":8453,"address":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","name":"USD Coin","symbol":"USDC","decimals":6}]}"#;
        let response_bytes = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response_bytes: &'static [u8] =
            Box::leak(response_bytes.into_bytes().into_boxed_slice());
        let url = mock_server(response_bytes).await;
        let client = TestClientBuilder::new().token_list_url(&url).build().await;
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
        let first = &body["tokens"][0];
        assert!(first.get("ISIN").is_none());
    }

    #[rocket::async_test]
    async fn test_get_tokens_reads_uppercase_isin_key() {
        let body = r#"{"tokens":[{"chainId":8453,"address":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","name":"USD Coin","symbol":"USDC","decimals":6,"extensions":{"ISIN":"US1234567890"}}]}"#;
        let response_bytes = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response_bytes: &'static [u8] =
            Box::leak(response_bytes.into_bytes().into_boxed_slice());
        let url = mock_server(response_bytes).await;
        let client = TestClientBuilder::new().token_list_url(&url).build().await;
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
        let first = &body["tokens"][0];
        assert_eq!(first["ISIN"], "US1234567890");
    }

    #[rocket::async_test]
    async fn test_get_tokens_filters_non_base_chain_tokens() {
        let body = r#"{"tokens":[{"chainId":8453,"address":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","name":"USD Coin","symbol":"USDC","decimals":6},{"chainId":1,"address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","name":"USD Coin","symbol":"USDC","decimals":6}]}"#;
        let response_bytes = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response_bytes: &'static [u8] =
            Box::leak(response_bytes.into_bytes().into_boxed_slice());
        let url = mock_server(response_bytes).await;
        let client = TestClientBuilder::new().token_list_url(&url).build().await;
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
        let tokens = body["tokens"].as_array().expect("tokens is an array");
        assert_eq!(tokens.len(), 1);
        assert_eq!(
            tokens[0]["address"],
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
        );
    }

    #[rocket::async_test]
    async fn test_get_tokens_returns_500_on_upstream_error() {
        let url = mock_server(
            b"HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        let client = TestClientBuilder::new().token_list_url(&url).build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("failed to retrieve token list"));
    }

    #[rocket::async_test]
    async fn test_get_tokens_returns_500_on_invalid_json() {
        let url = mock_server(
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\nnot-json!!!",
        )
        .await;
        let client = TestClientBuilder::new().token_list_url(&url).build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("failed to retrieve token list"));
    }

    #[rocket::async_test]
    async fn test_get_tokens_returns_500_on_fetch_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let client = TestClientBuilder::new()
            .token_list_url(format!("http://{addr}"))
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("failed to retrieve token list"));
    }
}
