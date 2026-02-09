use crate::types::common::TokenRef;
use rocket::form::FromForm;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, FromForm, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct OrdersPaginationParams {
    #[field(name = "page")]
    #[serde(default)]
    #[param(example = 1)]
    pub page: Option<u32>,
    #[field(name = "pageSize")]
    #[serde(default = "crate::types::common::default_page_size")]
    #[param(example = 20)]
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrderSummary {
    #[schema(example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: String,
    #[schema(example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: String,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
    #[schema(example = "500000")]
    pub output_vault_balance: String,
    #[schema(example = "0.0005")]
    pub io_ratio: String,
    #[schema(example = 1718452800)]
    pub created_at: u64,
    #[schema(example = "0xorderbook")]
    pub orderbook_id: String,
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
    #[schema(example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub order_hash: String,
    #[schema(example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub owner: String,
    #[schema(example = "0xorderbook")]
    pub orderbook_id: String,
    pub input_token: TokenRef,
    pub output_token: TokenRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrdersByTxResponse {
    #[schema(example = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890ab")]
    pub tx_hash: String,
    #[schema(example = 12345678)]
    pub block_number: u64,
    #[schema(example = 1718452800)]
    pub timestamp: u64,
    pub orders: Vec<OrderByTxEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pagination_defaults() {
        let json = r#"{}"#;
        let params: OrdersPaginationParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.page_size, Some(20));
        assert!(params.page.is_none());
    }
}
