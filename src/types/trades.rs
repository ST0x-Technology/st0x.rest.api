use crate::types::common::TokenRef;
use alloy::primitives::{Address, FixedBytes};
use rocket::form::{FromForm, FromFormField};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Denomination requested for the prices in the response. `Wtstock` (the
/// default) returns trade amounts and IO ratios exactly as recorded on
/// chain — the input/output sides settle in wrapped (`wt*`) share tokens.
/// `Tstock` adjusts every wrapped-side amount by the assets-per-share rate
/// at that trade's block, so the response reads in underlying tStock terms.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
    ToSchema,
    FromFormField,
)]
#[serde(rename_all = "lowercase")]
pub enum Denomination {
    /// Raw wtStock denomination (default — backwards compatible).
    #[default]
    Wtstock,
    /// Adjusted to underlying tStock using the wrapped exchange rate at the
    /// trade's block number.
    Tstock,
}

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct TradesPaginationParams {
    #[field(name = "page")]
    #[param(example = 1)]
    pub page: Option<u32>,
    #[field(name = "pageSize")]
    #[param(example = 20)]
    pub page_size: Option<u32>,
    #[field(name = "startTime")]
    #[param(example = 1718452800)]
    pub start_time: Option<u64>,
    #[field(name = "endTime")]
    #[param(example = 1718539200)]
    pub end_time: Option<u64>,
    /// `wtstock` (default) returns raw on-chain amounts. `tstock` adjusts
    /// wrapped-side amounts and ratios by the assets-per-share rate that
    /// was current at each trade's block.
    #[field(name = "denomination")]
    pub denomination: Option<Denomination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradeByAddress {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: FixedBytes<32>,
    #[schema(example = "1000000")]
    pub input_amount: String,
    #[schema(example = "500000")]
    pub output_amount: String,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
    #[schema(value_type = Option<String>)]
    pub order_hash: Option<FixedBytes<32>>,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    #[schema(example = 12345678)]
    pub block_number: u64,
    /// Denomination applied to amounts in this row. Mirrors the
    /// `denomination` query parameter — `wtstock` for raw on-chain amounts,
    /// `tstock` for amounts adjusted by the wrapped exchange rate.
    #[serde(default)]
    pub denomination: Denomination,
    /// Per-trade assets-per-share rate used to convert wrapped-side amounts
    /// when `denomination == "tstock"`. Decimal string; `None` when the row
    /// is in `wtstock` denomination or when no historical snapshot was
    /// available for the wrapped tokens at the trade's block.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "1.04", value_type = Option<String>)]
    pub assets_per_share: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesPagination {
    #[schema(example = 1)]
    pub page: u32,
    #[schema(example = 20)]
    pub page_size: u32,
    #[schema(example = 100)]
    pub total_trades: u64,
    #[schema(example = 5)]
    pub total_pages: u64,
    #[schema(example = true)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesByAddressResponse {
    pub trades: Vec<TradeByAddress>,
    pub pagination: TradesPagination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradeRequest {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub input_token: Address,
    #[schema(value_type = String, example = "0x4200000000000000000000000000000000000006")]
    pub output_token: Address,
    #[schema(example = "1000000")]
    pub maximum_input: String,
    #[schema(example = "0.0006")]
    pub maximum_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradeResult {
    #[schema(example = "900000")]
    pub input_amount: String,
    #[schema(example = "500000")]
    pub output_amount: String,
    #[schema(example = "0.00055")]
    pub actual_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradeByTxEntry {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub order_owner: Address,
    pub request: TradeRequest,
    pub result: TradeResult,
    /// Denomination applied to the request/result amounts. Mirrors the
    /// `denomination` query parameter.
    #[serde(default)]
    pub denomination: Denomination,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "1.04", value_type = Option<String>)]
    pub assets_per_share: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesTotals {
    #[schema(example = "900000")]
    pub total_input_amount: String,
    #[schema(example = "500000")]
    pub total_output_amount: String,
    #[schema(example = "0.00055")]
    pub average_io_ratio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesByTxResponse {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: FixedBytes<32>,
    #[schema(example = 12345678)]
    pub block_number: u64,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub sender: Address,
    pub trades: Vec<TradeByTxEntry>,
    pub totals: TradesTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesBatchRequest {
    #[schema(value_type = Vec<String>)]
    pub order_hashes: Vec<alloy::primitives::B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesBatchEntry {
    #[schema(value_type = String)]
    pub order_hash: alloy::primitives::B256,
    pub trades: Vec<crate::types::order::OrderTradeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TradesBatchResponse {
    pub orders: Vec<TradesBatchEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TakerTradesResponse {
    pub market_orders: Vec<TradesByTxResponse>,
    pub pagination: TradesPagination,
}
