use crate::types::common::Approval;
use crate::types::trades::Denomination;
use alloy::primitives::{Address, Bytes, U256};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SwapQuoteRequest {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "0.5")]
    pub output_amount: String,
    /// `wrapped` (default) returns the raw wrapped-token quote. `unwrapped`
    /// rescales `estimated_input`, `estimated_output`, and
    /// `estimated_io_ratio` by the latest `assetsPerShare` snapshot for any
    /// wrapped side of the pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denomination: Option<Denomination>,
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
    #[schema(example = "0.5")]
    pub estimated_output: String,
    #[schema(example = "1250.75")]
    pub estimated_input: String,
    #[schema(example = "2501.5")]
    pub estimated_io_ratio: String,
    /// Denomination applied to amounts/ratios in this response. Mirrors the
    /// `denomination` field on the request — `wrapped` for raw on-chain
    /// values, `unwrapped` for values rescaled via the wrapped exchange rate.
    #[serde(default)]
    pub denomination: Denomination,
    /// `assetsPerShare` rate(s) used when `denomination == "unwrapped"`.
    /// Decimal string; `None` for `wrapped` or when neither side of the pair
    /// is a wrapped token (so the conversion was a no-op).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "1.04", value_type = Option<String>)]
    pub assets_per_share: Option<String>,
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
    /// `wrapped` (default) treats `maximum_io_ratio` as already
    /// wtStock-denominated and forwards it straight to the contract.
    /// `unwrapped` interprets the caller-supplied ratio as a tStock value and
    /// reverse-converts it to wtStock terms before building the calldata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denomination: Option<Denomination>,
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
    pub approvals: Vec<Approval>,
    /// Denomination applied to the response. Mirrors the request value.
    #[serde(default)]
    pub denomination: Denomination,
    /// `assetsPerShare` rate(s) used when `denomination == "unwrapped"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "1.04", value_type = Option<String>)]
    pub assets_per_share: Option<String>,
    /// The wtStock-denominated `maximum_io_ratio` actually submitted to the
    /// orderbook contract. Equal to the caller-supplied value for `wrapped`,
    /// or the reverse-converted value for `unwrapped`.
    #[schema(example = "2600")]
    pub submitted_io_ratio: String,
}
