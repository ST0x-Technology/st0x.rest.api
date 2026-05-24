use crate::types::common::TokenRef;
use crate::types::trades::Denomination;
use alloy::primitives::{Address, Bytes, FixedBytes};
use rocket::form::{FromForm, FromFormField};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct OrdersPaginationParams {
    #[field(name = "page")]
    #[param(example = 1)]
    pub page: Option<u16>,
    #[field(name = "pageSize")]
    #[param(example = 20)]
    pub page_size: Option<u16>,
    /// `wrapped` (default) returns raw on-chain `ioRatio` values. `unwrapped`
    /// converts the ratio using the latest `assetsPerShare` snapshot for any
    /// wrapped side of the order pair.
    #[field(name = "denomination")]
    pub denomination: Option<Denomination>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, FromFormField, ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum OrderSide {
    Input,
    Output,
}

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct OrdersByTokenParams {
    #[field(name = "side")]
    pub side: Option<OrderSide>,
    #[field(name = "page")]
    #[param(example = 1)]
    pub page: Option<u16>,
    #[field(name = "pageSize")]
    #[param(example = 20)]
    pub page_size: Option<u16>,
    /// `wrapped` (default) returns raw on-chain `ioRatio` values. `unwrapped`
    /// converts the ratio using the latest `assetsPerShare` snapshot for any
    /// wrapped side of the order pair.
    #[field(name = "denomination")]
    pub denomination: Option<Denomination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderSummary {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: Address,
    #[schema(value_type = String, example = "0x01")]
    pub order_bytes: Bytes,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
    #[schema(example = "500000")]
    pub output_vault_balance: String,
    /// Simulated max output from on-chain quote (smaller than vault balance for DCA/strategy orders).
    /// Falls back to output_vault_balance when no quote is available.
    #[schema(example = "100")]
    pub max_output: String,
    #[schema(example = "0.0005")]
    pub io_ratio: String,
    #[schema(example = 1718452800)]
    pub created_at: u64,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub orderbook_id: Address,
    /// Denomination applied to `io_ratio`. Mirrors the `denomination` query
    /// parameter — `wrapped` for raw on-chain ratios, `unwrapped` for ratios
    /// rescaled by the wrapped exchange rate.
    #[serde(default)]
    pub denomination: Denomination,
    /// Per-order `assetsPerShare` rate used when
    /// `denomination == "unwrapped"`. Decimal string; `None` for `wrapped` or
    /// when neither side of the order pair is wrapped (no conversion
    /// required).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "1.04", value_type = Option<String>)]
    pub assets_per_share: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrdersPagination {
    #[schema(example = 1)]
    pub page: u32,
    #[schema(example = 20)]
    pub page_size: u32,
    #[schema(example = 100)]
    pub total_orders: u64,
    #[schema(example = 5)]
    pub total_pages: u64,
    #[schema(example = true)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrdersListResponse {
    pub orders: Vec<OrderSummary>,
    pub pagination: OrdersPagination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderByTxEntry {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: Address,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub orderbook_id: Address,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrdersByTxResponse {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: FixedBytes<32>,
    #[schema(example = 12345678)]
    pub block_number: u64,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    pub orders: Vec<OrderByTxEntry>,
}
