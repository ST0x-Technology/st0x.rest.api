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
//! Today the on-chain/subgraph fetch is stubbed: rates are read from the
//! `settings` table (key `wrapped_rate:<token_address>`), with a default of
//! `1.0` when no override has been configured. The fetcher trait keeps the
//! call site uniform so a real implementation (subgraph or RPC) can replace
//! [`StubRateFetcher`] without churn at the endpoints.

use crate::db::wrapped_rates as db_rates;
use crate::db::{settings, DbPool};
use alloy::primitives::Address;
use async_trait::async_trait;
use rain_orderbook_app_settings::token::TokenCfg;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Stub implementation: reads `wrapped_rate:<addr>` from the `settings`
/// table when present, otherwise returns [`STUB_DEFAULT_RATE`]. This is a
/// deliberate placeholder so the rest of the system (storage, endpoint,
/// per-trade lookup) can be exercised end-to-end without subgraph or RPC
/// dependencies. Replace with a real fetcher when one is ready.
pub(crate) struct StubRateFetcher<'a> {
    pub pool: &'a DbPool,
}

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

/// Treat a token as wrapped if its `extensions.category == "ST0x"` (this is
/// how the st0x registry labels wrapped tStock tokens — see the existing
/// remote-tokens metadata served alongside the registry).
pub(crate) fn is_wrapped_token(token: &TokenCfg) -> bool {
    extract_extension_string(token.extensions.as_ref(), "category")
        .map(|c| c.eq_ignore_ascii_case("ST0x"))
        .unwrap_or(false)
}

/// Pulls underlying-asset metadata from the wrapped token's extensions when
/// available. Recognized keys (any of):
///   - `asset` or `assetAddress` → address
///   - `assetSymbol` → symbol
///   - `assetDecimals` → decimals
///
/// All three default to zero/empty when absent — the snapshot row still
/// records the rate, the asset block is just informational.
fn extract_asset_hint(token: &TokenCfg) -> (Address, String, u8) {
    let ext = token.extensions.as_ref();
    let addr = extract_extension_string(ext, "assetAddress")
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
