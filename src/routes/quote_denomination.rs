//! Applies the `denomination=tstock` adjustment to forward-looking (quote /
//! order / calldata) response payloads.
//!
//! Unlike historical trade conversion — which uses each trade's block number
//! to pick a snapshot — these endpoints describe *current* state. They use
//! the latest snapshot per wrapped token, refreshing if older than 24h.
//!
//! The math mirrors `trades_denomination`:
//! - Amounts on a wrapped side are multiplied by `assetsPerShare`.
//! - An IO ratio (input/output) becomes `wtstock_ratio * (input_rate / output_rate)`
//!   where the rate for a non-wrapped side is treated as `1.0`.
//! - The reverse conversion used for `swap/calldata` is the inverse:
//!   `wtstock_ratio = tstock_ratio * (output_rate / input_rate)`. The
//!   contract always speaks wtStock, so the caller hands us a tStock ratio
//!   and we feed the contract its wrapped equivalent.

use crate::db::wrapped_rates as db_rates;
use crate::db::DbPool;
use crate::error::ApiError;
use crate::raindex::SharedRaindexProvider;
use crate::types::trades::Denomination;
use crate::wrapped_rates::{self, RateFetcher};
use alloy::primitives::Address;
use rain_math_float::Float;
use rain_orderbook_app_settings::token::TokenCfg;
use std::collections::HashMap;
use std::ops::{Div, Mul};

/// Max age before a cached rate snapshot is refreshed from the subgraph.
const MAX_SNAPSHOT_AGE_SECS: i64 = 86_400;

/// Lookup of the *current* assets-per-share rate for each wrapped token.
/// Memoizes per-Address so a single response only fetches once.
pub(crate) struct CurrentRateLookup<'a> {
    pool: &'a DbPool,
    fetcher: &'a dyn RateFetcher,
    wrapped_tokens: HashMap<Address, TokenCfg>,
    cache: HashMap<Address, Option<String>>,
}

