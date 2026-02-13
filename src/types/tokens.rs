use alloy::primitives::Address;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub address: Address,
    #[schema(example = "USDC")]
    pub symbol: String,
    #[schema(example = "USD Coin")]
    pub name: String,
    #[serde(rename = "ISIN")]
    #[schema(example = "US1234567890")]
    pub isin: String,
    #[schema(example = 6)]
    pub decimals: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenListResponse {
    pub tokens: Vec<TokenInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteTokenList {
    pub tokens: Vec<RemoteToken>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteToken {
    pub chain_id: u32,
    pub address: Address,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_info_serializes_isin_uppercase() {
        let token = TokenInfo {
            address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                .parse()
                .unwrap(),
            symbol: "USDC".into(),
            name: "USD Coin".into(),
            isin: "US1234567890".into(),
            decimals: 6,
        };
        let json = serde_json::to_string(&token).unwrap();
        assert!(json.contains("\"ISIN\""));
        assert!(!json.contains("\"isin\""));
    }
}
