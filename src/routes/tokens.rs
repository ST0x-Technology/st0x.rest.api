use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::SharedRaindexProvider;
use crate::types::tokens::{RemoteTokenList, TokenInfo, TokenListResponse};
use rocket::serde::json::Json;
use rocket::{Route, State};
use std::time::Duration;
use tracing::Instrument;

const TARGET_CHAIN_ID: u32 = crate::CHAIN_ID;
const TOKEN_LIST_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, thiserror::Error)]
enum TokenError {
    #[error("failed to fetch token list: {0}")]
    Fetch(reqwest::Error),
    #[error("failed to deserialize token list: {0}")]
    Deserialize(reqwest::Error),
    #[error("token list returned non-200 status: {0}")]
    BadStatus(reqwest::StatusCode),
    #[error("no token list URL configured in registry")]
    NoTokenListUrl,
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
    shared_raindex: &State<SharedRaindexProvider>,
) -> Result<Json<TokenListResponse>, ApiError> {
    let raindex = shared_raindex.read().await;
    let urls = raindex.get_remote_token_urls()?;
    let url = urls.first().ok_or(TokenError::NoTokenListUrl)?.to_string();
    drop(raindex);

    async move {
        tracing::info!("request received");

        tracing::info!(url = %url, timeout_secs = TOKEN_LIST_TIMEOUT_SECS, "fetching token list");

        let response = reqwest::Client::new()
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
    use crate::test_helpers::{
        basic_auth_header, mock_raindex_registry_url, seed_api_key, TestClientBuilder,
    };
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

    async fn build_client_with_token_url(token_url: &str) -> rocket::local::asynchronous::Client {
        let registry_url = mock_raindex_registry_url(Some(token_url)).await;
        TestClientBuilder::new()
            .raindex_registry_url(registry_url)
            .build()
            .await
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
        let client = build_client_with_token_url(&url).await;
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
        let client = build_client_with_token_url(&url).await;
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
        let client = build_client_with_token_url(&url).await;
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
        let client = build_client_with_token_url(&url).await;
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
        let client = build_client_with_token_url(&url).await;
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
        let client = build_client_with_token_url(&url).await;
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
        let url = format!("http://{addr}");
        let client = build_client_with_token_url(&url).await;
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
