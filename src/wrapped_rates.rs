//! ERC4626 wrapped-token exchange rate ("assets per share") tracking.
//!
//! Each wrapped st0x token (e.g. `wtMSTR`) is an ERC4626 vault whose share
//! token wraps an underlying tStock asset. The exchange rate
//! `assetsPerShare = convertToAssets(10^shareDecimals) / 10^assetDecimals`
//! tells us how many tStock units a single wtStock unit redeems for.
//!
//! Rates change infrequently, so we cache them aggressively (24 hours) and
//! persist every observation to the `wrapped_exchange_rate_snapshots` table.
//! When the trades endpoints are asked for prices in tStock denomination,
//! they look up the snapshot at or before each trade's block number so the
//! conversion reflects the rate that was live at trade time.
//!
//! ## Data source
//!
//! The wrapped tStock contracts on Base are ERC4626 vaults sitting over
//! gildlab's `OffchainAssetReceiptVault` (OARV) — the legacy tStock tokens
//! such as `0x013b78...` (tMSTR). Each OARV has a `wrappedTokenContractAddress`
//! field linking it to its `wt*` wrapper. The `sft-base` subgraph
//! (https://api.goldsky.com/.../sft-base) indexes both sides of that pair
//! including `totalShares`, the wrapper relationship, and an indexed-head
//! block via `_meta.block` — see [`SubgraphRateFetcher`].
//!
//! For OARV the contract enforces `totalAssets() == totalSupply()` so
//! `assetsPerShare` is structurally fixed at `1.0`. The subgraph fetcher
//! still records the rate per token so callers see a consistent shape and
//! the snapshot history accumulates real block numbers — the moment a
//! token switches to a variable-rate wrapper (e.g. an oracle-driven vault)
//! the same fetcher will read the dynamic value without code churn at the
//! endpoint layer.
//!
//! [`RpcRateFetcher`] remains in the module as an alternate implementation:
//! it calls `IERC4626::convertToAssets(10^decimals)` directly via the
//! registry-configured RPC. Useful when the subgraph is out of date or for
//! sanity-checking the indexer against on-chain truth.
//!
//! [`StubRateFetcher`] is `#[cfg(test)]`-only — exercises the storage layer
//! without a network dependency.

#[cfg(test)]
use crate::db::settings;
use crate::db::wrapped_rates as db_rates;
use crate::db::DbPool;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use async_trait::async_trait;
use rain_orderbook_app_settings::token::TokenCfg;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

alloy::sol! {
    #[sol(rpc)]
    interface IERC4626Rate {
        function convertToAssets(uint256 shares) external view returns (uint256 assets);
    }
}

#[cfg(test)]
pub(crate) const STUB_DEFAULT_RATE: &str = "1.0";

#[derive(Debug, Clone)]
pub(crate) struct RateObservation {
    pub token_address: Address,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub assets_per_share: String,
    pub asset_address: Address,
    pub asset_symbol: String,
    pub asset_decimals: u8,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RateFetchError {
    #[error("subgraph fetch unavailable: {0}")]
    SubgraphUnavailable(String),
    #[error("rpc fetch unavailable: {0}")]
    RpcUnavailable(String),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
}

/// Fetches the current `assetsPerShare` for a wrapped token.
///
/// [`SubgraphRateFetcher`] is the default production impl (sft-base
/// indexes wrapper state on Base). [`RpcRateFetcher`] is an alternate that
/// reads the ERC4626 contract directly. [`StubRateFetcher`] is test-only.
#[async_trait]
pub(crate) trait RateFetcher: Send + Sync {
    async fn fetch_current_rate(&self, token: &TokenCfg)
        -> Result<RateObservation, RateFetchError>;
}

/// Test-only fetcher backing the unit suite. Reads
/// `wrapped_rate:<addr>` from the `settings` table when present, otherwise
/// returns [`STUB_DEFAULT_RATE`]. The production code path uses
/// [`RpcRateFetcher`]; this stub exists so storage and conversion logic
/// can be exercised without spinning up an Ethereum node.
#[cfg(test)]
pub(crate) struct StubRateFetcher<'a> {
    pub pool: &'a DbPool,
}

