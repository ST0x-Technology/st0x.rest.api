//! Applies the `denomination=unwrapped` adjustment to trade response payloads.
//!
//! The on-chain trade record always settles in wrapped (`wt*`) share tokens.
//! To express a trade in underlying tStock terms we multiply the
//! wrapped-side amount by the assets-per-share rate that was current at the
//! trade's block, look up via [`crate::db::wrapped_rates::get_at_or_before_block`].
//! When a side isn't a wrapped token, or no historical snapshot exists, the
//! row's amounts and ratios are left untouched and `assets_per_share` is set
//! to `None` so callers can distinguish "converted" from "unavailable".
//!
//! Shared math (float parsing, sign-aware multiply, rate-string combination,
//! ratio scaling) and the [`RateLookup`](crate::denomination::RateLookup) trait
//! live in [`crate::denomination`].

use crate::db::wrapped_rates as db_rates;
use crate::db::DbPool;
use crate::denomination::{
    combine_rate_strings, convert_amounts_to_unwrapped_ratio, format_float, multiply_decimal,
    parse_float, RateLookup,
};
use crate::error::ApiError;
use crate::types::trades::{
    Denomination, TradeByTxEntry, TradesByAddressResponse, TradesByTxResponse, TradesTotals,
};
use alloy::primitives::Address;
use async_trait::async_trait;
use rain_math_float::Float;
use std::collections::{HashMap, HashSet};
use std::ops::{Add, Div};

/// Builds a lookup of wrapped-token rate snapshots for the addresses
/// touched by a batch of trades. Each `(token_address, block_number)` pair
/// is resolved to the snapshot ≤ that block, then memoized so repeated
/// lookups in the same response are free.
pub(crate) struct HistoricalRateLookup<'a> {
    pool: &'a DbPool,
    cache: HashMap<(Address, u64), Option<db_rates::WrappedRateSnapshot>>,
    wrapped_tokens: HashSet<Address>,
}

impl<'a> HistoricalRateLookup<'a> {
    pub(crate) fn new(pool: &'a DbPool, wrapped_tokens: HashSet<Address>) -> Self {
        Self {
            pool,
            cache: HashMap::new(),
            wrapped_tokens,
        }
    }

    async fn snapshot_at(
        &mut self,
        token: Address,
        block_number: u64,
    ) -> Result<Option<db_rates::WrappedRateSnapshot>, ApiError> {
        if !self.is_wrapped(token) {
            return Ok(None);
        }
        let key = (token, block_number);
        if let Some(existing) = self.cache.get(&key) {
            return Ok(existing.clone());
        }
        let snapshot = db_rates::get_at_or_before_block(self.pool, token, block_number)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, token = ?token, block_number, "rate lookup failed");
                ApiError::Internal("wrapped rate lookup failed".into())
            })?;
        self.cache.insert(key, snapshot.clone());
        Ok(snapshot)
    }
}

#[async_trait]
impl<'a> RateLookup for HistoricalRateLookup<'a> {
    type Context = u64;

    async fn rate_for(
        &mut self,
        token: Address,
        block_number: u64,
    ) -> Result<Option<String>, ApiError> {
        let snapshot = self.snapshot_at(token, block_number).await?;
        Ok(snapshot.map(|s| s.assets_per_share))
    }

    fn is_wrapped(&self, addr: Address) -> bool {
        self.wrapped_tokens.contains(&addr)
    }
}

pub(crate) async fn apply_denomination_by_address(
    lookup: &mut HistoricalRateLookup<'_>,
    denomination: Denomination,
    response: &mut TradesByAddressResponse,
) -> Result<(), ApiError> {
    if denomination == Denomination::Wrapped {
        return Ok(());
    }

    for trade in &mut response.trades {
        let input_rate = lookup
            .rate_for(trade.input_token.address, trade.block_number)
            .await?;
        let output_rate = lookup
            .rate_for(trade.output_token.address, trade.block_number)
            .await?;

        if let Some(rate) = input_rate.as_deref() {
            trade.input_amount = multiply_decimal(&trade.input_amount, rate)?;
        }
        if let Some(rate) = output_rate.as_deref() {
            trade.output_amount = multiply_decimal(&trade.output_amount, rate)?;
        }
        trade.denomination = Denomination::Unwrapped;
        trade.assets_per_share =
            combine_rate_strings(input_rate.as_deref(), output_rate.as_deref());
    }

    Ok(())
}

