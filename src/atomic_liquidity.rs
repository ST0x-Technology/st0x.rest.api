use alloy::primitives::U256;
use rain_math_float::{Float, FloatError};
use std::ops::Mul;

/// Returns whether the quoted maximum trade transfers at least one atomic unit
/// of both tokens after the same lossy decimal conversion used for calldata.
pub(crate) fn has_executable_atomic_amounts(
    max_output: Float,
    ratio: Float,
    input_decimals: u8,
    output_decimals: u8,
) -> Result<bool, FloatError> {
    let max_input = max_output.mul(ratio)?;
    let input_units = max_input.to_fixed_decimal_lossy(input_decimals)?.0;
    let output_units = max_output.to_fixed_decimal_lossy(output_decimals)?.0;
    Ok(input_units != U256::ZERO && output_units != U256::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_production_wtmstr_quote_that_truncates_to_zero_usdc() {
        // Order 0x23015e87f07fb96cdf41dfb49861f8bc637b0a03f3c7f96ae3070e3845217315
        // produced these exact values in the 2026-08-17 production logs.
        let max_output = Float::parse(
            "3.838009207986009475041066889213119489421507552037575367421589999999e-7".to_string(),
        )
        .unwrap();
        let ratio = Float::parse(
            "0.007502722698831713208597944445410618026068357798898643535155918523172".to_string(),
        )
        .unwrap();

        assert!(!has_executable_atomic_amounts(max_output, ratio, 18, 6).unwrap());
    }

    #[test]
    fn rejects_reverse_production_quote_that_truncates_to_zero_wtmstr() {
        let max_output =
            Float::parse("3.0929278006630021974171463616436307359367256347038e-19".to_string())
                .unwrap();
        let ratio = Float::parse(
            "133.28494736393696697573612599399148735052322980189575719227947789035".to_string(),
        )
        .unwrap();

        assert!(!has_executable_atomic_amounts(max_output, ratio, 6, 18).unwrap());
    }

    #[test]
    fn accepts_executable_wtmstr_liquidity_near_the_real_price() {
        let max_output = Float::parse("1".to_string()).unwrap();
        let ratio =
            Float::parse("0.01022722482519392577486900982658075498646246277856858".to_string())
                .unwrap();

        assert!(has_executable_atomic_amounts(max_output, ratio, 18, 6).unwrap());
    }

    #[test]
    fn accepts_quote_at_exact_atomic_boundaries() {
        let max_output = Float::parse("0.000001".to_string()).unwrap();
        let ratio = Float::parse("0.000000000001".to_string()).unwrap();

        assert!(has_executable_atomic_amounts(max_output, ratio, 18, 6).unwrap());
    }
}
