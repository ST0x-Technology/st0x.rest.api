use alloy::primitives::{Address, FixedBytes};
use rocket::form::FromForm;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, Default, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct VaultsQueryParams {
    #[field(name = "owner")]
    #[param(required = true)]
    #[param(example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: Option<String>,
    #[field(name = "token")]
    #[param(example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub token: Option<String>,
    #[field(name = "hideZeroBalance")]
    #[param(example = false)]
    pub hide_zero_balance: Option<bool>,
    #[field(name = "page")]
    #[param(example = 1)]
    pub page: Option<u16>,
    #[field(name = "pageSize")]
    #[param(example = 100)]
    pub page_size: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultTokenResponse {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub address: Address,
    #[schema(example = "USD Coin")]
    pub name: Option<String>,
    #[schema(example = "USDC")]
    pub symbol: Option<String>,
    #[schema(example = 6)]
    pub decimals: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultTotalTokenResponse {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub address: Address,
    #[schema(example = "USDC")]
    pub symbol: Option<String>,
    #[schema(example = 6)]
    pub decimals: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultOrderRef {
    #[schema(value_type = String, example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: FixedBytes<32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultPositionResponse {
    #[schema(value_type = String, example = "0xabcdef")]
    pub id: String,
    #[schema(example = "123")]
    pub vault_id: String,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: Address,
    pub token: VaultTokenResponse,
    #[schema(example = "1000000")]
    pub balance: String,
    #[schema(value_type = String, example = "0xd2938e7c9fe3597f78832ce780feb61945c377d7")]
    pub orderbook: Address,
    pub orders_as_input: Vec<VaultOrderRef>,
    pub orders_as_output: Vec<VaultOrderRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultsPagination {
    #[schema(example = 1)]
    pub page: u32,
    #[schema(example = 100)]
    pub page_size: u32,
    #[schema(example = 123)]
    pub total_items: u64,
    #[schema(example = true)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultsResponse {
    pub vaults: Vec<VaultPositionResponse>,
    pub pagination: VaultsPagination,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultTotalResponse {
    pub token: VaultTotalTokenResponse,
    #[schema(example = "42000000000000000000")]
    pub total_balance: String,
    #[schema(example = 42)]
    pub vault_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VaultTotalsResponse {
    pub totals: Vec<VaultTotalResponse>,
}
