use crate::auth::AuthenticatedKey;
use crate::db::wrapped_rates as db_rates;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::SharedRaindexProvider;
use crate::types::common::TokenRef;
use crate::wrapped_rates::{self, StubRateFetcher};
use alloy::primitives::Address;
use rain_orderbook_app_settings::token::TokenCfg;
use rain_orderbook_common::raindex_client::RaindexError;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;
use utoipa::ToSchema;

/// Caps how stale a snapshot may be before the endpoint forces a refresh.
/// 24 hours matches the user's tolerance for rate-change infrequency.
const EXCHANGE_RATE_MAX_AGE_SECS: i64 = 86_400;

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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeRateEntry {
    /// The wrapped ERC4626 share token address.
    pub share: TokenRef,
    /// Underlying tStock asset metadata. When the registry doesn't include
    /// `assetAddress`/`assetSymbol`/`assetDecimals` extensions for the
    /// wrapped token, address is zero and symbol is a best-effort derivation
    /// of the wrapped symbol (e.g. `wtMSTR` → `tMSTR`).
    pub asset: TokenRef,
    /// Decimal string. `1.0` means one wrapped share redeems for one
    /// underlying tStock unit. Greater than `1.0` means the wrapper has
    /// accumulated underlying assets (positive yield).
    #[schema(example = "1.0")]
    pub assets_per_share: String,
    /// Block at which the snapshot was observed. Zero indicates the stub
    /// fetcher recorded the snapshot without consulting a chain head (i.e.
    /// no real RPC/subgraph call has happened yet).
    #[schema(example = 12345678)]
    pub block_number: u64,
    /// Unix seconds of the observation block, or the capture time when
    /// `blockNumber == 0`.
    #[schema(example = 1718452800)]
    pub block_timestamp: u64,
    /// ISO-8601 timestamp recording when the API persisted this snapshot.
    pub captured_at: String,
}

#[utoipa::path(
    get,
    path = "/v1/tokens/exchange-rates",
    tag = "Tokens",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "Current wrapped-token exchange rates", body = Vec<ExchangeRateEntry>),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/exchange-rates")]
pub async fn get_exchange_rates(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
    pool: &State<DbPool>,
) -> Result<Json<Vec<ExchangeRateEntry>>, ApiError> {
    async move {
        tracing::info!("exchange-rates request received");
        let raindex = shared_raindex.read().await;
        let tokens = raindex
            .client()
            .get_all_tokens()
            .map_err(|e: RaindexError| {
                tracing::error!(error = %e, "failed to get tokens from raindex");
                ApiError::Internal("failed to retrieve token list".into())
            })?;

        let wrapped: Vec<TokenCfg> = tokens
            .into_values()
            .filter(wrapped_rates::is_wrapped_token)
            .collect();
        tracing::info!(wrapped_count = wrapped.len(), "refreshing wrapped rates");

        let fetcher = StubRateFetcher { pool: pool.inner() };
        let mut entries = Vec::with_capacity(wrapped.len());
        for token in &wrapped {
            let snapshot = wrapped_rates::refresh_if_stale(
                pool.inner(),
                &fetcher,
                token,
                EXCHANGE_RATE_MAX_AGE_SECS,
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, token = ?token.address, "failed to refresh wrapped rate");
                ApiError::Internal("failed to fetch wrapped exchange rate".into())
            })?;
            entries.push(snapshot_to_entry(token, snapshot));
        }

        Ok(Json(entries))
    }
    .instrument(span.0)
    .await
}

fn snapshot_to_entry(
    token: &TokenCfg,
    snapshot: db_rates::WrappedRateSnapshot,
) -> ExchangeRateEntry {
    let asset_addr = snapshot
        .asset_address
        .parse::<Address>()
        .unwrap_or(Address::ZERO);
    ExchangeRateEntry {
        share: TokenRef {
            address: token.address,
            symbol: token.symbol.clone().unwrap_or_default(),
            decimals: token.decimals.unwrap_or(0),
        },
        asset: TokenRef {
            address: asset_addr,
            symbol: snapshot.asset_symbol,
            decimals: u8::try_from(snapshot.asset_decimals).unwrap_or(0),
        },
        assets_per_share: snapshot.assets_per_share,
        block_number: u64::try_from(snapshot.block_number).unwrap_or(0),
        block_timestamp: u64::try_from(snapshot.block_timestamp).unwrap_or(0),
        captured_at: snapshot.captured_at,
    }
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_tokens, get_exchange_rates]
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
        let settings = r#"version: 4
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
deployers:
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
        let config = crate::raindex::RaindexProvider::load(
            &registry_url,
            None,
            std::collections::HashMap::new(),
        )
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
        let settings = r#"version: 4
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
deployers:
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
        let config = crate::raindex::RaindexProvider::load(
            &registry_url,
            None,
            std::collections::HashMap::new(),
        )
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