pub(crate) async fn apply_denomination_by_tx(
    lookup: &mut HistoricalRateLookup<'_>,
    denomination: Denomination,
    response: &mut TradesByTxResponse,
) -> Result<(), ApiError> {
    if denomination == Denomination::Wrapped {
        return Ok(());
    }

    let block_number = response.block_number;
    let mut adjusted_totals_input: Option<Float> = None;
    let mut adjusted_totals_output: Option<Float> = None;

    for entry in &mut response.trades {
        let input_rate = lookup
            .rate_for(entry.request.input_token, block_number)
            .await?;
        let output_rate = lookup
            .rate_for(entry.request.output_token, block_number)
            .await?;

        adjust_entry(entry, input_rate.as_deref(), output_rate.as_deref())?;

        // Accumulate totals using the *adjusted* request amounts (which now
        // sit in tStock terms when adjustment happened, raw otherwise).
        let totals_input = adjusted_totals_input
            .take()
            .map_or_else(Float::zero, Ok)
            .map_err(|e| {
                tracing::error!(error = %e, "float zero construction failed");
                ApiError::Internal("trade totals calculation failed".into())
            })?;
        let entry_input = parse_float(&entry.result.input_amount, "trade totals")?;
        adjusted_totals_input = Some(totals_input.add(entry_input).map_err(|e| {
            tracing::error!(error = %e, "failed to sum unwrapped input");
            ApiError::Internal("trade totals calculation failed".into())
        })?);

        let totals_output = adjusted_totals_output
            .take()
            .map_or_else(Float::zero, Ok)
            .map_err(|e| {
                tracing::error!(error = %e, "float zero construction failed");
                ApiError::Internal("trade totals calculation failed".into())
            })?;
        // The result.output_amount is negative (vault outflow); accumulate
        // its magnitude to mirror the wrapped totals semantics.
        let magnitude = entry.result.output_amount.trim_start_matches('-');
        let entry_output = parse_float(magnitude, "trade totals")?;
        adjusted_totals_output = Some(totals_output.add(entry_output).map_err(|e| {
            tracing::error!(error = %e, "failed to sum unwrapped output");
            ApiError::Internal("trade totals calculation failed".into())
        })?);
    }

    if let (Some(input), Some(output)) = (adjusted_totals_input, adjusted_totals_output) {
        let zero = Float::zero().map_err(|e| {
            tracing::error!(error = %e, "float zero construction failed");
            ApiError::Internal("trade totals calculation failed".into())
        })?;
        let avg = if output.eq(zero).unwrap_or(true) {
            zero
        } else {
            input.div(output).map_err(|e| {
                tracing::error!(error = %e, "failed to compute average io ratio in unwrapped");
                ApiError::Internal("trade totals calculation failed".into())
            })?
        };
        response.totals = TradesTotals {
            total_input_amount: format_float(input, "trade totals")?,
            total_output_amount: format_float(output, "trade totals")?,
            average_io_ratio: format_float(avg, "trade totals")?,
        };
    }

    Ok(())
}