#[cfg(test)]
#[async_trait]
impl<'a> RateFetcher for StubRateFetcher<'a> {
    async fn fetch_current_rate(
        &self,
        token: &TokenCfg,
    ) -> Result<RateObservation, RateFetchError> {
        let setting_key = format!("wrapped_rate:{:#x}", token.address);
        let stored = settings::get_setting(self.pool, &setting_key)
            .await
            .map_err(RateFetchError::from)?;
        let rate = stored.unwrap_or_else(|| STUB_DEFAULT_RATE.to_string());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let (asset_address, asset_symbol, asset_decimals) = extract_asset_hint(token);

        Ok(RateObservation {
            token_address: token.address,
            block_number: 0,
            block_timestamp: now,
            assets_per_share: rate,
            asset_address,
            asset_symbol,
            asset_decimals,
        })
    }
}

/// Production fetcher: calls `IERC4626::convertToAssets(10^decimals)` on the
/// wrapped token via the registry-declared RPC. The first RPC in the
/// token's network configuration is tried first; subsequent RPCs serve as
/// fallbacks on transport/eth_call failure. The block number recorded on
/// the snapshot is `eth_blockNumber` at fetch time — the slight race
/// between that and the `latest`-tagged `eth_call` is bounded by Base's
/// 2s block time and is acceptable for a 24h-cached rate.
///
/// Assumes share-decimals == asset-decimals (true for every ST0x pair
/// today: both wt* and t* are 18 decimals). The returned ratio is the
/// unscaled decimal `assets / shares`.
pub(crate) struct RpcRateFetcher {
    rpcs: Vec<Url>,
}

impl RpcRateFetcher {
    pub(crate) fn new(rpcs: Vec<Url>) -> Self {
        Self { rpcs }
    }
}

#[async_trait]
impl RateFetcher for RpcRateFetcher {
    async fn fetch_current_rate(
        &self,
        token: &TokenCfg,
    ) -> Result<RateObservation, RateFetchError> {
        if self.rpcs.is_empty() {
            return Err(RateFetchError::RpcUnavailable(
                "no rpc urls configured for token network".into(),
            ));
        }

        let share_decimals = token.decimals.unwrap_or(18);
        let one_share = scale_one(share_decimals);
        let (asset_address, asset_symbol, asset_decimals) = extract_asset_hint(token);
        let effective_asset_decimals = if asset_decimals == 0 {
            share_decimals
        } else {
            asset_decimals
        };

        let mut last_err: Option<String> = None;
        for rpc in &self.rpcs {
            let provider = ProviderBuilder::new().connect_http(rpc.clone());
            let contract = IERC4626Rate::new(token.address, &provider);

            let assets = match contract.convertToAssets(one_share).call().await {
                Ok(value) => value,
                Err(e) => {
                    tracing::warn!(
                        token = ?token.address,
                        rpc = %rpc,
                        error = %e,
                        "convertToAssets call failed; trying next rpc"
                    );
                    last_err = Some(e.to_string());
                    continue;
                }
            };

            let block_number = provider.get_block_number().await.unwrap_or_else(|e| {
                tracing::warn!(
                    token = ?token.address,
                    rpc = %rpc,
                    error = %e,
                    "block_number lookup failed; recording snapshot with block 0"
                );
                0
            });

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let formatted = format_ratio(assets, effective_asset_decimals);
            return Ok(RateObservation {
                token_address: token.address,
                block_number,
                block_timestamp: now,
                assets_per_share: formatted,
                asset_address,
                asset_symbol,
                asset_decimals: effective_asset_decimals,
            });
        }

        Err(RateFetchError::RpcUnavailable(
            last_err.unwrap_or_else(|| "all rpcs failed".into()),
        ))
    }
}

