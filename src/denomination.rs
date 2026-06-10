use crate::error::ApiError;
use crate::wrap_ratio::WrapRatioValue;
use alloy::primitives::Address;
use rain_math_float::Float;
use std::collections::HashMap;
use std::ops::{Div, Mul};

pub(crate) type WrapRatioMap = HashMap<Address, WrapRatioValue>;

pub(crate) fn convert_wrapped_amount_for_token(
    amount: String,
    token: Address,
    ratios: &WrapRatioMap,
) -> Result<String, ApiError> {
    let Some(ratio) = ratios.get(&token) else {
        return Ok(amount);
    };

    let amount = parse_float(amount, "amount")?;
    let converted = amount.mul(parse_ratio(ratio)?).map_err(|e| {
        tracing::error!(error = %e, "failed to convert wrapped amount");
        ApiError::Internal("failed to convert wrapped amount".into())
    })?;
    format_float(converted, "amount")
}

pub(crate) fn convert_wrapped_io_ratio(
    io_ratio: String,
    input_token: Address,
    output_token: Address,
    ratios: &WrapRatioMap,
) -> Result<String, ApiError> {
    if io_ratio == "-" {
        return Ok(io_ratio);
    }

    let io_ratio = parse_float(io_ratio, "io_ratio")?;
    let input_assets_per_share = ratio_for_token(input_token, ratios)?;
    let output_assets_per_share = ratio_for_token(output_token, ratios)?;

    let converted = io_ratio
        .mul(input_assets_per_share)
        .and_then(|ratio| ratio.div(output_assets_per_share))
        .map_err(|e| {
            tracing::error!(error = %e, "failed to convert wrapped IO ratio");
            ApiError::Internal("failed to convert wrapped IO ratio".into())
        })?;

    format_float(converted, "io_ratio")
}

fn ratio_for_token(token: Address, ratios: &WrapRatioMap) -> Result<Float, ApiError> {
    match ratios.get(&token) {
        Some(ratio) => parse_ratio(ratio),
        None => Float::parse("1".to_string()).map_err(|e| {
            tracing::error!(error = %e, token = %token, "failed to create identity wrap ratio");
            ApiError::Internal("failed to convert denomination".into())
        }),
    }
}

fn parse_ratio(ratio: &WrapRatioValue) -> Result<Float, ApiError> {
    Float::parse(ratio.assets_per_share.clone()).map_err(|e| {
        tracing::error!(
            error = %e,
            share_address = %ratio.share_address,
            assets_per_share = %ratio.assets_per_share,
            "failed to parse wrapped token ratio"
        );
        ApiError::Internal("failed to read wrapped token ratio".into())
    })
}

fn parse_float(value: String, label: &str) -> Result<Float, ApiError> {
    Float::parse(value).map_err(|e| {
        tracing::error!(error = %e, label, "failed to parse denomination value");
        ApiError::Internal(format!("failed to parse {label}"))
    })
}

fn format_float(value: Float, label: &str) -> Result<String, ApiError> {
    value.format().map_err(|e| {
        tracing::error!(error = %e, label, "failed to format denomination value");
        ApiError::Internal(format!("failed to format {label}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const WT_MSTR: Address = address!("ff05e1bd696900dc6a52ca35ca61bb1024eda8e2");
    const WT_COIN: Address = address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    const USDC: Address = address!("833589fcd6edb6e08f4c7c32d4f71b54bda02913");

    fn ratio(share_address: Address, assets_per_share: &str) -> WrapRatioValue {
        WrapRatioValue {
            share_address,
            assets_per_share: assets_per_share.to_string(),
        }
    }

    #[test]
    fn converts_amount_for_wrapped_token() {
        let ratios = HashMap::from([(WT_MSTR, ratio(WT_MSTR, "2"))]);

        let result = convert_wrapped_amount_for_token("1.5".to_string(), WT_MSTR, &ratios)
            .expect("convert amount");

        assert_eq!(result, "3");
    }

    #[test]
    fn leaves_amount_for_unwrapped_token() {
        let ratios = HashMap::from([(WT_MSTR, ratio(WT_MSTR, "2"))]);

        let result = convert_wrapped_amount_for_token("1.5".to_string(), USDC, &ratios)
            .expect("convert amount");

        assert_eq!(result, "1.5");
    }

    #[test]
    fn converts_io_ratio_for_wrapped_sides() {
        let ratios = HashMap::from([
            (WT_MSTR, ratio(WT_MSTR, "2")),
            (WT_COIN, ratio(WT_COIN, "4")),
        ]);

        let result = convert_wrapped_io_ratio("10".to_string(), WT_MSTR, WT_COIN, &ratios)
            .expect("convert ratio");

        assert_eq!(result, "5");
    }

    #[test]
    fn preserves_dash_io_ratio() {
        let ratios = HashMap::from([(WT_MSTR, ratio(WT_MSTR, "2"))]);

        let result = convert_wrapped_io_ratio("-".to_string(), WT_MSTR, USDC, &ratios)
            .expect("convert ratio");

        assert_eq!(result, "-");
    }
}
