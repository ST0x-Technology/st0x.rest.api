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
    /// Wallet that would execute the swap. Optional for a read-only quote, but
    /// recommended because oracle-backed orders can depend on the taker when
    /// building their signed context.
    #[schema(value_type = Option<String>, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub taker: Option<Address>,
    /// Token the taker will spend.
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    /// Token the taker will receive.
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    /// Determines which token `amount` refers to and whether a partial fill is
    /// allowed.
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    /// Human-readable target amount. This is an output-token amount for
    /// `buyUpTo`, and an input-token amount for `spendExact` or `spendUpTo`.
    #[schema(example = "100")]
    pub amount: String,
    /// Explicit maximum input-token amount per one output token. For example,
    /// `2600` for USDC -> WETH means at most 2600 USDC per WETH. Provide
    /// exactly one of `priceCap` or `slippageBps`.
    #[schema(example = "2600")]
    pub price_cap: Option<String>,
    /// Slippage tolerance in basis points (BPS), where 1 BPS = 0.01%,
    /// 50 BPS = 0.5%, and 100 BPS = 1%. The API applies this tolerance to the
    /// worst price in the selected SDK simulation to produce `resolvedPriceCap`.
    /// Provide exactly one of `priceCap` or `slippageBps`.
    #[schema(example = 50, minimum = 1, maximum = 5000)]
    pub slippage_bps: Option<u16>,
    /// Optional trusted reference price, always expressed as input-token units
    /// per one output token. For USDC -> WETH, use USDC per WETH; for
    /// WETH -> USDC, use WETH per USDC. Only valid with `slippageBps`. Before
    /// applying slippage, the API excludes candidates whose input/output ratio
    /// is more than 5% above this reference.
    #[schema(example = "2500")]
    pub reference_io_ratio: Option<String>,
    /// Units used by numeric request and response fields. `wrapped` is the
    /// default orderbook denomination. `unwrapped` changes numeric units only;
    /// token addresses must still be wrapped/orderbook token addresses.
    #[serde(default)]
    #[schema(example = "wrapped", default = "wrapped")]
    pub denomination: SwapDenomination,
}