impl<'a> CurrentRateLookup<'a> {
    pub(crate) fn new(
        pool: &'a DbPool,
        fetcher: &'a dyn RateFetcher,
        wrapped_tokens: HashMap<Address, TokenCfg>,
    ) -> Self {
        Self {
            pool,
            fetcher,
            wrapped_tokens,
            cache: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn is_wrapped(&self, addr: Address) -> bool {
        self.wrapped_tokens.contains_key(&addr)
    }

    /// Returns the current `assetsPerShare` decimal string for `token`, or
    /// `None` if the token is not a wrapped token. Refreshes via the
    /// configured fetcher when the latest persisted snapshot is older than
    /// `MAX_SNAPSHOT_AGE_SECS`.
    pub(crate) async fn rate_for(&mut self, token: Address) -> Result<Option<String>, ApiError> {
        if let Some(cached) = self.cache.get(&token) {
            return Ok(cached.clone());
        }
        let Some(token_cfg) = self.wrapped_tokens.get(&token).cloned() else {
            self.cache.insert(token, None);
            return Ok(None);
        };

        let snapshot = match wrapped_rates::refresh_if_stale(
            self.pool,
            self.fetcher,
            &token_cfg,
            MAX_SNAPSHOT_AGE_SECS,
        )
        .await
        {
            Ok(s) => Some(s),
            Err(e) => {
                // If refresh fails (subgraph/RPC down), fall back to whatever
                // snapshot is already persisted; if none, give up gracefully.
                tracing::warn!(error = %e, token = ?token, "rate refresh failed; falling back to last snapshot");
                db_rates::get_latest_for_token(self.pool, token)
                    .await
                    .map_err(|e| {
                        tracing::error!(error = %e, token = ?token, "rate lookup fallback failed");
                        ApiError::Internal("wrapped rate lookup failed".into())
                    })?
            }
        };

        let rate = snapshot.map(|s| s.assets_per_share);
        self.cache.insert(token, rate.clone());
        Ok(rate)
    }
}

/// Build the wrapped-token map (address → TokenCfg) by filtering the
/// registry. The TokenCfg is required for the subgraph refresh path.
pub(crate) async fn wrapped_token_map(
    shared_raindex: &SharedRaindexProvider,
) -> Result<HashMap<Address, TokenCfg>, ApiError> {
    let raindex = shared_raindex.read().await;
    let tokens = raindex.client().get_all_tokens().map_err(|e| {
        tracing::error!(error = %e, "failed to get tokens for wrapped map");
        ApiError::Internal("failed to load token list".into())
    })?;
    Ok(tokens
        .into_values()
        .filter(wrapped_rates::is_wrapped_token)
        .map(|t| (t.address, t))
        .collect())
}

fn parse_float(value: &str, context: &'static str) -> Result<Float, ApiError> {
    Float::parse(value.to_string()).map_err(|e| {
        tracing::error!(error = %e, value, context, "failed to parse float");
        ApiError::Internal(format!("{context} parse failed"))
    })
}

fn format_float(value: Float, context: &'static str) -> Result<String, ApiError> {
    value.format().map_err(|e| {
        tracing::error!(error = %e, context, "float formatting failed");
        ApiError::Internal(format!("{context} format failed"))
    })
}

fn multiply_decimal(amount: &str, rate: &str) -> Result<String, ApiError> {
    let amount_f = parse_float(amount, "quote amount")?;
    let rate_f = parse_float(rate, "wrapped rate")?;
    let product = amount_f.mul(rate_f).map_err(|e| {
        tracing::error!(error = %e, "failed to scale amount by wrapped rate");
        ApiError::Internal("denomination adjustment failed".into())
    })?;
    format_float(product, "quote amount")
}

/// Combine optional input/output rates into a single response field. If both
/// sides have rates and they differ, we emit both — matches the pattern used
/// by `trades_denomination::combine_rate_strings`.
fn combine_rate_strings(input_rate: Option<&str>, output_rate: Option<&str>) -> Option<String> {
    match (input_rate, output_rate) {
        (Some(i), Some(o)) if i == o => Some(i.to_string()),
        (Some(i), Some(o)) => Some(format!("input={i};output={o}")),
        (Some(i), None) => Some(i.to_string()),
        (None, Some(o)) => Some(o.to_string()),
        (None, None) => None,
    }
}

/// Convert a wtStock-denominated IO ratio to its tStock equivalent.
///
/// `tstock_ratio = wtstock_ratio * (input_rate / output_rate)` with `1.0`
/// substituted for the non-wrapped side. Both sides wrapped at the same rate
/// is a no-op (the ratio cancels).
fn convert_ratio_to_tstock(
    ratio: &str,
    input_rate: Option<&str>,
    output_rate: Option<&str>,
) -> Result<String, ApiError> {
    if input_rate.is_none() && output_rate.is_none() {
        return Ok(ratio.to_string());
    }
    let one = Float::parse("1.0".into()).map_err(|e| {
        tracing::error!(error = %e, "float one construction failed");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    let ratio_f = parse_float(ratio, "io ratio")?;
    let input_f = match input_rate {
        Some(r) => parse_float(r, "wrapped rate")?,
        None => one,
    };
    let output_f = match output_rate {
        Some(r) => parse_float(r, "wrapped rate")?,
        None => one,
    };
    let scale = input_f.div(output_f).map_err(|e| {
        tracing::error!(error = %e, "failed to scale io ratio");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    let converted = ratio_f.mul(scale).map_err(|e| {
        tracing::error!(error = %e, "failed to apply tstock conversion to io ratio");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    format_float(converted, "io ratio")
}

/// Inverse of `convert_ratio_to_tstock`. Caller's tStock-denominated ratio
/// becomes the wtStock value the contract expects.
fn convert_ratio_to_wtstock(
    ratio: &str,
    input_rate: Option<&str>,
    output_rate: Option<&str>,
) -> Result<String, ApiError> {
    if input_rate.is_none() && output_rate.is_none() {
        return Ok(ratio.to_string());
    }
    let one = Float::parse("1.0".into()).map_err(|e| {
        tracing::error!(error = %e, "float one construction failed");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    let ratio_f = parse_float(ratio, "io ratio")?;
    let input_f = match input_rate {
        Some(r) => parse_float(r, "wrapped rate")?,
        None => one,
    };
    let output_f = match output_rate {
        Some(r) => parse_float(r, "wrapped rate")?,
        None => one,
    };
    let scale = output_f.div(input_f).map_err(|e| {
        tracing::error!(error = %e, "failed to scale io ratio");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    let converted = ratio_f.mul(scale).map_err(|e| {
        tracing::error!(error = %e, "failed to apply wtstock conversion to io ratio");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    format_float(converted, "io ratio")
}

/// Adjust a swap quote response to tStock terms. No-op when `denomination`
/// is `Wtstock`. Mutates `response` in place.
pub(crate) async fn apply_denomination_to_quote(
    response: &mut crate::types::swap::SwapQuoteResponse,
    denomination: Denomination,
    lookup: &mut CurrentRateLookup<'_>,
) -> Result<(), ApiError> {
    if denomination == Denomination::Wtstock {
        response.denomination = Denomination::Wtstock;
        return Ok(());
    }

    let input_rate = lookup.rate_for(response.input_token).await?;
    let output_rate = lookup.rate_for(response.output_token).await?;

    if let Some(rate) = input_rate.as_deref() {
        response.estimated_input = multiply_decimal(&response.estimated_input, rate)?;
    }
    if let Some(rate) = output_rate.as_deref() {
        response.estimated_output = multiply_decimal(&response.estimated_output, rate)?;
        // The request-side `output_amount` echoes what the caller asked for.
        // When the user requests tstock, the response should keep the field
        // self-consistent: `output_amount` is the wtStock amount we actually
        // simulated against. We leave it unchanged so the response is
        // unambiguous about what was simulated; the rescaled estimate moves
        // to `estimated_output`.
    }

    response.estimated_io_ratio = convert_ratio_to_tstock(
        &response.estimated_io_ratio,
        input_rate.as_deref(),
        output_rate.as_deref(),
    )?;

    response.denomination = Denomination::Tstock;
    response.assets_per_share = combine_rate_strings(input_rate.as_deref(), output_rate.as_deref());
    Ok(())
}

/// Reverse-convert a tStock-denominated `maximum_io_ratio` to its wtStock
/// equivalent for on-chain submission. Returns the wtStock ratio plus the
/// combined `assets_per_share` rate string (or unchanged ratio + None for
/// `wtstock` denomination / non-wrapped pairs).
pub(crate) async fn reverse_convert_calldata_ratio(
    io_ratio: &str,
    input_token: Address,
    output_token: Address,
    denomination: Denomination,
    lookup: &mut CurrentRateLookup<'_>,
) -> Result<(String, Option<String>), ApiError> {
    if denomination == Denomination::Wtstock {
        return Ok((io_ratio.to_string(), None));
    }
    let input_rate = lookup.rate_for(input_token).await?;
    let output_rate = lookup.rate_for(output_token).await?;
    let wtstock_ratio =
        convert_ratio_to_wtstock(io_ratio, input_rate.as_deref(), output_rate.as_deref())?;
    let aps = combine_rate_strings(input_rate.as_deref(), output_rate.as_deref());
    Ok((wtstock_ratio, aps))
}

/// Generic order-list adjustment. `extract` returns the `(input_token,
/// output_token, io_ratio)` triple for an order entry; the closure-pair
/// `update` writes the new ratio, `denomination`, and `assets_per_share`
/// back. Order list ordering is preserved — callers sort orderbook depth
/// before converting.
pub(crate) async fn apply_denomination_to_order_list<T, E, U>(
    orders: &mut [T],
    denomination: Denomination,
    lookup: &mut CurrentRateLookup<'_>,
    extract: E,
    update: U,
) -> Result<(), ApiError>
where
    E: Fn(&T) -> (Address, Address, String),
    U: Fn(&mut T, String, Option<String>, Denomination),
{
    if denomination == Denomination::Wtstock {
        return Ok(());
    }
    // NOTE: ordering is preserved; we iterate in place and only rewrite
    // string fields. The orderbook-depth sort already happened upstream.
    for entry in orders.iter_mut() {
        let (input_token, output_token, io_ratio) = extract(entry);
        let input_rate = lookup.rate_for(input_token).await?;
        let output_rate = lookup.rate_for(output_token).await?;
        // Skip when the ratio is a placeholder (failed quote) — leave the
        // entry untouched but still mark denomination so the response shape
        // is consistent.
        if io_ratio == "-" {
            let aps = combine_rate_strings(input_rate.as_deref(), output_rate.as_deref());
            update(entry, io_ratio, aps, Denomination::Tstock);
            continue;
        }
        let new_ratio =
            convert_ratio_to_tstock(&io_ratio, input_rate.as_deref(), output_rate.as_deref())?;
        let aps = combine_rate_strings(input_rate.as_deref(), output_rate.as_deref());
        update(entry, new_ratio, aps, Denomination::Tstock);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::types::swap::SwapQuoteResponse;
    use crate::wrapped_rates::{RateFetchError, RateObservation};
    use alloy::primitives::address;
    use async_trait::async_trait;
    use rain_orderbook_app_settings::network::NetworkCfg;
    use rain_orderbook_app_settings::yaml::default_document;
    use std::sync::Arc;

    async fn fresh_pool() -> DbPool {
        let id = uuid::Uuid::new_v4();
        db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("init db")
    }

    fn token_cfg(addr: Address, symbol: &str) -> TokenCfg {
        TokenCfg {
            document: default_document(),
            key: symbol.into(),
            network: Arc::new(NetworkCfg {
                document: default_document(),
                key: "base".into(),
                rpcs: vec![],
                chain_id: 8453,
                label: None,
                network_id: None,
                currency: None,
            }),
            address: addr,
            decimals: Some(18),
            label: None,
            symbol: Some(symbol.into()),
            logo_uri: None,
            extensions: None,
        }
    }

    /// Test fetcher returning a fixed rate per token.
    struct FixedRateFetcher {
        rates: HashMap<Address, String>,
    }

    #[async_trait]
    impl RateFetcher for FixedRateFetcher {
        async fn fetch_current_rate(
            &self,
            token: &TokenCfg,
        ) -> Result<RateObservation, RateFetchError> {
            let rate = self
                .rates
                .get(&token.address)
                .cloned()
                .unwrap_or_else(|| "1.0".into());
            Ok(RateObservation {
                token_address: token.address,
                block_number: 0,
                block_timestamp: 0,
                assets_per_share: rate,
                asset_address: Address::ZERO,
                asset_symbol: "tStub".into(),
                asset_decimals: 18,
            })
        }
    }

    const WT_MSTR: Address = address!("00000000000000000000000000000000000000aa");
    const USDC: Address = address!("00000000000000000000000000000000000000bb");
    const WT_OTHER: Address = address!("00000000000000000000000000000000000000cc");

    fn quote_response_with(input: Address, output: Address) -> SwapQuoteResponse {
        SwapQuoteResponse {
            input_token: input,
            output_token: output,
            output_amount: "100".into(),
            estimated_output: "100".into(),
            estimated_input: "10".into(),
            estimated_io_ratio: "0.1".into(),
            denomination: Denomination::Wtstock,
            assets_per_share: None,
        }
    }

    #[rocket::async_test]
    async fn test_wtstock_default_is_noop() {
        let pool = fresh_pool().await;
        let fetcher = FixedRateFetcher {
            rates: HashMap::new(),
        };
        let mut lookup = CurrentRateLookup::new(&pool, &fetcher, HashMap::new());
        let mut resp = quote_response_with(USDC, WT_MSTR);
        apply_denomination_to_quote(&mut resp, Denomination::Wtstock, &mut lookup)
            .await
            .unwrap();
        assert_eq!(resp.estimated_input, "10");
        assert_eq!(resp.estimated_output, "100");
        assert_eq!(resp.estimated_io_ratio, "0.1");
        assert_eq!(resp.denomination, Denomination::Wtstock);
        assert!(resp.assets_per_share.is_none());
    }

    #[rocket::async_test]
    async fn test_tstock_scales_input_when_input_wrapped() {
        let pool = fresh_pool().await;
        let mut rates = HashMap::new();
        rates.insert(WT_MSTR, "2.0".into());
        let fetcher = FixedRateFetcher { rates };
        let mut wrapped = HashMap::new();
        wrapped.insert(WT_MSTR, token_cfg(WT_MSTR, "wtMSTR"));
        let mut lookup = CurrentRateLookup::new(&pool, &fetcher, wrapped);

        let mut resp = quote_response_with(WT_MSTR, USDC);
        apply_denomination_to_quote(&mut resp, Denomination::Tstock, &mut lookup)
            .await
            .unwrap();
        // input is wt*, scaled by 2.0
        assert_eq!(resp.estimated_input, "20");
        // output is USDC, unchanged
        assert_eq!(resp.estimated_output, "100");
        // ratio: 0.1 * (2.0 / 1.0) = 0.2
        assert_eq!(resp.estimated_io_ratio, "0.2");
        assert_eq!(resp.denomination, Denomination::Tstock);
        assert_eq!(resp.assets_per_share.as_deref(), Some("2.0"));
    }

    #[rocket::async_test]
    async fn test_tstock_noop_when_neither_side_wrapped() {
        let pool = fresh_pool().await;
        let fetcher = FixedRateFetcher {
            rates: HashMap::new(),
        };
        let mut lookup = CurrentRateLookup::new(&pool, &fetcher, HashMap::new());
        let mut resp = quote_response_with(USDC, USDC);
        apply_denomination_to_quote(&mut resp, Denomination::Tstock, &mut lookup)
            .await
            .unwrap();
        assert_eq!(resp.estimated_input, "10");
        assert_eq!(resp.estimated_output, "100");
        assert_eq!(resp.estimated_io_ratio, "0.1");
        assert_eq!(resp.denomination, Denomination::Tstock);
        assert!(resp.assets_per_share.is_none());
    }

    #[rocket::async_test]
    async fn test_tstock_both_sides_wrapped_does_not_panic() {
        let pool = fresh_pool().await;
        let mut rates = HashMap::new();
        rates.insert(WT_MSTR, "1.5".into());
        rates.insert(WT_OTHER, "2.5".into());
        let fetcher = FixedRateFetcher { rates };
        let mut wrapped = HashMap::new();
        wrapped.insert(WT_MSTR, token_cfg(WT_MSTR, "wtMSTR"));
        wrapped.insert(WT_OTHER, token_cfg(WT_OTHER, "wtOTHER"));
        let mut lookup = CurrentRateLookup::new(&pool, &fetcher, wrapped);

        let mut resp = quote_response_with(WT_MSTR, WT_OTHER);
        apply_denomination_to_quote(&mut resp, Denomination::Tstock, &mut lookup)
            .await
            .unwrap();
        // input scaled by 1.5, output scaled by 2.5
        assert_eq!(resp.estimated_input, "15");
        assert_eq!(resp.estimated_output, "250");
        // ratio: 0.1 * (1.5 / 2.5) = 0.06
        assert_eq!(resp.estimated_io_ratio, "0.06");
        assert_eq!(resp.denomination, Denomination::Tstock);
        assert_eq!(
            resp.assets_per_share.as_deref(),
            Some("input=1.5;output=2.5")
        );
    }

    #[rocket::async_test]
    async fn test_reverse_convert_calldata_ratio_round_trip() {
        // Caller provides X tStock; we expect X / R submitted to contract
        // when *input* is wrapped (since wt_ratio = ts_ratio * out_rate / in_rate
        // with out=1.0, in=R → wt_ratio = ts_ratio / R).
        let pool = fresh_pool().await;
        let mut rates = HashMap::new();
        rates.insert(WT_MSTR, "2.0".into());
        let fetcher = FixedRateFetcher { rates };
        let mut wrapped = HashMap::new();
        wrapped.insert(WT_MSTR, token_cfg(WT_MSTR, "wtMSTR"));
        let mut lookup = CurrentRateLookup::new(&pool, &fetcher, wrapped);

        // input is wt*, output is USDC
        let (wt_ratio, aps) =
            reverse_convert_calldata_ratio("0.4", WT_MSTR, USDC, Denomination::Tstock, &mut lookup)
                .await
                .unwrap();
        // 0.4 * (1.0 / 2.0) = 0.2
        assert_eq!(wt_ratio, "0.2");
        assert_eq!(aps.as_deref(), Some("2.0"));
    }

    #[rocket::async_test]
    async fn test_reverse_convert_wtstock_is_passthrough() {
        let pool = fresh_pool().await;
        let fetcher = FixedRateFetcher {
            rates: HashMap::new(),
        };
        let mut lookup = CurrentRateLookup::new(&pool, &fetcher, HashMap::new());
        let (wt_ratio, aps) = reverse_convert_calldata_ratio(
            "0.5",
            WT_MSTR,
            USDC,
            Denomination::Wtstock,
            &mut lookup,
        )
        .await
        .unwrap();
        assert_eq!(wt_ratio, "0.5");
        assert!(aps.is_none());
    }
}
