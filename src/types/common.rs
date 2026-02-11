use alloy::primitives::{Address, Bytes, FixedBytes};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenRef {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub address: Address,
    #[schema(example = "USDC")]
    pub symbol: String,
    #[schema(example = 6)]
    pub decimals: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    #[schema(value_type = String, example = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")]
    pub token: Address,
    #[schema(value_type = String, example = "0x1234567890abcdef1234567890abcdef12345678")]
    pub spender: Address,
    #[schema(example = "1000000")]
    pub amount: String,
    #[schema(example = "USDC")]
    pub symbol: String,
    #[schema(value_type = String, example = "0xabcdef...")]
    pub approval_data: Bytes,
}

#[derive(Debug)]
pub struct ValidatedAddress(pub Address);

impl<'a> rocket::request::FromParam<'a> for ValidatedAddress {
    type Error = &'a str;

    fn from_param(param: &'a str) -> Result<Self, Self::Error> {
        param
            .parse::<Address>()
            .map(ValidatedAddress)
            .map_err(|_| param)
    }
}

#[derive(Debug)]
pub struct ValidatedFixedBytes(pub FixedBytes<32>);

impl<'a> rocket::request::FromParam<'a> for ValidatedFixedBytes {
    type Error = &'a str;

    fn from_param(param: &'a str) -> Result<Self, Self::Error> {
        param
            .parse::<FixedBytes<32>>()
            .map(ValidatedFixedBytes)
            .map_err(|_| param)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocket::request::FromParam;

    #[test]
    fn test_path_address_valid() {
        let result = ValidatedAddress::from_param("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert!(result.is_ok());
    }

    #[test]
    fn test_path_address_rejects_garbage() {
        let result = ValidatedAddress::from_param("not-an-address");
        assert!(result.is_err());
    }

    #[test]
    fn test_path_address_rejects_wrong_length() {
        let result = ValidatedAddress::from_param("0x833589");
        assert!(result.is_err());
    }

    #[test]
    fn test_path_address_rejects_non_hex() {
        let result = ValidatedAddress::from_param("0xZZZZ89fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert!(result.is_err());
    }

    #[test]
    fn test_path_fixed_bytes_valid() {
        let result = ValidatedFixedBytes::from_param(
            "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_path_fixed_bytes_rejects_garbage() {
        let result = ValidatedFixedBytes::from_param("not-a-hash");
        assert!(result.is_err());
    }

    #[test]
    fn test_path_fixed_bytes_rejects_wrong_length() {
        let result = ValidatedFixedBytes::from_param("0xabcdef");
        assert!(result.is_err());
    }

    #[test]
    fn test_path_fixed_bytes_rejects_non_hex() {
        let result = ValidatedFixedBytes::from_param(
            "0xZZZZef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
        );
        assert!(result.is_err());
    }
}