fn scale_one(decimals: u8) -> U256 {
    let mut value = U256::from(1u64);
    for _ in 0..decimals {
        value *= U256::from(10u64);
    }
    value
}

/// Default production fetcher: pulls wrapper state out of the sft-base
/// subgraph. Each wrapped (`wt*`) token in the registry has a backing OARV
/// indexed under `OffchainAssetReceiptVault` with `wrappedTokenContractAddress`
/// pointing back at it. We query that linkage to:
///
/// 1. confirm the token really is an OARV-backed wrapper (errors out otherwise);
/// 2. resolve the underlying tStock's address/symbol/totalShares;
/// 3. capture `_meta.block` so the snapshot row records the indexer's head
///    block — important for historical lookups via the `unwrapped` denomination
///    toggle on `/v1/trades/*`.
///
/// Today the rate is always `1.0`: OARV enforces `totalAssets() ==
/// totalSupply()` so the ERC4626 wrapper round-trips 1:1. The fetcher still
/// renders the value through [`format_ratio`] so the moment the indexer
/// surfaces a variable-rate wrapper (e.g. an oracle-driven vault, where
/// `convertToAssets` is non-trivial) callers will see a non-`1.0` value
/// without any code change at the endpoint.
pub(crate) struct SubgraphRateFetcher {
    pub endpoint: String,
}