fn adjust_entry(
    entry: &mut TradeByTxEntry,
    input_rate: Option<&str>,
    output_rate: Option<&str>,
) -> Result<(), ApiError> {
    if let Some(rate) = input_rate {
        entry.request.maximum_input = multiply_decimal(&entry.request.maximum_input, rate)?;
        entry.result.input_amount = multiply_decimal(&entry.result.input_amount, rate)?;
    }
    if let Some(rate) = output_rate {
        entry.result.output_amount = multiply_decimal(&entry.result.output_amount, rate)?;
    }

    let new_ratio = convert_amounts_to_unwrapped_ratio(
        &entry.result.input_amount,
        &entry.result.output_amount,
        // Amounts above are already scaled — pass None so we don't double-apply.
        None,
        None,
    )?;
    entry.request.maximum_io_ratio = new_ratio.clone();
    entry.result.actual_io_ratio = new_ratio;
    entry.denomination = Denomination::Unwrapped;
    entry.assets_per_share = combine_rate_strings(input_rate, output_rate);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::types::common::TokenRef;
    use crate::types::trades::{
        TradeByAddress, TradeByTxEntry, TradeRequest, TradeResult, TradesByAddressResponse,
        TradesByTxResponse, TradesPagination, TradesTotals,
    };

    fn token(addr_byte: u8, symbol: &str, decimals: u8) -> TokenRef {
        let mut bytes = [0u8; 20];
        bytes[19] = addr_byte;
        TokenRef {
            address: bytes.into(),
            symbol: symbol.into(),
            decimals,
        }
    }

    async fn fresh_pool() -> DbPool {
        let id = uuid::Uuid::new_v4();
        db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("init db")
    }

    #[rocket::async_test]
    async fn test_no_adjustment_when_wrapped() {
        let pool = fresh_pool().await;
        let mut lookup = HistoricalRateLookup::new(&pool, HashSet::new());
        let mut response = TradesByAddressResponse {
            trades: vec![TradeByAddress {
                tx_hash: Default::default(),
                input_amount: "1.0".into(),
                output_amount: "-2.0".into(),
                input_token: token(1, "wtMSTR", 18),
                output_token: token(2, "USDC", 6),
                order_hash: None,
                timestamp: 1,
                block_number: 100,
                denomination: Denomination::Wrapped,
                assets_per_share: None,
            }],
            pagination: TradesPagination {
                page: 1,
                page_size: 20,
                total_trades: 1,
                total_pages: 1,
                has_more: false,
            },
        };
        apply_denomination_by_address(&mut lookup, Denomination::Wrapped, &mut response)
            .await
            .unwrap();
        assert_eq!(response.trades[0].input_amount, "1.0");
        assert_eq!(response.trades[0].output_amount, "-2.0");
        assert_eq!(response.trades[0].denomination, Denomination::Wrapped);
        assert!(response.trades[0].assets_per_share.is_none());
    }

    #[rocket::async_test]
    async fn test_unwrapped_adjusts_input_only_when_wrapped() {
        let pool = fresh_pool().await;
        let wt = token(1, "wtMSTR", 18);
        db_rates::insert_snapshot(
            &pool,
            &db_rates::NewWrappedRateSnapshot {
                token_address: wt.address,
                block_number: 50,
                block_timestamp: 1_700_000_000,
                assets_per_share: "1.5",
                asset_address: Address::ZERO,
                asset_symbol: "tMSTR",
                asset_decimals: 18,
            },
        )
        .await
        .unwrap();

        let mut wrapped = HashSet::new();
        wrapped.insert(wt.address);
        let mut lookup = HistoricalRateLookup::new(&pool, wrapped);

        let mut response = TradesByAddressResponse {
            trades: vec![TradeByAddress {
                tx_hash: Default::default(),
                input_amount: "10".into(),
                output_amount: "-50".into(),
                input_token: wt.clone(),
                output_token: token(2, "USDC", 6),
                order_hash: None,
                timestamp: 1,
                block_number: 100,
                denomination: Denomination::Wrapped,
                assets_per_share: None,
            }],
            pagination: TradesPagination {
                page: 1,
                page_size: 20,
                total_trades: 1,
                total_pages: 1,
                has_more: false,
            },
        };
        apply_denomination_by_address(&mut lookup, Denomination::Unwrapped, &mut response)
            .await
            .unwrap();

        // input_amount: 10 * 1.5 = 15
        assert_eq!(response.trades[0].input_amount, "15");
        // output is USDC, not wrapped — unchanged
        assert_eq!(response.trades[0].output_amount, "-50");
        assert_eq!(response.trades[0].denomination, Denomination::Unwrapped);
        assert_eq!(response.trades[0].assets_per_share.as_deref(), Some("1.5"));
    }

    #[rocket::async_test]
    async fn test_unwrapped_leaves_unchanged_when_no_snapshot_at_block() {
        let pool = fresh_pool().await;
        let wt = token(1, "wtMSTR", 18);
        // Snapshot only at block 200; trade at block 100 — should not match.
        db_rates::insert_snapshot(
            &pool,
            &db_rates::NewWrappedRateSnapshot {
                token_address: wt.address,
                block_number: 200,
                block_timestamp: 1_700_000_000,
                assets_per_share: "1.5",
                asset_address: Address::ZERO,
                asset_symbol: "tMSTR",
                asset_decimals: 18,
            },
        )
        .await
        .unwrap();

        let mut wrapped = HashSet::new();
        wrapped.insert(wt.address);
        let mut lookup = HistoricalRateLookup::new(&pool, wrapped);

        let mut response = TradesByAddressResponse {
            trades: vec![TradeByAddress {
                tx_hash: Default::default(),
                input_amount: "10".into(),
                output_amount: "-50".into(),
                input_token: wt.clone(),
                output_token: token(2, "USDC", 6),
                order_hash: None,
                timestamp: 1,
                block_number: 100,
                denomination: Denomination::Wrapped,
                assets_per_share: None,
            }],
            pagination: TradesPagination {
                page: 1,
                page_size: 20,
                total_trades: 1,
                total_pages: 1,
                has_more: false,
            },
        };
        apply_denomination_by_address(&mut lookup, Denomination::Unwrapped, &mut response)
            .await
            .unwrap();

        assert_eq!(response.trades[0].input_amount, "10");
        assert_eq!(response.trades[0].denomination, Denomination::Unwrapped);
        assert!(response.trades[0].assets_per_share.is_none());
    }

    #[rocket::async_test]
    async fn test_unwrapped_adjusts_tx_response_amounts_and_ratios() {
        let pool = fresh_pool().await;
        let wt = token(1, "wtMSTR", 18);
        db_rates::insert_snapshot(
            &pool,
            &db_rates::NewWrappedRateSnapshot {
                token_address: wt.address,
                block_number: 50,
                block_timestamp: 1_700_000_000,
                assets_per_share: "2.0",
                asset_address: Address::ZERO,
                asset_symbol: "tMSTR",
                asset_decimals: 18,
            },
        )
        .await
        .unwrap();

        let mut wrapped = HashSet::new();
        wrapped.insert(wt.address);
        let mut lookup = HistoricalRateLookup::new(&pool, wrapped);

        let mut response = TradesByTxResponse {
            tx_hash: Default::default(),
            block_number: 100,
            timestamp: 1,
            sender: Address::ZERO,
            trades: vec![TradeByTxEntry {
                order_hash: Default::default(),
                order_owner: Address::ZERO,
                request: TradeRequest {
                    input_token: wt.address,
                    output_token: token(2, "USDC", 6).address,
                    maximum_input: "10".into(),
                    maximum_io_ratio: "0.2".into(),
                },
                result: TradeResult {
                    input_amount: "10".into(),
                    output_amount: "-50".into(),
                    actual_io_ratio: "0.2".into(),
                },
                denomination: Denomination::Wrapped,
                assets_per_share: None,
            }],
            totals: TradesTotals {
                total_input_amount: "10".into(),
                total_output_amount: "50".into(),
                average_io_ratio: "0.2".into(),
            },
        };

        apply_denomination_by_tx(&mut lookup, Denomination::Unwrapped, &mut response)
            .await
            .unwrap();

        // 10 wtMSTR @ rate 2.0 → 20 tMSTR
        assert_eq!(response.trades[0].result.input_amount, "20");
        assert_eq!(response.trades[0].request.maximum_input, "20");
        // USDC side unchanged
        assert_eq!(response.trades[0].result.output_amount, "-50");
        // Ratio recomputed: 20 / 50 = 0.4
        assert_eq!(response.trades[0].request.maximum_io_ratio, "0.4");
        assert_eq!(response.trades[0].denomination, Denomination::Unwrapped);
        // Totals recomputed
        assert_eq!(response.totals.total_input_amount, "20");
        assert_eq!(response.totals.average_io_ratio, "0.4");
    }
}