/// OpenAPI-only request shape that documents the runtime requirement to provide
/// exactly one price limit. Rocket deserializes [`SwapQuoteV2Request`] so it can
/// return a descriptive 400 for both-or-neither requests.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum SwapQuoteV2RequestBody {
    PriceCap(SwapQuoteV2PriceCapRequest),
    Slippage(SwapQuoteV2SlippageRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteV2RequestCommon {
    /// Wallet that would execute the swap. Recommended for oracle-backed
    /// orders, whose signed context can depend on the taker.
    #[schema(value_type = Option<String>, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub taker: Option<Address>,
    /// Token the taker will spend.
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    /// Token the taker will receive.
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    /// Determines which token `amount` refers to and whether partial fills are
    /// allowed.
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    /// Human-readable output-token amount for `buyUpTo`, or input-token amount
    /// for `spendExact` and `spendUpTo`.
    #[schema(example = "100")]
    pub amount: String,
    /// Numeric denomination. Token addresses remain wrapped/orderbook
    /// addresses even when this is `unwrapped`.
    #[serde(default)]
    #[schema(example = "wrapped", default = "wrapped")]
    pub denomination: SwapDenomination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteV2PriceCapRequest {
    #[serde(flatten)]
    pub request: SwapQuoteV2RequestCommon,
    /// Maximum input-token amount per one output token.
    #[schema(example = "2600")]
    pub price_cap: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteV2SlippageRequest {
    #[serde(flatten)]
    pub request: SwapQuoteV2RequestCommon,
    /// Slippage tolerance in basis points: 1 BPS = 0.01%, 50 BPS = 0.5%,
    /// and 100 BPS = 1%.
    #[schema(example = 50, minimum = 1, maximum = 5000)]
    pub slippage_bps: u16,
    /// Optional trusted input-token-per-output-token reference price. The API
    /// rejects candidate ratios more than 5% above it before applying slippage.
    #[schema(example = "2500")]
    pub reference_io_ratio: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteV2Response {
    /// Token the taker spends.
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    /// Token the taker receives.
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    /// Requested target in the units implied by `mode`.
    #[schema(example = "100")]
    pub amount: String,
    #[schema(example = "wrapped")]
    pub denomination: SwapDenomination,
    /// Input-token amount selected by the simulation.
    #[schema(example = "100")]
    pub estimated_input: String,
    /// Output-token amount selected by the simulation.
    #[schema(example = "0.04")]
    pub estimated_output: String,
    /// Simulated input-token amount per one output token.
    #[schema(example = "2500")]
    pub estimated_io_ratio: String,
    /// Whether the simulation completely filled the requested amount. Up-to
    /// modes can return `false`; exact modes fail instead of returning a partial
    /// quote.
    pub fully_filled: bool,
    /// Final maximum input-token amount per one output token. This is the
    /// supplied `priceCap` or the cap derived from `slippageBps`. If a later
    /// calldata request requires approvals, reuse its resolved cap as
    /// `priceCap` on the retry so the limit does not move.
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
    /// Buy up to `amount` output tokens. Partial fills are allowed.
    BuyUpTo,
    /// Spend exactly `amount` input tokens. Fails when the full amount cannot
    /// be executed.
    SpendExact,
    /// Spend up to `amount` input tokens. Partial fills are allowed.
    SpendUpTo,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataV2Request {
    /// Wallet that will execute the swap and whose allowance is checked.
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub taker: Address,
    /// Token the taker will spend.
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    /// Token the taker will receive.
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    /// Determines which token `amount` refers to and whether a partial fill is
    /// allowed.
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    /// Human-readable output-token amount for `buyUpTo`, or input-token amount
    /// for `spendExact` and `spendUpTo`.
    #[schema(example = "100")]
    pub amount: String,
    /// Explicit maximum input-token amount per one output token. Provide
    /// exactly one of `priceCap` or `slippageBps`.
    #[schema(example = "2600")]
    pub price_cap: Option<String>,
    /// Slippage tolerance in basis points (BPS), where 1 BPS = 0.01%,
    /// 50 BPS = 0.5%, and 100 BPS = 1%. The API applies this tolerance to the
    /// worst price in the selected SDK simulation. Provide exactly one of
    /// `priceCap` or `slippageBps`.
    #[schema(example = 50, minimum = 1, maximum = 5000)]
    pub slippage_bps: Option<u16>,
    /// Optional trusted reference price, expressed as input-token units per one
    /// output token. Only valid with `slippageBps`. The API excludes candidate
    /// ratios more than 5% above this reference before applying slippage.
    #[schema(example = "2500")]
    pub reference_io_ratio: Option<String>,
    /// Units used by numeric request and response fields. Token addresses remain
    /// wrapped/orderbook addresses when this is `unwrapped`.
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
    /// Wallet that will execute the swap and whose allowance is checked.
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub taker: Address,
    /// Token the taker will spend.
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    /// Token the taker will receive.
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    /// Determines which token `amount` refers to and whether a partial fill is
    /// allowed.
    #[schema(example = "spendExact")]
    pub mode: SwapCalldataMode,
    /// Human-readable output-token amount for `buyUpTo`, or input-token amount
    /// for `spendExact` and `spendUpTo`.
    #[schema(example = "100")]
    pub amount: String,
    /// Numeric denomination. Token addresses remain wrapped/orderbook
    /// addresses even when this is `unwrapped`.
    #[serde(default)]
    #[schema(example = "wrapped", default = "wrapped")]
    pub denomination: SwapDenomination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataV2PriceCapRequest {
    #[serde(flatten)]
    pub request: SwapCalldataV2RequestCommon,
    /// Maximum input-token amount per one output token.
    #[schema(example = "2600")]
    pub price_cap: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapCalldataV2SlippageRequest {
    #[serde(flatten)]
    pub request: SwapCalldataV2RequestCommon,
    /// Slippage tolerance in basis points: 1 BPS = 0.01%, 50 BPS = 0.5%,
    /// and 100 BPS = 1%.
    #[schema(example = 50, minimum = 1, maximum = 5000)]
    pub slippage_bps: u16,
    /// Optional trusted input-token-per-output-token reference price. The API
    /// rejects candidate ratios more than 5% above it before applying slippage.
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
    /// Final maximum input-token amount per one output token, in the requested
    /// denomination. If approvals are returned, reuse this value as `priceCap`
    /// on the follow-up request and omit `slippageBps` and `referenceIoRatio`.
    /// This keeps the original limit fixed while the approval is submitted.
    #[schema(example = "2613")]
    pub resolved_price_cap: String,
}