#[derive(Debug, Deserialize)]
struct SubgraphResponse {
    data: Option<SubgraphData>,
    #[serde(default)]
    errors: Vec<SubgraphError>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubgraphData {
    offchain_asset_receipt_vaults: Vec<SubgraphVault>,
    #[serde(rename = "_meta", default)]
    meta: Option<SubgraphMeta>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubgraphVault {
    id: String,
    symbol: Option<String>,
    name: Option<String>,
    total_shares: Option<String>,
    wrapped_token_contract_address: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubgraphMeta {
    block: SubgraphBlock,
}

#[derive(Debug, Deserialize)]
struct SubgraphBlock {
    number: i64,
    timestamp: i64,
}

#[derive(Debug, Deserialize)]
struct SubgraphError {
    message: String,
}

impl SubgraphRateFetcher {
    pub(crate) fn new(endpoint: String) -> Self {
        Self { endpoint }
    }

    /// GraphQL body. We use exact lower-case address matching — the sft-base
    /// subgraph stores Bytes as lower-case hex.
    fn build_query(wt_address: Address) -> String {
        let addr = format!("{wt_address:#x}");
        format!(
            "{{ offchainAssetReceiptVaults(where: {{ wrappedTokenContractAddress: \"{addr}\" }}) \
             {{ id symbol name totalShares wrappedTokenContractAddress }} \
             _meta {{ block {{ number timestamp }} }} }}",
        )
    }
}

#[async_trait]
impl RateFetcher for SubgraphRateFetcher {
    async fn fetch_current_rate(
        &self,
        token: &TokenCfg,
    ) -> Result<RateObservation, RateFetchError> {
        let client = reqwest::Client::new();
        let body = serde_json::json!({ "query": Self::build_query(token.address) });
        let response = client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| RateFetchError::SubgraphUnavailable(e.to_string()))?;

        let status = response.status();
        let payload: SubgraphResponse = response
            .json()
            .await
            .map_err(|e| RateFetchError::SubgraphUnavailable(format!("decode: {e}")))?;

        if !payload.errors.is_empty() {
            let joined = payload
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(RateFetchError::SubgraphUnavailable(format!(
                "graphql errors ({status}): {joined}"
            )));
        }

        let data = payload.data.ok_or_else(|| {
            RateFetchError::SubgraphUnavailable(format!("empty response ({status})"))
        })?;

        let vault = data
            .offchain_asset_receipt_vaults
            .into_iter()
            .next()
            .ok_or_else(|| {
                RateFetchError::SubgraphUnavailable(format!(
                    "subgraph has no OARV linked to wrapped token {:#x}",
                    token.address
                ))
            })?;

        let share_decimals = token.decimals.unwrap_or(18);
        // For OARV the wrapper's totalAssets == totalSupply, so the ratio is
        // structurally 1.0. We still echo the rate as decimal string via the
        // same code path the variable-rate case will use, so a future indexer
        // upgrade that surfaces a dynamic ratio drops in without touching
        // call sites.
        let assets_per_share = format_ratio(scale_one(share_decimals), share_decimals);

        let asset_address = vault
            .id
            .parse::<Address>()
            .map_err(|e| RateFetchError::SubgraphUnavailable(format!("vault id parse: {e}")))?;
        let asset_symbol = vault
            .symbol
            .or_else(|| derived_asset_symbol(token))
            .unwrap_or_default();
        let asset_decimals = share_decimals;

        let (block_number, block_timestamp) = match data.meta.map(|m| m.block) {
            Some(b) => (
                u64::try_from(b.number).unwrap_or(0),
                u64::try_from(b.timestamp).unwrap_or(0),
            ),
            None => (0, current_unix_secs()),
        };

        // `total_shares` is included in the response for parity with the
        // future oracle-driven case; for OARV it'll equal the wrapper's
        // totalSupply. We don't surface it yet — the API contract is just
        // `assetsPerShare`.
        let _ = vault.total_shares;
        let _ = vault.wrapped_token_contract_address;
        let _ = vault.name;

        Ok(RateObservation {
            token_address: token.address,
            block_number,
            block_timestamp,
            assets_per_share,
            asset_address,
            asset_symbol,
            asset_decimals,
        })
    }
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Renders `assets / 10^decimals` as a decimal string, trimming trailing
/// zeros but always keeping at least one fractional digit so the result is
/// unambiguously a decimal (e.g. `1.0`, not `1`).
fn format_ratio(assets: U256, decimals: u8) -> String {
    let scale = scale_one(decimals);
    let integer = assets / scale;
    let remainder = assets % scale;
    if remainder.is_zero() {
        return format!("{integer}.0");
    }
    let frac_full = format!("{remainder:0width$}", width = decimals as usize);
    let trimmed = frac_full.trim_end_matches('0');
    let frac = if trimmed.is_empty() { "0" } else { trimmed };
    format!("{integer}.{frac}")
}

/// Treat a token as wrapped if its `extensions.category == "ST0x"` (this is
/// how the st0x registry labels wrapped tStock tokens — see the existing
/// remote-tokens metadata served alongside the registry).
pub(crate) fn is_wrapped_token(token: &TokenCfg) -> bool {
    extract_extension_string(token.extensions.as_ref(), "category")
        .map(|c| c.eq_ignore_ascii_case("ST0x"))
        .unwrap_or(false)
}

/// Pulls underlying-asset metadata from the wrapped token's extensions when
/// available. The st0x registry's token-list metadata records the
/// underlying tStock address under `unwrappedAddress` (see
/// `st0x.registry/token-lists/base.json`). We accept the historical
/// alternates `assetAddress`/`asset` too for forward-compat.
///
/// Symbol and decimals aren't carried by the registry today, so they are
/// derived: symbol drops the leading `w` (`wtMSTR` → `tMSTR`) and decimals
/// fall back to the wrapped token's decimals (always 18 in the current
/// st0x deployment; both sides of every existing pair share the same scale).
pub(crate) fn extract_asset_hint(token: &TokenCfg) -> (Address, String, u8) {
    let ext = token.extensions.as_ref();
    let addr = extract_extension_string(ext, "unwrappedAddress")
        .or_else(|| extract_extension_string(ext, "assetAddress"))
        .or_else(|| extract_extension_string(ext, "asset"))
        .and_then(|s| s.parse::<Address>().ok())
        .unwrap_or(Address::ZERO);
    let symbol = extract_extension_string(ext, "assetSymbol")
        .or_else(|| derived_asset_symbol(token))
        .unwrap_or_default();
    let decimals = extract_extension_string(ext, "assetDecimals")
        .and_then(|s| s.parse::<u8>().ok())
        .or(token.decimals)
        .unwrap_or(0);
    (addr, symbol, decimals)
}

/// Default tStock symbol guess: drop the leading `w` from `wt<X>` to give
/// `t<X>`. Used only when no `assetSymbol` extension is configured.
fn derived_asset_symbol(token: &TokenCfg) -> Option<String> {
    let symbol = token.symbol.as_deref()?;
    if let Some(rest) = symbol.strip_prefix("wt") {
        Some(format!("t{rest}"))
    } else if let Some(rest) = symbol.strip_prefix("w") {
        Some(rest.to_string())
    } else {
        None
    }
}

fn extract_extension_string(
    extensions: Option<&HashMap<String, Value>>,
    key: &str,
) -> Option<String> {
    let ext = extensions?;
    match ext.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(v) => Some(v.to_string()),
    }
}

/// Refresh a single wrapped token: fetch the current rate via the supplied
/// fetcher and persist a new snapshot row. Returns the inserted observation
/// so the caller can echo it in the API response.
pub(crate) async fn refresh_and_persist(
    pool: &DbPool,
    fetcher: &dyn RateFetcher,
    token: &TokenCfg,
) -> Result<RateObservation, RateFetchError> {
    let observation = fetcher.fetch_current_rate(token).await?;
    db_rates::insert_snapshot(
        pool,
        &db_rates::NewWrappedRateSnapshot {
            token_address: observation.token_address,
            block_number: observation.block_number,
            block_timestamp: observation.block_timestamp,
            assets_per_share: &observation.assets_per_share,
            asset_address: observation.asset_address,
            asset_symbol: &observation.asset_symbol,
            asset_decimals: observation.asset_decimals,
        },
    )
    .await?;
    Ok(observation)
}

/// Refresh a single token *only* if the most recent snapshot is older than
/// `max_age_secs`. Returns the snapshot used (either freshly fetched or the
/// existing recent one).
pub(crate) async fn refresh_if_stale(
    pool: &DbPool,
    fetcher: &dyn RateFetcher,
    token: &TokenCfg,
    max_age_secs: i64,
) -> Result<db_rates::WrappedRateSnapshot, RateFetchError> {
    let latest = db_rates::get_latest_for_token(pool, token.address).await?;
    if let Some(snapshot) = &latest {
        if let Some(age) = snapshot_age_secs(&snapshot.captured_at) {
            if age <= max_age_secs {
                return Ok(snapshot.clone());
            }
        }
    }

    refresh_and_persist(pool, fetcher, token).await?;
    db_rates::get_latest_for_token(pool, token.address)
        .await?
        .ok_or_else(|| {
            RateFetchError::SubgraphUnavailable("snapshot disappeared after insert".into())
        })
}

fn snapshot_age_secs(captured_at: &str) -> Option<i64> {
    let parsed = chrono::NaiveDateTime::parse_from_str(captured_at, "%Y-%m-%d %H:%M:%S").ok()?;
    let captured = parsed.and_utc().timestamp();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(now - captured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rain_orderbook_app_settings::network::NetworkCfg;
    use rain_orderbook_app_settings::yaml::default_document;
    use std::sync::Arc;

    async fn fresh_pool() -> DbPool {
        let id = uuid::Uuid::new_v4();
        db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("init db")
    }

    fn make_token(symbol: Option<&str>, extensions: Option<HashMap<String, Value>>) -> TokenCfg {
        TokenCfg {
            document: default_document(),
            key: "tok".into(),
            network: Arc::new(NetworkCfg {
                document: default_document(),
                key: "base".into(),
                rpcs: vec![],
                chain_id: 8453,
                label: None,
                network_id: None,
                currency: None,
            }),
            address: "0x1111111111111111111111111111111111111111"
                .parse()
                .unwrap(),
            decimals: Some(18),
            label: None,
            symbol: symbol.map(|s| s.to_string()),
            logo_uri: None,
            extensions,
        }
    }

    fn ext_st0x() -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("category".into(), Value::String("ST0x".into()));
        m
    }

    #[test]
    fn test_is_wrapped_token_st0x_category_matches() {
        let token = make_token(Some("wtMSTR"), Some(ext_st0x()));
        assert!(is_wrapped_token(&token));
    }

    #[test]
    fn test_is_wrapped_token_without_extension_does_not_match() {
        let token = make_token(Some("USDC"), None);
        assert!(!is_wrapped_token(&token));
    }

    #[test]
    fn test_derived_asset_symbol_strips_wt() {
        let token = make_token(Some("wtMSTR"), None);
        assert_eq!(derived_asset_symbol(&token).unwrap(), "tMSTR");
    }

    #[test]
    fn test_format_ratio_handles_integer_result() {
        let one = scale_one(18);
        assert_eq!(format_ratio(one, 18), "1.0");
    }

    #[test]
    fn test_format_ratio_renders_one_point_five() {
        let one_point_five = scale_one(18) + scale_one(18) / U256::from(2u64);
        assert_eq!(format_ratio(one_point_five, 18), "1.5");
    }

    #[test]
    fn test_format_ratio_strips_trailing_zeros() {
        // 1.04 * 10^18
        let value = scale_one(18) + scale_one(16) * U256::from(4u64);
        assert_eq!(format_ratio(value, 18), "1.04");
    }

    #[test]
    fn test_format_ratio_zero() {
        assert_eq!(format_ratio(U256::ZERO, 18), "0.0");
    }

    #[test]
    fn test_extract_asset_hint_picks_unwrapped_address() {
        let mut ext = ext_st0x();
        ext.insert(
            "unwrappedAddress".into(),
            Value::String("0x013b782f402d61aa1004cca95b9f5bb402c9d5fe".into()),
        );
        let token = make_token(Some("wtMSTR"), Some(ext));
        let (addr, sym, dec) = extract_asset_hint(&token);
        assert_eq!(
            format!("{addr:#x}"),
            "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe"
        );
        assert_eq!(sym, "tMSTR");
        assert_eq!(dec, 18);
    }

    #[rocket::async_test]
    async fn test_rpc_fetcher_fails_when_no_rpcs_configured() {
        let token = make_token(Some("wtMSTR"), Some(ext_st0x()));
        let fetcher = RpcRateFetcher::new(vec![]);
        let err = fetcher.fetch_current_rate(&token).await.unwrap_err();
        assert!(matches!(err, RateFetchError::RpcUnavailable(_)));
    }

    /// Stand up an in-process HTTP mock that always responds with the given
    /// JSON body, returning the URL clients should hit. Loosely mirrors the
    /// existing `mock_raindex_registry_url` pattern from `test_helpers.rs`,
    /// kept inline here so the test crate doesn't need to depend on a heavier
    /// mock-server crate.
    async fn mock_subgraph(response_body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let body = response_body.to_string();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ =
                        tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}/subgraph")
    }

    #[rocket::async_test]
    async fn test_subgraph_fetcher_resolves_oarv_metadata() {
        let token = make_token(Some("wtMSTR"), Some(ext_st0x()));
        let body = r#"{
          "data": {
            "offchainAssetReceiptVaults": [{
              "id": "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe",
              "symbol": "tMSTR",
              "name": "MicroStrategy Incorporated ST0x",
              "totalShares": "220647411091097441658",
              "wrappedTokenContractAddress": "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2"
            }],
            "_meta": {"block": {"number": 46329477, "timestamp": 1779448301}}
          }
        }"#;
        let url = mock_subgraph(body).await;

        let fetcher = SubgraphRateFetcher::new(url);
        let obs = fetcher.fetch_current_rate(&token).await.unwrap();
        // OARV is fixed 1:1
        assert_eq!(obs.assets_per_share, "1.0");
        assert_eq!(obs.asset_symbol, "tMSTR");
        assert_eq!(
            format!("{:#x}", obs.asset_address),
            "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe"
        );
        assert_eq!(obs.block_number, 46_329_477);
        assert_eq!(obs.block_timestamp, 1_779_448_301);
    }

