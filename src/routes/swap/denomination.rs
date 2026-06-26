use super::SwapDataSource;
use crate::error::ApiError;
use crate::types::swap::{SwapCalldataResponse, SwapDenomination};
use crate::wrap_ratio::WrapRatioValue;
use alloy::primitives::Address;
use rain_math_float::Float;
use rain_orderbook_common::take_orders::TakeOrdersMode;
use std::collections::HashMap;
use std::ops::{Div, Mul};

pub(crate) struct CalldataRequestNormalization {
    pub denomination: SwapDenomination,
    pub input_token: Address,
    pub output_token: Address,
    pub mode: TakeOrdersMode,
    pub amount: String,
    pub amount_field: &'static str,
    pub price_cap: String,
    pub price_cap_field: &'static str,
}

pub(crate) async fn normalize_quote_amounts(
    ds: &dyn SwapDataSource,
    denomination: SwapDenomination,
    input_token: Address,
    output_token: Address,
    estimated_input: Float,
    estimated_output: Float,
) -> Result<(Float, Float), ApiError> {
    match denomination {
        SwapDenomination::Wrapped => Ok((estimated_input, estimated_output)),
        SwapDenomination::Unwrapped => {
            tracing::info!("normalizing swap quote response to unwrapped denomination");
            let ratios = ds
                .get_wrap_ratios_for_tokens(&[input_token, output_token])
                .await?;

            let converted_input =
                convert_wrapped_amount_for_token(estimated_input, input_token, &ratios)?;
            let converted_output =
                convert_wrapped_amount_for_token(estimated_output, output_token, &ratios)?;

            Ok((converted_input, converted_output))
        }
    }
}

pub(crate) async fn normalize_calldata_request_values(
    ds: &dyn SwapDataSource,
    req: CalldataRequestNormalization,
) -> Result<(String, String, HashMap<Address, WrapRatioValue>), ApiError> {
    match req.denomination {
        SwapDenomination::Wrapped => Ok((req.amount, req.price_cap, HashMap::new())),
        SwapDenomination::Unwrapped => {
            tracing::info!("normalizing swap calldata request to wrapped denomination");
            let ratios = ds
                .get_wrap_ratios_for_tokens(&[req.input_token, req.output_token])
                .await?;
            let input_is_wrapped = ratios.contains_key(&req.input_token);
            let output_is_wrapped = ratios.contains_key(&req.output_token);

            if !input_is_wrapped && !output_is_wrapped {
                return Ok((req.amount, req.price_cap, ratios));
            }

            let input_assets_per_share = ratio_for_token(req.input_token, &ratios)?;
            let output_assets_per_share = ratio_for_token(req.output_token, &ratios)?;

            let price_cap = parse_user_float(req.price_cap, req.price_cap_field)?;

            let normalized_amount = if amount_needs_wrapped_normalization(
                req.mode,
                input_is_wrapped,
                output_is_wrapped,
            ) {
                let amount = parse_user_float(req.amount, req.amount_field)?;
                let ratio = amount_ratio(req.mode, input_assets_per_share, output_assets_per_share);
                let wrapped_amount = amount.div(ratio).map_err(|e| {
                    tracing::error!(error = %e, "failed to normalize calldata amount");
                    ApiError::Internal("failed to normalize amount".into())
                })?;
                format_float(wrapped_amount, "amount")?
            } else {
                req.amount
            };

            let normalized_io_ratio = price_cap
                .mul(output_assets_per_share)
                .and_then(|ratio| ratio.div(input_assets_per_share))
                .map_err(|e| {
                    tracing::error!(error = %e, "failed to normalize calldata IO ratio");
                    ApiError::Internal("failed to normalize IO ratio".into())
                })?;

            Ok((
                normalized_amount,
                format_float(normalized_io_ratio, "IO ratio")?,
                ratios,
            ))
        }
    }
}

