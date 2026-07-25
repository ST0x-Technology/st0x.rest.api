use crate::types::common::Approval;
use alloy::primitives::{Address, Bytes, U256};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SwapDenomination {
    #[default]
    Wrapped,
    Unwrapped,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteRequest {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "0.5")]
    pub output_amount: String,
    #[serde(default)]
    #[schema(example = "wrapped", default = "wrapped")]
    pub denomination: SwapDenomination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteResponse {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "0.5")]
    pub output_amount: String,
    #[schema(example = "wrapped")]
    pub denomination: SwapDenomination,
    #[schema(example = "0.5")]
    pub estimated_output: String,
    #[schema(example = "1250.75")]
    pub estimated_input: String,
    #[schema(example = "2501.5")]
    pub estimated_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteV2Request {
    /// Optional taker used when building oracle-backed quote candidates.
    #[schema(value_type = Option<String>, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub taker: Option<Address>,
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    #[schema(example = "100")]
    pub amount: String,
    /// Explicit price cap. Provide exactly one of `priceCap` or `slippageBps`.
    #[schema(example = "2600")]
    pub price_cap: Option<String>,
    /// Optional slippage tolerance resolved from SDK quotes. Provide exactly
    /// one of `priceCap` or `slippageBps`.
    #[schema(example = 50, minimum = 1, maximum = 5000)]
    pub slippage_bps: Option<u16>,
    /// Optional input-per-output oracle ratio used to exclude candidate orders
    /// more than 5% worse than the reference before resolving `slippageBps`.
    #[schema(example = "2500")]
    pub reference_io_ratio: Option<String>,
    #[serde(default)]
    #[schema(example = "wrapped", default = "wrapped")]
    pub denomination: SwapDenomination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteV2Response {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    #[schema(example = "100")]
    pub amount: String,
    #[schema(example = "wrapped")]
    pub denomination: SwapDenomination,
    #[schema(example = "100")]
    pub estimated_input: String,
    #[schema(example = "0.04")]
    pub estimated_output: String,
    #[schema(example = "2500")]
    pub estimated_io_ratio: String,
    /// Whether the requested amount was completely filled.
    pub fully_filled: bool,
    /// Final price cap in the requested denomination.
    #[schema(example = "2512.5")]
    pub resolved_price_cap: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataRequest {
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub taker: Address,
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "0.5")]
    pub output_amount: String,
    #[schema(example = "2600")]
    pub maximum_io_ratio: String,
    #[serde(default)]
    #[schema(example = "wrapped", default = "wrapped")]
    pub denomination: SwapDenomination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum SwapCalldataMode {
    BuyUpTo,
    SpendExact,
    SpendUpTo,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataV2Request {
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub taker: Address,
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    #[schema(example = "100")]
    pub amount: String,
    /// Explicit price cap. Provide exactly one of `priceCap` or `slippageBps`.
    #[schema(example = "2600")]
    pub price_cap: Option<String>,
    /// Optional slippage tolerance resolved from SDK quotes. Provide exactly
    /// one of `priceCap` or `slippageBps`.
    #[schema(example = 50, minimum = 1, maximum = 5000)]
    pub slippage_bps: Option<u16>,
    /// Optional input-per-output oracle ratio used to exclude candidate orders
    /// more than 5% worse than the reference before resolving `slippageBps`.
    /// Only valid with `slippageBps` and uses the requested `denomination`.
    #[schema(example = "2500")]
    pub reference_io_ratio: Option<String>,
    #[serde(default)]
    #[schema(example = "wrapped", default = "wrapped")]
    pub denomination: SwapDenomination,
}

/// OpenAPI-only request shape that encodes the runtime requirement to provide
/// exactly one price limit. Rocket deserializes [`SwapCalldataV2Request`] so it
/// can return a descriptive 400 for both-or-neither requests.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum SwapCalldataV2RequestBody {
    PriceCap(SwapCalldataV2PriceCapRequest),
    Slippage(SwapCalldataV2SlippageRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataV2RequestCommon {
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub taker: Address,
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    #[schema(example = "100")]
    pub amount: String,
    #[serde(default)]
    #[schema(example = "wrapped", default = "wrapped")]
    pub denomination: SwapDenomination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataV2PriceCapRequest {
    #[serde(flatten)]
    pub request: SwapCalldataV2RequestCommon,
    #[schema(example = "2600")]
    pub price_cap: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataV2SlippageRequest {
    #[serde(flatten)]
    pub request: SwapCalldataV2RequestCommon,
    #[schema(example = 50, minimum = 1, maximum = 5000)]
    pub slippage_bps: u16,
    /// Optional input-per-output oracle ratio used to exclude candidate orders
    /// more than 5% worse than the reference before resolving the price cap.
    #[schema(example = "2500")]
    pub reference_io_ratio: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataResponse {
    #[schema(value_type = String, example = "0xDEF171Fe48CF0115B1d80b88dc8eAB59176FEe57")]
    pub to: Address,
    #[schema(value_type = String, example = "0xabcdef...")]
    pub data: Bytes,
    #[schema(value_type = String, example = "0x0")]
    pub value: U256,
    #[schema(example = "1250.75")]
    pub estimated_input: String,
    #[schema(example = "wrapped")]
    pub denomination: SwapDenomination,
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataV2Response {
    #[serde(flatten)]
    pub calldata: SwapCalldataResponse,
    /// Final price cap in the requested denomination. Reuse this value as
    /// `priceCap` after approvals.
    #[schema(example = "2613")]
    pub resolved_price_cap: String,
}