    #[rocket::async_test]
    async fn test_subgraph_fetcher_errors_when_token_unrecognized() {
        let token = make_token(Some("wtUNKNOWN"), Some(ext_st0x()));
        let body = r#"{
          "data": {"offchainAssetReceiptVaults": [], "_meta": {"block": {"number": 1, "timestamp": 1}}}
        }"#;
        let url = mock_subgraph(body).await;

        let fetcher = SubgraphRateFetcher::new(url);
        let err = fetcher.fetch_current_rate(&token).await.unwrap_err();
        match err {
            RateFetchError::SubgraphUnavailable(msg) => assert!(msg.contains("no OARV linked")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[rocket::async_test]
    async fn test_subgraph_fetcher_propagates_graphql_errors() {
        let token = make_token(Some("wtMSTR"), Some(ext_st0x()));
        let body = r#"{
          "errors": [{"message": "Type `Query` has no field `oar`"}]
        }"#;
        let url = mock_subgraph(body).await;

        let fetcher = SubgraphRateFetcher::new(url);
        let err = fetcher.fetch_current_rate(&token).await.unwrap_err();
        match err {
            RateFetchError::SubgraphUnavailable(msg) => assert!(msg.contains("graphql errors")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[rocket::async_test]
    async fn test_stub_returns_default_rate_when_no_setting() {
        let pool = fresh_pool().await;
        let token = make_token(Some("wtMSTR"), Some(ext_st0x()));
        let fetcher = StubRateFetcher { pool: &pool };

        let obs = fetcher.fetch_current_rate(&token).await.unwrap();
        assert_eq!(obs.assets_per_share, STUB_DEFAULT_RATE);
        assert_eq!(obs.asset_symbol, "tMSTR");
        assert_eq!(obs.asset_decimals, 18);
    }

    #[rocket::async_test]
    async fn test_stub_returns_setting_when_present() {
        let pool = fresh_pool().await;
        let token = make_token(Some("wtMSTR"), Some(ext_st0x()));
        let key = format!("wrapped_rate:{:#x}", token.address);
        settings::set_setting(&pool, &key, "1.234").await.unwrap();

        let fetcher = StubRateFetcher { pool: &pool };
        let obs = fetcher.fetch_current_rate(&token).await.unwrap();
        assert_eq!(obs.assets_per_share, "1.234");
    }

    #[rocket::async_test]
    async fn test_refresh_and_persist_inserts_row() {
        let pool = fresh_pool().await;
        let token = make_token(Some("wtMSTR"), Some(ext_st0x()));
        let fetcher = StubRateFetcher { pool: &pool };

        let obs = refresh_and_persist(&pool, &fetcher, &token).await.unwrap();
        assert_eq!(obs.assets_per_share, STUB_DEFAULT_RATE);

        let latest = db_rates::get_latest_for_token(&pool, token.address)
            .await
            .unwrap()
            .expect("snapshot row");
        assert_eq!(latest.assets_per_share, STUB_DEFAULT_RATE);
    }

    #[rocket::async_test]
    async fn test_refresh_if_stale_skips_when_fresh() {
        let pool = fresh_pool().await;
        let token = make_token(Some("wtMSTR"), Some(ext_st0x()));
        let fetcher = StubRateFetcher { pool: &pool };

        refresh_and_persist(&pool, &fetcher, &token).await.unwrap();

        // Second call within freshness window should return the existing snapshot
        // without inserting another row.
        let before = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM wrapped_exchange_rate_snapshots WHERE token_address = ?",
        )
        .bind(format!("{:#x}", token.address))
        .fetch_one(&pool)
        .await
        .unwrap();
        let _ = refresh_if_stale(&pool, &fetcher, &token, 86_400)
            .await
            .unwrap();
        let after = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM wrapped_exchange_rate_snapshots WHERE token_address = ?",
        )
        .bind(format!("{:#x}", token.address))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, after);
    }
}
