use super::map_raindex_error;
use crate::error::ApiError;
use alloy::primitives::Address;
use rain_math_float::Float;
use rain_orderbook_common::take_orders::{
    simulate_buy_over_candidates, simulate_spend_over_candidates, ParsedTakeOrdersMode,
    SimulationResult, TakeOrderCandidate, TakeOrdersMode,
};
use std::collections::HashMap;
use std::ops::{Div, Mul};

const BASIS_POINTS_SCALE: &str = "10000";

pub(crate) fn resolve_slippage_price_cap(
    candidates: Vec<TakeOrderCandidate>,
    mode: TakeOrdersMode,
    amount: &str,
    slippage_bps: u16,
) -> Result<Float, ApiError> {
    let mode = ParsedTakeOrdersMode::parse(mode, amount).map_err(map_raindex_error)?;
    let unbounded_price_cap = Float::max_positive_value().map_err(|error| {
        tracing::error!(%error, "failed to create unbounded slippage price cap");
        ApiError::Internal("failed to resolve slippage".into())
    })?;
    let simulation = select_best_raindex_simulation(candidates, mode, unbounded_price_cap)?;
    let worst_price = worst_price(&simulation)?.ok_or_else(|| {
        tracing::warn!("slippage simulation selected no order legs");
        ApiError::NotFound("no liquidity found for this pair".into())
    })?;
    let scale = Float::parse(BASIS_POINTS_SCALE.to_string()).map_err(|error| {
        tracing::error!(%error, "failed to create basis points scale");
        ApiError::Internal("failed to resolve slippage".into())
    })?;
    let multiplier = Float::parse((u32::from(slippage_bps) + 10_000).to_string())
        .and_then(|value| value.div(scale))
        .map_err(|error| {
            tracing::error!(%error, slippage_bps, "failed to calculate slippage multiplier");
            ApiError::Internal("failed to resolve slippage".into())
        })?;
    worst_price.mul(multiplier).map_err(|error| {
        tracing::error!(%error, slippage_bps, "failed to apply slippage tolerance");
        ApiError::Internal("failed to resolve slippage".into())
    })
}

fn select_best_raindex_simulation(
    candidates: Vec<TakeOrderCandidate>,
    mode: ParsedTakeOrdersMode,
    price_cap: Float,
) -> Result<SimulationResult, ApiError> {
    // Calldata can execute against one raindex deployment. Keep the deployment
    // comparison aligned with the SDK's take-orders selection while using its
    // public candidate simulation functions for the actual order walking.
    let mut candidates_by_raindex: HashMap<Address, Vec<TakeOrderCandidate>> = HashMap::new();
    for candidate in candidates {
        candidates_by_raindex
            .entry(candidate.raindex)
            .or_default()
            .push(candidate);
    }

    let mut best: Option<(Address, SimulationResult)> = None;
    for (raindex, candidates) in candidates_by_raindex {
        let simulation = if mode.is_buy_mode() {
            simulate_buy_over_candidates(candidates, mode.target_amount(), price_cap)
        } else {
            simulate_spend_over_candidates(candidates, mode.target_amount(), price_cap)
        }
        .map_err(map_raindex_error)?;

        if simulation.legs.is_empty() {
            continue;
        }

        let replace = match &best {
            None => true,
            Some((best_raindex, best_simulation)) => {
                is_better_simulation(raindex, &simulation, *best_raindex, best_simulation)?
            }
        };
        if replace {
            best = Some((raindex, simulation));
        }
    }

    best.map(|(_, simulation)| simulation).ok_or_else(|| {
        tracing::warn!("no raindex simulation produced liquidity for slippage resolution");
        ApiError::NotFound("no liquidity found for this pair".into())
    })
}

fn is_better_simulation(
    raindex: Address,
    simulation: &SimulationResult,
    best_raindex: Address,
    best_simulation: &SimulationResult,
) -> Result<bool, ApiError> {
    if simulation
        .total_output
        .gt(best_simulation.total_output)
        .map_err(map_float_comparison_error)?
    {
        return Ok(true);
    }
    if !simulation
        .total_output
        .eq(best_simulation.total_output)
        .map_err(map_float_comparison_error)?
    {
        return Ok(false);
    }

    match (worst_price(simulation)?, worst_price(best_simulation)?) {
        (Some(price), Some(best_price)) => {
            if price.lt(best_price).map_err(map_float_comparison_error)? {
                Ok(true)
            } else if price.eq(best_price).map_err(map_float_comparison_error)? {
                Ok(raindex < best_raindex)
            } else {
                Ok(false)
            }
        }
        _ => Ok(raindex < best_raindex),
    }
}

fn worst_price(simulation: &SimulationResult) -> Result<Option<Float>, ApiError> {
    let mut worst = None;
    for leg in &simulation.legs {
        if worst
            .map(|price| {
                leg.candidate
                    .ratio
                    .gt(price)
                    .map_err(map_float_comparison_error)
            })
            .transpose()?
            .unwrap_or(true)
        {
            worst = Some(leg.candidate.ratio);
        }
    }
    Ok(worst)
}

fn map_float_comparison_error(error: rain_math_float::FloatError) -> ApiError {
    tracing::error!(%error, "failed to compare slippage simulation values");
    ApiError::Internal("failed to resolve slippage".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::mock_candidate;

    fn candidate(raindex_byte: u8, max_output: &str, ratio: &str) -> TakeOrderCandidate {
        let mut candidate = mock_candidate(max_output, ratio);
        candidate.raindex = Address::from([raindex_byte; 20]);
        candidate
    }

    #[test]
    fn resolves_cap_from_worst_selected_buy_leg() {
        let cap = resolve_slippage_price_cap(
            vec![candidate(1, "5", "1"), candidate(1, "5", "2")],
            TakeOrdersMode::BuyUpTo,
            "8",
            50,
        )
        .unwrap();

        assert_eq!(cap.format().unwrap(), "2.01");
    }

    #[test]
    fn uses_sdk_deployment_selection_semantics() {
        let cap = resolve_slippage_price_cap(
            vec![candidate(1, "8", "3"), candidate(2, "10", "4")],
            TakeOrdersMode::BuyUpTo,
            "10",
            100,
        )
        .unwrap();

        assert_eq!(cap.format().unwrap(), "4.04");
    }

    #[test]
    fn breaks_equal_output_ties_with_lower_worst_price() {
        let cap = resolve_slippage_price_cap(
            vec![candidate(1, "10", "3"), candidate(2, "10", "2")],
            TakeOrdersMode::BuyUpTo,
            "10",
            100,
        )
        .unwrap();

        assert_eq!(cap.format().unwrap(), "2.02");
    }

    #[test]
    fn supports_spend_modes() {
        let cap = resolve_slippage_price_cap(
            vec![candidate(1, "5", "1"), candidate(1, "5", "2")],
            TakeOrdersMode::SpendExact,
            "8",
            100,
        )
        .unwrap();

        assert_eq!(cap.format().unwrap(), "2.02");
    }
}
