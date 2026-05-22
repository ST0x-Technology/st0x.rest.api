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
//! The rain.orderbook subgraph only indexes ERC20 metadata (address, name,
//! symbol, decimals) for tokens that appear in vaults — it does not track
//! ERC4626 share/asset accounting. The st0x.oracle subgraph indexes oracle
//! adapter mappings, not vault state. There is no dedicated wrapper
//! subgraph. The only authoritative source for the current `assetsPerShare`
//! is the ERC4626 contract itself, via `convertToAssets(10^decimals)`.
//!
//! [`RpcRateFetcher`] is the default production fetcher: it dials the
//! token's registry-configured RPC, makes the view call, and records the
//! observation with the chain head block number so historical lookups work.
//! [`StubRateFetcher`] remains in this module solely so unit tests can
//! exercise the storage layer without spinning up an RPC.

#[cfg(test)]
use crate::db::settings;
use crate::db::wrapped_rates as db_rates;
use crate::db::DbPool;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use async_trait::async_trait;
use rain_orderbook_app_settings::token::TokenCfg;
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
/// Implementations may consult the orderbook subgraph (preferred — see
/// [`SubgraphRateFetcher`], not yet implemented) or call the ERC4626
/// contract directly (preferred fallback — see [`RpcRateFetcher`], not yet
/// implemented). [`StubRateFetcher`] is the only concrete impl today.
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
fn extract_asset_hint(token: &TokenCfg) -> (Address, String, u8) {
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
