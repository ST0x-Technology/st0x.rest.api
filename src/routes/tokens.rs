use crate::auth::AuthenticatedKey;
use crate::db::wrapped_donations as db_donations;
use crate::db::wrapped_rates as db_rates;
use crate::db::DbPool;
use crate::denomination::WrappedTokenIndex;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::SharedRaindexProvider;
use crate::types::common::TokenRef;
use crate::wrapped_rates::{self, SubgraphRateFetcher};
use alloy::primitives::Address;
use rain_orderbook_app_settings::token::TokenCfg;
use rain_orderbook_common::raindex_client::RaindexError;
use rocket::form::FromForm;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;
use utoipa::{IntoParams, ToSchema};

/// Caps how stale a snapshot may be before the endpoint forces a refresh.
/// 24 hours matches the user's tolerance for rate-change infrequency.
const EXCHANGE_RATE_MAX_AGE_SECS: i64 = 86_400;

/// Newtype around the sft-base subgraph endpoint string used as Rocket
/// managed state. Wrapped so we can index by type in `&State<...>` without
/// colliding with other `String` state.
#[derive(Debug, Clone)]
pub struct SftSubgraphUrl(pub String);

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
    sft_subgraph_url: &State<SftSubgraphUrl>,
) -> Result<Json<Vec<ExchangeRateEntry>>, ApiError> {
    async move {
        tracing::info!("exchange-rates request received");
        let wrapped: Vec<TokenCfg> = WrappedTokenIndex::load(shared_raindex.inner())
            .await?
            .into_values()
            .collect();
        tracing::info!(wrapped_count = wrapped.len(), "refreshing wrapped rates");

        let mut entries = Vec::with_capacity(wrapped.len());
        let fetcher = SubgraphRateFetcher::new(sft_subgraph_url.inner().0.clone());
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

/// Cap on `pageSize` to keep responses bounded. Mirrors the magnitude used
/// by the trades/orders endpoints.
const HISTORY_MAX_PAGE_SIZE: u32 = 500;
const HISTORY_DEFAULT_PAGE_SIZE: u32 = 50;

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeRateHistoryParams {
    /// Wrapped (`wt*`) token address whose history to fetch.
    #[field(name = "token")]
    #[param(example = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2")]
    pub token: String,
    #[field(name = "page")]
    #[param(example = 1)]
    pub page: Option<u32>,
    #[field(name = "pageSize")]
    #[param(example = 50)]
    pub page_size: Option<u32>,
    /// Inclusive lower-bound block number filter. When omitted, the
    /// response starts at the earliest recorded snapshot.
    #[field(name = "fromBlock")]
    pub from_block: Option<u64>,
    /// Inclusive upper-bound block number filter. When omitted, the
    /// response ends at the latest recorded event.
    #[field(name = "toBlock")]
    pub to_block: Option<u64>,
}

/// One row in the merged snapshot+donation feed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ExchangeRateHistoryEvent {
    /// Periodic snapshot row from `wrapped_exchange_rate_snapshots`. Today
    /// these are 1:1 for every live wrapper; they show the indexer's head
    /// block at the time we captured the rate.
    #[serde(rename_all = "camelCase")]
    Snapshot {
        #[schema(example = 12345678)]
        block_number: u64,
        #[schema(example = 1718452800)]
        block_timestamp: u64,
        #[schema(example = "1.0")]
        assets_per_share: String,
        captured_at: String,
    },
    /// Donation event detected by the wrapper scanner — an OARV transfer
    /// straight to the wrapper that bumped `assetsPerShare` without
    /// minting new wrapper shares. Currently the scanner is a stub
    /// (see `crate::wrapped_donations`); rows only appear once it lands.
    #[serde(rename_all = "camelCase")]
    Donation {
        #[schema(example = 12345700)]
        block_number: u64,
        #[schema(example = 1718453000)]
        block_timestamp: u64,
        #[schema(value_type = String, example = "0xabcdef...")]
        tx_hash: String,
        #[schema(value_type = String, example = "0xdef...")]
        donor: String,
        #[schema(example = "100.0")]
        asset_amount: String,
        #[schema(example = "1.05")]
        new_assets_per_share: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeRateHistoryPagination {
    #[schema(example = 1)]
    pub page: u32,
    #[schema(example = 50)]
    pub page_size: u32,
    #[schema(example = 73)]
    pub total_events: u64,
    #[schema(example = 2)]
    pub total_pages: u64,
    #[schema(example = true)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeRateHistoryResponse {
    pub share: TokenRef,
    pub asset: TokenRef,
    pub events: Vec<ExchangeRateHistoryEvent>,
    pub pagination: ExchangeRateHistoryPagination,
}

#[utoipa::path(
    get,
    path = "/v1/tokens/exchange-rates/history",
    tag = "Tokens",
    security(("basicAuth" = [])),
    params(ExchangeRateHistoryParams),
    responses(
        (status = 200, description = "Wrapped-token exchange rate history (snapshots + donations)", body = ExchangeRateHistoryResponse),
        (status = 400, description = "Bad request (invalid token / pagination)", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "Wrapped token not registered", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/exchange-rates/history?<params..>")]
pub async fn get_exchange_rate_history(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
    pool: &State<DbPool>,
    params: ExchangeRateHistoryParams,
) -> Result<Json<ExchangeRateHistoryResponse>, ApiError> {
    async move {
        tracing::info!(token = %params.token, "exchange-rates history request received");

        let token_addr: Address = params.token.parse().map_err(|e| {
            tracing::warn!(error = %e, token = %params.token, "invalid token address");
            ApiError::BadRequest("invalid token address".into())
        })?;

        let (page, page_size) = parse_history_pagination(params.page, params.page_size)?;

        // Reject tokens that aren't wrapped — the history endpoint is only
        // meaningful for ERC4626 wrappers. Going through the shared index
        // keeps the wrapped-token definition in one place.
        let index = WrappedTokenIndex::load(shared_raindex.inner()).await?;
        let token_cfg = index.into_map().remove(&token_addr).ok_or_else(|| {
            tracing::info!(token = %params.token, "token not a registered wrapped token");
            ApiError::NotFound("wrapped token not registered".into())
        })?;

        // Pull every event in the requested window first; pagination is
        // applied post-merge so snapshots and donations interleave
        // chronologically. The volumes are bounded (one snapshot per
        // refresh, donations are rare) so an in-memory merge stays cheap.
        let snapshots = db_rates::list_for_token_in_range(
            pool.inner(),
            token_addr,
            params.from_block,
            params.to_block,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "snapshot history lookup failed");
            ApiError::Internal("snapshot history lookup failed".into())
        })?;

        let donations = db_donations::list_for_wrapper(
            pool.inner(),
            token_addr,
            params.from_block,
            params.to_block,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "donation history lookup failed");
            ApiError::Internal("donation history lookup failed".into())
        })?;

        let (asset_addr, asset_symbol, asset_decimals) = derive_asset_ref(&snapshots);

        let merged = merge_events(snapshots, donations);
        let total_events = merged.len() as u64;
        let total_pages = if total_events == 0 {
            0
        } else {
            total_events.div_ceil(u64::from(page_size))
        };
        let start = ((page - 1) as usize).saturating_mul(page_size as usize);
        let end = start.saturating_add(page_size as usize).min(merged.len());
        let page_events = if start >= merged.len() {
            Vec::new()
        } else {
            merged[start..end].to_vec()
        };
        let has_more = (start + page_events.len()) < merged.len();

        Ok(Json(ExchangeRateHistoryResponse {
            share: TokenRef {
                address: token_cfg.address,
                symbol: token_cfg.symbol.clone().unwrap_or_default(),
                decimals: token_cfg.decimals.unwrap_or(0),
            },
            asset: TokenRef {
                address: asset_addr,
                symbol: asset_symbol,
                decimals: asset_decimals,
            },
            events: page_events,
            pagination: ExchangeRateHistoryPagination {
                page,
                page_size,
                total_events,
                total_pages,
                has_more,
            },
        }))
    }
    .instrument(span.0)
    .await
}

/// Defaults `page` to 1 and `page_size` to [`HISTORY_DEFAULT_PAGE_SIZE`].
/// Rejects zero or oversize pageSize early so the caller gets a clean 400
/// instead of a silently clamped response.
fn parse_history_pagination(
    page: Option<u32>,
    page_size: Option<u32>,
) -> Result<(u32, u32), ApiError> {
    let page = page.unwrap_or(1);
    if page == 0 {
        return Err(ApiError::BadRequest("page must be >= 1".into()));
    }
    let page_size = page_size.unwrap_or(HISTORY_DEFAULT_PAGE_SIZE);
    if page_size == 0 {
        return Err(ApiError::BadRequest("pageSize must be >= 1".into()));
    }
    if page_size > HISTORY_MAX_PAGE_SIZE {
        return Err(ApiError::BadRequest(format!(
            "pageSize must be <= {HISTORY_MAX_PAGE_SIZE}"
        )));
    }
    Ok((page, page_size))
}

/// Merge snapshots and donations into a single chronologically sorted
/// stream. Within the same block, snapshots sort before donations so the
/// rate value "before" any donation is always visible first.
fn merge_events(
    snapshots: Vec<db_rates::WrappedRateSnapshot>,
    donations: Vec<db_donations::DonationEventRow>,
) -> Vec<ExchangeRateHistoryEvent> {
    let mut events: Vec<(u64, u8, ExchangeRateHistoryEvent)> =
        Vec::with_capacity(snapshots.len() + donations.len());
    for s in snapshots {
        let block_number = u64::try_from(s.block_number).unwrap_or(0);
        events.push((
            block_number,
            0,
            ExchangeRateHistoryEvent::Snapshot {
                block_number,
                block_timestamp: u64::try_from(s.block_timestamp).unwrap_or(0),
                assets_per_share: s.assets_per_share,
                captured_at: s.captured_at,
            },
        ));
    }
    for d in donations {
        let block_number = u64::try_from(d.block_number).unwrap_or(0);
        events.push((
            block_number,
            1,
            ExchangeRateHistoryEvent::Donation {
                block_number,
                block_timestamp: u64::try_from(d.block_timestamp).unwrap_or(0),
                tx_hash: d.tx_hash,
                donor: d.donor_address,
                asset_amount: d.asset_amount,
                new_assets_per_share: d.new_assets_per_share,
            },
        ));
    }
    events.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    events.into_iter().map(|(_, _, ev)| ev).collect()
}

/// Pulls the underlying-asset metadata off the most recent snapshot in the
/// window. When no snapshot exists we return a zero address / empty symbol —
/// the response is still well-formed and the caller can tell the asset
/// metadata is unavailable.
fn derive_asset_ref(snapshots: &[db_rates::WrappedRateSnapshot]) -> (Address, String, u8) {
    let Some(latest) = snapshots.last() else {
        return (Address::ZERO, String::new(), 0);
    };
    let addr = latest
        .asset_address
        .parse::<Address>()
        .unwrap_or(Address::ZERO);
    let decimals = u8::try_from(latest.asset_decimals).unwrap_or(0);
    (addr, latest.asset_symbol.clone(), decimals)
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_tokens, get_exchange_rates, get_exchange_rate_history]
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

    /// Build a registry that registers a single wrapped token (wtMSTR)
    /// alongside USDC. Used by the exchange-rate-history tests below to
    /// give the route a recognised wrapper.
    async fn build_history_client() -> rocket::local::asynchronous::Client {
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
  "version": { "major": 1, "minor": 0, "patch": 0 },
  "tokens": [
    {
      "chainId": 8453,
      "address": "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2",
      "decimals": 18,
      "name": "Wrapped MSTR",
      "symbol": "wtMSTR",
      "extensions": { "category": "ST0x" }
    }
  ]
}"#;
        let registry_url = crate::test_helpers::mock_raindex_registry_url_with_settings_and_tokens(
            settings,
            remote_tokens,
        )
        .await;
        let config = crate::raindex::RaindexProvider::load(
            &registry_url,
            None,
            std::collections::HashMap::new(),
        )
        .await
        .expect("load raindex config");
        crate::test_helpers::TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await
    }

    async fn seed_history_fixtures(client: &rocket::local::asynchronous::Client) {
        use crate::db::wrapped_donations::{self as db_donations, NewDonationEvent};
        use crate::db::wrapped_rates::{self as db_rates, NewWrappedRateSnapshot};
        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        let wrapper: alloy::primitives::Address = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2"
            .parse()
            .unwrap();
        let asset: alloy::primitives::Address = "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe"
            .parse()
            .unwrap();

        // Snapshots at blocks 100 and 300.
        for (bn, ts, rate) in [
            (100u64, 1_700_000_000u64, "1.0"),
            (300, 1_700_002_000, "1.05"),
        ] {
            db_rates::insert_snapshot(
                pool,
                &NewWrappedRateSnapshot {
                    token_address: wrapper,
                    block_number: bn,
                    block_timestamp: ts,
                    assets_per_share: rate,
                    asset_address: asset,
                    asset_symbol: "tMSTR",
                    asset_decimals: 18,
                },
            )
            .await
            .unwrap();
        }

        // Donations at blocks 200 (between snapshots) and 300 (same block as
        // the second snapshot — snapshot should sort before donation).
        let donor: alloy::primitives::Address = "0x0000000000000000000000000000000000000abc"
            .parse()
            .unwrap();
        let tx1: alloy::primitives::B256 =
            "0x0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap();
        let tx2: alloy::primitives::B256 =
            "0x0000000000000000000000000000000000000000000000000000000000000002"
                .parse()
                .unwrap();
        db_donations::insert_event(
            pool,
            &NewDonationEvent {
                wrapper_address: wrapper,
                donor_address: donor,
                asset_amount: "10.0",
                block_number: 200,
                block_timestamp: 1_700_001_000,
                tx_hash: tx1,
                new_assets_per_share: "1.02",
            },
        )
        .await
        .unwrap();
        db_donations::insert_event(
            pool,
            &NewDonationEvent {
                wrapper_address: wrapper,
                donor_address: donor,
                asset_amount: "20.0",
                block_number: 300,
                block_timestamp: 1_700_002_000,
                tx_hash: tx2,
                new_assets_per_share: "1.08",
            },
        )
        .await
        .unwrap();
    }

    #[rocket::async_test]
    async fn test_exchange_rate_history_returns_404_for_unknown_token() {
        let client = build_history_client().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        // Address not in the registry.
        let response = client
            .get("/v1/tokens/exchange-rates/history?token=0x1111111111111111111111111111111111111111")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);
    }

    #[rocket::async_test]
    async fn test_exchange_rate_history_returns_400_for_invalid_token() {
        let client = build_history_client().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens/exchange-rates/history?token=not-a-hex-address")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn test_exchange_rate_history_merges_snapshots_and_donations_in_block_order() {
        let client = build_history_client().await;
        seed_history_fixtures(&client).await;

        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens/exchange-rates/history?token=0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();

        let events = body["events"].as_array().expect("events array");
        assert_eq!(events.len(), 4);
        // Block 100 snapshot, block 200 donation, block 300 snapshot (sorts
        // before donation in same block), block 300 donation.
        assert_eq!(events[0]["type"], "snapshot");
        assert_eq!(events[0]["blockNumber"], 100);
        assert_eq!(events[1]["type"], "donation");
        assert_eq!(events[1]["blockNumber"], 200);
        assert_eq!(events[2]["type"], "snapshot");
        assert_eq!(events[2]["blockNumber"], 300);
        assert_eq!(events[3]["type"], "donation");
        assert_eq!(events[3]["blockNumber"], 300);

        assert_eq!(body["pagination"]["totalEvents"], 4);
        assert_eq!(body["pagination"]["page"], 1);
        assert_eq!(body["pagination"]["hasMore"], false);
        assert_eq!(body["share"]["symbol"], "wtMSTR");
        assert_eq!(body["asset"]["symbol"], "tMSTR");
    }

    #[rocket::async_test]
    async fn test_exchange_rate_history_paginates_and_respects_block_filter() {
        let client = build_history_client().await;
        seed_history_fixtures(&client).await;

        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        // pageSize=2, page=1 → first two events (snapshot@100, donation@200).
        let response = client
            .get("/v1/tokens/exchange-rates/history?token=0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2&page=1&pageSize=2")
            .header(Header::new("Authorization", header.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["blockNumber"], 100);
        assert_eq!(events[1]["blockNumber"], 200);
        assert_eq!(body["pagination"]["totalPages"], 2);
        assert_eq!(body["pagination"]["hasMore"], true);

        // fromBlock filter strips the snapshot@100 row.
        let response = client
            .get("/v1/tokens/exchange-rates/history?token=0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2&fromBlock=200")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let events = body["events"].as_array().unwrap();
        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .all(|e| e["blockNumber"].as_u64().unwrap() >= 200));
    }
}
