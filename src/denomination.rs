//! Shared helpers for converting between `wtstock` and `tstock` denominations.
//!
//! Two route families need the same math but talk to different sources of
//! `assetsPerShare`:
//!
//! - Historical (trades) responses look up the snapshot ≤ the trade's block
//!   number via [`crate::db::wrapped_rates::get_at_or_before_block`].
//! - Forward-looking (quote / order / calldata) responses use the *current*
//!   snapshot, refreshed via [`crate::wrapped_rates::refresh_if_stale`] when
//!   stale.
//!
//! Both shapes are unified behind the [`RateLookup`] trait. The remaining
//! arithmetic — float parsing/formatting, sign-aware multiplication, and the
//! two ratio-conversion directions — lives in this module so the per-route
//! adjusters in `routes::trades_denomination` and `routes::quote_denomination`
//! only carry response-shaping code.

use crate::error::ApiError;
use crate::raindex::SharedRaindexProvider;
use crate::wrapped_rates;
use alloy::primitives::Address;
use async_trait::async_trait;
use rain_math_float::Float;
use rain_orderbook_app_settings::token::TokenCfg;
use std::collections::{HashMap, HashSet};
use std::ops::{Div, Mul};

/// Parse a decimal string into a [`Float`], mapping errors onto [`ApiError`].
/// `context` is folded into the error message and tracing entry so callers can
/// distinguish which field failed without a backtrace.
pub(crate) fn parse_float(value: &str, context: &'static str) -> Result<Float, ApiError> {
    Float::parse(value.to_string()).map_err(|e| {
        tracing::error!(error = %e, value, context, "failed to parse float");
        ApiError::Internal(format!("{context} parse failed"))
    })
}

/// Format a [`Float`] back to a decimal string with the same error mapping
/// convention as [`parse_float`].
pub(crate) fn format_float(value: Float, context: &'static str) -> Result<String, ApiError> {
    value.format().map_err(|e| {
        tracing::error!(error = %e, context, "float formatting failed");
        ApiError::Internal(format!("{context} format failed"))
    })
}

/// Multiply a decimal `amount` by a decimal `rate`, preserving any leading
/// minus sign on `amount` (vault outflows in trade responses are encoded as
/// negative strings — we scale the magnitude and re-attach the sign).
///
/// Unsigned callers (quote endpoints) can pass positive strings and get back a
/// positive result with no extra ceremony.
pub(crate) fn multiply_decimal(amount: &str, rate: &str) -> Result<String, ApiError> {
    let trimmed = amount.trim_start();
    let (sign, magnitude) = match trimmed.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", trimmed),
    };
    let amount_f = parse_float(magnitude, "amount")?;
    let rate_f = parse_float(rate, "wrapped rate")?;
    let product = amount_f.mul(rate_f).map_err(|e| {
        tracing::error!(error = %e, "failed to scale amount by wrapped rate");
        ApiError::Internal("denomination adjustment failed".into())
    })?;
    Ok(format!("{sign}{}", format_float(product, "amount")?))
}

/// Collapse optional per-side `assetsPerShare` rates into a single response
/// field. Equal rates render as one decimal; mismatched rates render as
/// `input=X;output=Y` so the response stays self-describing.
pub(crate) fn combine_rate_strings(
    input_rate: Option<&str>,
    output_rate: Option<&str>,
) -> Option<String> {
    match (input_rate, output_rate) {
        (Some(i), Some(o)) if i == o => Some(i.to_string()),
        (Some(i), Some(o)) => Some(format!("input={i};output={o}")),
        (Some(i), None) => Some(i.to_string()),
        (None, Some(o)) => Some(o.to_string()),
        (None, None) => None,
    }
}