fn amount_needs_wrapped_normalization(
    mode: TakeOrdersMode,
    input_is_wrapped: bool,
    output_is_wrapped: bool,
) -> bool {
    match mode {
        TakeOrdersMode::BuyExact | TakeOrdersMode::BuyUpTo => output_is_wrapped,
        TakeOrdersMode::SpendExact | TakeOrdersMode::SpendUpTo => input_is_wrapped,
    }
}

fn amount_ratio(
    mode: TakeOrdersMode,
    input_assets_per_share: Float,
    output_assets_per_share: Float,
) -> Float {
    match mode {
        TakeOrdersMode::BuyExact | TakeOrdersMode::BuyUpTo => output_assets_per_share,
        TakeOrdersMode::SpendExact | TakeOrdersMode::SpendUpTo => input_assets_per_share,
    }
}

pub(crate) fn normalize_calldata_response(
    ratios: &HashMap<Address, WrapRatioValue>,
    denomination: SwapDenomination,
    input_token: Address,
    mut response: SwapCalldataResponse,
) -> Result<SwapCalldataResponse, ApiError> {
    response.denomination = denomination;

    if denomination == SwapDenomination::Wrapped {
        return Ok(response);
    }

    tracing::info!("normalizing swap calldata response to unwrapped denomination");
    if ratios.contains_key(&input_token) {
        let estimated_input = parse_internal_float(response.estimated_input, "estimated_input")?;
        response.estimated_input = format_float(
            convert_wrapped_amount_for_token(estimated_input, input_token, ratios)?,
            "estimated input",
        )?;
    }

    Ok(response)
}

fn convert_wrapped_amount_for_token(
    amount: Float,
    token: Address,
    ratios: &HashMap<Address, WrapRatioValue>,
) -> Result<Float, ApiError> {
    match ratios.get(&token) {
        Some(ratio) => {
            tracing::info!(
                share_address = %ratio.share_address,
                assets_per_share = %ratio.assets_per_share,
                "normalizing wrapped token amount"
            );
            amount.mul(parse_ratio(ratio)?).map_err(|e| {
                tracing::error!(error = %e, "failed to normalize wrapped token amount");
                ApiError::Internal("failed to normalize wrapped token amount".into())
            })
        }
        None => Ok(amount),
    }
}

fn ratio_for_token(
    token: Address,
    ratios: &HashMap<Address, WrapRatioValue>,
) -> Result<Float, ApiError> {
    match ratios.get(&token) {
        Some(ratio) => {
            tracing::info!(
                share_address = %ratio.share_address,
                assets_per_share = %ratio.assets_per_share,
                "using wrapped token ratio"
            );
            parse_ratio(ratio)
        }
        None => Float::parse("1".to_string()).map_err(|e| {
            tracing::error!(error = %e, "failed to create identity wrap ratio");
            ApiError::Internal("failed to normalize denomination".into())
        }),
    }
}

fn parse_ratio(ratio: &WrapRatioValue) -> Result<Float, ApiError> {
    Float::parse(ratio.assets_per_share.clone()).map_err(|e| {
        tracing::error!(error = %e, "failed to parse wrapped token ratio");
        ApiError::Internal("failed to read wrapped token ratio".into())
    })
}

fn parse_user_float(value: String, field: &str) -> Result<Float, ApiError> {
    Float::parse(value).map_err(|e| {
        tracing::error!(error = %e, field, "failed to parse swap denomination value");
        ApiError::BadRequest(format!("invalid {field}"))
    })
}

fn parse_internal_float(value: String, field: &str) -> Result<Float, ApiError> {
    Float::parse(value).map_err(|e| {
        tracing::error!(error = %e, field, "failed to parse swap denomination response value");
        ApiError::Internal(format!("failed to read {field}"))
    })
}

fn format_float(value: Float, label: &str) -> Result<String, ApiError> {
    value.format().map_err(|e| {
        tracing::error!(error = %e, label, "failed to format swap denomination value");
        ApiError::Internal(format!("failed to format {label}"))
    })
}