/// Recompute an IO ratio in tStock terms from already-known input/output
/// amounts. Used by the trades-by-tx path where amounts are scaled first and
/// the ratio derives from the post-scaling values.
///
/// `input_rate` / `output_rate` are optional pre-scaling multipliers applied
/// to the amounts before the division. Pass `None` when the caller has
/// already scaled the amounts (the typical case after `multiply_decimal`).
/// Signs on the amount strings are stripped — the resulting ratio is always
/// the magnitude.
pub(crate) fn convert_amounts_to_tstock_ratio(
    input_amount: &str,
    output_amount: &str,
    input_rate: Option<&str>,
    output_rate: Option<&str>,
) -> Result<String, ApiError> {
    let zero = Float::zero().map_err(|e| {
        tracing::error!(error = %e, "float zero construction failed");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    let input_f = parse_float(input_amount.trim_start_matches('-'), "amount")?;
    let output_f = parse_float(output_amount.trim_start().trim_start_matches('-'), "amount")?;

    let input_f = match input_rate {
        Some(r) => input_f.mul(parse_float(r, "wrapped rate")?).map_err(|e| {
            tracing::error!(error = %e, "failed to scale input by wrapped rate");
            ApiError::Internal("denomination adjustment failed".into())
        })?,
        None => input_f,
    };
    let output_f = match output_rate {
        Some(r) => output_f.mul(parse_float(r, "wrapped rate")?).map_err(|e| {
            tracing::error!(error = %e, "failed to scale output by wrapped rate");
            ApiError::Internal("denomination adjustment failed".into())
        })?,
        None => output_f,
    };

    if output_f.eq(zero).unwrap_or(true) {
        return Ok("0".into());
    }
    let ratio = input_f.div(output_f).map_err(|e| {
        tracing::error!(error = %e, "failed to compute io ratio in tstock");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    format_float(ratio, "io ratio")
}

/// Convert a wtStock-denominated IO ratio to its tStock equivalent.
///
/// `tstock_ratio = wtstock_ratio * (input_rate / output_rate)` with `1.0`
/// substituted for the non-wrapped side. Both sides wrapped at the same rate
/// is a no-op (the ratio cancels).
pub(crate) fn convert_ratio_to_tstock(
    ratio: &str,
    input_rate: Option<&str>,
    output_rate: Option<&str>,
) -> Result<String, ApiError> {
    scale_ratio(ratio, input_rate, output_rate, /*invert=*/ false)
}

/// Inverse of [`convert_ratio_to_tstock`]. Caller's tStock-denominated ratio
/// becomes the wtStock value the contract expects.
pub(crate) fn convert_ratio_to_wtstock(
    ratio: &str,
    input_rate: Option<&str>,
    output_rate: Option<&str>,
) -> Result<String, ApiError> {
    scale_ratio(ratio, input_rate, output_rate, /*invert=*/ true)
}

fn scale_ratio(
    ratio: &str,
    input_rate: Option<&str>,
    output_rate: Option<&str>,
    invert: bool,
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
    let (num, denom) = if invert {
        (output_f, input_f)
    } else {
        (input_f, output_f)
    };
    let scale = num.div(denom).map_err(|e| {
        tracing::error!(error = %e, "failed to scale io ratio");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    let converted = ratio_f.mul(scale).map_err(|e| {
        tracing::error!(error = %e, "failed to apply denomination conversion to io ratio");
        ApiError::Internal("io ratio calculation failed".into())
    })?;
    format_float(converted, "io ratio")
}

/// Lookup of registry tokens whose symbol identifies them as wrapped (`wt*`)
/// share tokens. The historical path only needs membership (`HashSet`); the
/// current path needs the full [`TokenCfg`] for subgraph refreshes. We build
/// both shapes once per request so the two consumers share the registry walk.
pub(crate) struct WrappedTokenIndex {
    set: HashSet<Address>,
    map: HashMap<Address, TokenCfg>,
}

impl WrappedTokenIndex {
    /// Walk the raindex token registry, keep only the wrapped tokens, and
    /// surface them as both a membership set and an address→config map.
    pub(crate) async fn load(shared_raindex: &SharedRaindexProvider) -> Result<Self, ApiError> {
        let raindex = shared_raindex.read().await;
        let tokens = raindex.client().get_all_tokens().map_err(|e| {
            tracing::error!(error = %e, "failed to get tokens for wrapped index");
            ApiError::Internal("failed to load token list".into())
        })?;
        let map: HashMap<Address, TokenCfg> = tokens
            .into_values()
            .filter(wrapped_rates::is_wrapped_token)
            .map(|t| (t.address, t))
            .collect();
        let set = map.keys().copied().collect();
        Ok(Self { set, map })
    }

    #[allow(dead_code)] // public surface mirrors as_map; current call sites consume via `into_set`
    pub(crate) fn as_set(&self) -> &HashSet<Address> {
        &self.set
    }

    #[allow(dead_code)] // public surface mirrors as_set; current call sites consume via `into_map`
    pub(crate) fn as_map(&self) -> &HashMap<Address, TokenCfg> {
        &self.map
    }

    pub(crate) fn into_set(self) -> HashSet<Address> {
        self.set
    }

    pub(crate) fn into_map(self) -> HashMap<Address, TokenCfg> {
        self.map
    }
}

/// Unified rate-resolution interface shared by the historical and current
/// adjusters. Implementors decide what `Context` they need (block number for
/// historical, `()` for current) and how to cache/fall back on failure.
///
/// Returns the decimal `assetsPerShare` string for `token`, or `None` if
/// `token` is not a wrapped share token (or, for the historical path, no
/// snapshot exists at-or-before the requested block).
#[async_trait]
pub(crate) trait RateLookup {
    type Context: Copy + Send;
    async fn rate_for(
        &mut self,
        token: Address,
        ctx: Self::Context,
    ) -> Result<Option<String>, ApiError>;
    fn is_wrapped(&self, token: Address) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiply_decimal_preserves_negative_sign() {
        let out = multiply_decimal("-10", "1.5").unwrap();
        assert_eq!(out, "-15");
    }

    #[test]
    fn multiply_decimal_unsigned_is_positive() {
        let out = multiply_decimal("10", "1.5").unwrap();
        assert_eq!(out, "15");
    }

    #[test]
    fn combine_rate_strings_collapses_equal() {
        assert_eq!(
            combine_rate_strings(Some("1.5"), Some("1.5")).as_deref(),
            Some("1.5")
        );
        assert_eq!(
            combine_rate_strings(Some("1.5"), Some("2.5")).as_deref(),
            Some("input=1.5;output=2.5")
        );
        assert!(combine_rate_strings(None, None).is_none());
    }

    #[test]
    fn convert_ratio_to_tstock_noop_when_no_rates() {
        let out = convert_ratio_to_tstock("0.1", None, None).unwrap();
        assert_eq!(out, "0.1");
    }

    #[test]
    fn convert_ratio_to_tstock_scales_by_input_over_output() {
        let out = convert_ratio_to_tstock("0.1", Some("2.0"), None).unwrap();
        assert_eq!(out, "0.2");
    }

    #[test]
    fn convert_ratio_to_wtstock_is_inverse() {
        let tstock = convert_ratio_to_tstock("0.1", Some("2.0"), None).unwrap();
        let round_trip = convert_ratio_to_wtstock(&tstock, Some("2.0"), None).unwrap();
        assert_eq!(round_trip, "0.1");
    }
}
