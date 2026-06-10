use crate::erc4626::batch_share_ratios;
use crate::error::ApiError;
use alloy::primitives::Address;
use rain_erc::erc4626::{Erc4626BatchItem, Erc4626BatchResponse, Erc4626BatchVault};
use rain_orderbook_app_settings::token::TokenCfg;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WrapRatioResponse {
    #[schema(value_type = String, example = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2")]
    pub(crate) share_address: Address,
    #[schema(value_type = String, example = "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe")]
    asset_address: Address,
    #[schema(example = "1.0")]
    pub(crate) assets_per_share: String,
    #[schema(example = 123)]
    block_number: u64,
    #[schema(nullable = true, example = 456)]
    block_timestamp: Option<u64>,
    #[schema(example = "1717351200")]
    captured_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WrapRatioValue {
    pub share_address: Address,
    pub assets_per_share: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WrapRatioLookupError {
    #[error("missing registry extensions.unwrappedAddress")]
    MissingUnwrappedAddress,
    #[error("invalid registry extensions.unwrappedAddress: {0}")]
    InvalidUnwrappedAddress(String),
    #[error("registry unwrappedAddress does not match ERC4626 asset")]
    AssetMismatch,
    #[error("failed to read ERC4626 ratio: {0}")]
    BatchItem(String),
    #[error("ERC4626 batch response did not include requested token")]
    MissingBatchItem,
    #[error("ERC4626 batch response included duplicate requested token")]
    DuplicateBatchItem,
}

impl WrapRatioLookupError {
    pub(crate) fn into_api_error(self) -> ApiError {
        match self {
            WrapRatioLookupError::MissingUnwrappedAddress => {
                ApiError::Internal("token is missing unwrappedAddress".into())
            }
            WrapRatioLookupError::InvalidUnwrappedAddress(_) => {
                ApiError::Internal("token has invalid unwrappedAddress".into())
            }
            WrapRatioLookupError::AssetMismatch => {
                ApiError::Internal("token unwrappedAddress does not match ERC4626 asset".into())
            }
            WrapRatioLookupError::BatchItem(_) => {
                ApiError::Internal("failed to read ERC4626 ratio".into())
            }
            WrapRatioLookupError::MissingBatchItem | WrapRatioLookupError::DuplicateBatchItem => {
                ApiError::Internal("failed to read ERC4626 ratio".into())
            }
        }
    }

    pub(crate) fn batch_message(&self) -> String {
        match self {
            WrapRatioLookupError::MissingUnwrappedAddress => {
                "missing registry unwrappedAddress".to_string()
            }
            WrapRatioLookupError::InvalidUnwrappedAddress(_) => {
                "invalid registry unwrappedAddress".to_string()
            }
            WrapRatioLookupError::AssetMismatch => {
                "registry unwrappedAddress does not match ERC4626 asset".to_string()
            }
            WrapRatioLookupError::BatchItem(_)
            | WrapRatioLookupError::MissingBatchItem
            | WrapRatioLookupError::DuplicateBatchItem => {
                "failed to read ERC4626 ratio".to_string()
            }
        }
    }
}

pub(crate) fn is_st0x_token(token: &TokenCfg) -> bool {
    token
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("category"))
        .and_then(Value::as_str)
        == Some("ST0x")
}

pub(crate) fn unwrapped_address(token: &TokenCfg) -> Result<Address, WrapRatioLookupError> {
    let value = token
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("unwrappedAddress"))
        .ok_or(WrapRatioLookupError::MissingUnwrappedAddress)?;

    let Some(address) = value.as_str() else {
        return Err(WrapRatioLookupError::InvalidUnwrappedAddress(
            value.to_string(),
        ));
    };

    address
        .parse()
        .map_err(|_| WrapRatioLookupError::InvalidUnwrappedAddress(address.to_string()))
}

fn assets_per_share_display(assets_display: String) -> String {
    if assets_display == "1" {
        "1.0".to_string()
    } else {
        assets_display
    }
}

pub(crate) struct WrapRatioBatchInput<'a> {
    pub token: &'a TokenCfg,
    pub expected_asset_address: Address,
}

pub(crate) struct WrapRatioMetadata {
    block_number: u64,
    block_timestamp: Option<u64>,
    captured_at: String,
}

impl WrapRatioMetadata {
    pub(crate) fn from_batch_response(response: &Erc4626BatchResponse) -> Self {
        Self {
            block_number: response.block_number,
            block_timestamp: Some(response.block_timestamp),
            captured_at: response.captured_at.to_string(),
        }
    }
}

pub(crate) struct WrapRatioBatchGroupResponse {
    pub input_indices: Vec<usize>,
    pub response: Erc4626BatchResponse,
}

pub(crate) fn find_wrap_ratio_item(
    items: &[Erc4626BatchItem],
    share_address: Address,
) -> Result<&Erc4626BatchItem, WrapRatioLookupError> {
    let mut matches = items
        .iter()
        .filter(|item| item.vault_address == share_address);
    let Some(item) = matches.next() else {
        tracing::error!(
            share_address = %share_address,
            "ERC4626 batch response did not include requested token"
        );
        return Err(WrapRatioLookupError::MissingBatchItem);
    };

    if matches.next().is_some() {
        tracing::error!(
            share_address = %share_address,
            "ERC4626 batch response included duplicate requested token"
        );
        return Err(WrapRatioLookupError::DuplicateBatchItem);
    }

    Ok(item)
}

pub(crate) fn build_wrap_ratio_response(
    item: &Erc4626BatchItem,
    expected_asset_address: Address,
    metadata: &WrapRatioMetadata,
) -> Result<WrapRatioResponse, WrapRatioLookupError> {
    if !item.success {
        return Err(WrapRatioLookupError::BatchItem(
            item.error
                .clone()
                .unwrap_or_else(|| "failed to read ERC4626 ratio".to_string()),
        ));
    }

    let Some(ratio) = item.data.as_ref() else {
        return Err(WrapRatioLookupError::BatchItem(
            "missing ERC4626 batch item data".to_string(),
        ));
    };

    if ratio.asset_address != expected_asset_address {
        tracing::error!(
            share_address = %item.vault_address,
            expected_asset_address = %expected_asset_address,
            erc4626_asset_address = %ratio.asset_address,
            "registry unwrappedAddress does not match ERC4626 asset"
        );
        return Err(WrapRatioLookupError::AssetMismatch);
    }

    Ok(WrapRatioResponse {
        share_address: item.vault_address,
        asset_address: expected_asset_address,
        assets_per_share: assets_per_share_display(ratio.assets_display.clone()),
        block_number: metadata.block_number,
        block_timestamp: metadata.block_timestamp,
        captured_at: metadata.captured_at.clone(),
    })
}

pub(crate) async fn read_wrap_ratios_batch(
    inputs: &[WrapRatioBatchInput<'_>],
) -> Result<Vec<WrapRatioBatchGroupResponse>, ApiError> {
    let mut inputs_by_chain: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (index, input) in inputs.iter().enumerate() {
        inputs_by_chain
            .entry(input.token.network.chain_id)
            .or_default()
            .push(index);
    }

    let mut responses = Vec::with_capacity(inputs_by_chain.len());
    for (chain_id, input_indices) in inputs_by_chain {
        let Some(first_input_index) = input_indices.first() else {
            continue;
        };
        let Some(first_input) = inputs.get(*first_input_index) else {
            continue;
        };

        let vaults = input_indices
            .iter()
            .filter_map(|index| inputs.get(*index))
            .map(|input| Erc4626BatchVault::new(input.token.address))
            .collect();

        let response = batch_share_ratios(&first_input.token.network.rpcs, vaults, None)
            .await
            .map_err(|error| {
                tracing::error!(
                    chain_id,
                    error = %error,
                    "failed to read ERC4626 batch ratios"
                );
                ApiError::Internal("failed to read ERC4626 ratios".into())
            })?;

        responses.push(WrapRatioBatchGroupResponse {
            input_indices,
            response,
        });
    }

    Ok(responses)
}

pub(crate) async fn read_wrap_ratio_values_for_addresses(
    tokens: &[TokenCfg],
    share_addresses: &[Address],
) -> Result<HashMap<Address, WrapRatioValue>, ApiError> {
    let requested: HashSet<Address> = share_addresses.iter().copied().collect();
    let mut inputs = Vec::new();

    for token in tokens
        .iter()
        .filter(|token| requested.contains(&token.address) && is_st0x_token(token))
    {
        let expected_asset_address = unwrapped_address(token).map_err(|error| {
            tracing::error!(
                share_address = %token.address,
                error = %error,
                "failed to read wrapped token ratio"
            );
            error.into_api_error()
        })?;
        inputs.push(WrapRatioBatchInput {
            token,
            expected_asset_address,
        });
    }

    let mut ratios = HashMap::new();
    for group in read_wrap_ratios_batch(&inputs).await? {
        let metadata = WrapRatioMetadata::from_batch_response(&group.response);

        if group.response.items.len() != group.input_indices.len() {
            tracing::error!(
                expected = group.input_indices.len(),
                actual = group.response.items.len(),
                "ERC4626 batch response item count mismatch"
            );
        }

        for index in group.input_indices {
            let Some(input) = inputs.get(index) else {
                continue;
            };

            let row = find_wrap_ratio_item(&group.response.items, input.token.address)
                .and_then(|item| {
                    build_wrap_ratio_response(item, input.expected_asset_address, &metadata)
                })
                .map_err(|error| {
                    tracing::error!(
                        share_address = %input.token.address,
                        error = %error,
                        "failed to read wrapped token ratio"
                    );
                    error.into_api_error()
                })?;

            ratios.insert(
                row.share_address,
                WrapRatioValue {
                    share_address: row.share_address,
                    assets_per_share: row.assets_per_share,
                },
            );
        }
    }

    Ok(ratios)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Address, U256};
    use rain_erc::erc4626::{Erc4626BatchItem, Erc4626ShareAssetConversion};
    use rain_orderbook_app_settings::network::NetworkCfg;
    use serde_json::json;
    use std::sync::Arc;

    const WT_MSTR: Address = address!("ff05e1bd696900dc6a52ca35ca61bb1024eda8e2");
    const T_MSTR: Address = address!("013b782f402d61aa1004cca95b9f5bb402c9d5fe");
    const WT_BAD: Address = address!("1111111111111111111111111111111111111111");

    fn token(address: Address, extensions: Option<serde_json::Value>) -> TokenCfg {
        let extensions = extensions.map(|value| {
            serde_json::from_value(value).expect("extensions fixture should be an object")
        });
        let mut network = NetworkCfg::dummy();
        network.key = "base".to_string();
        network.chain_id = 8453;
        network.network_id = Some(8453);
        network.currency = Some("ETH".to_string());

        TokenCfg {
            document: rain_orderbook_app_settings::yaml::default_document(),
            key: format!("{address:#x}"),
            address,
            network: Arc::new(network),
            decimals: Some(18),
            label: Some("Wrapped MicroStrategy ST0x".to_string()),
            symbol: Some("wtMSTR".to_string()),
            logo_uri: None,
            extensions,
        }
    }

    fn batch_item(vault_address: Address) -> Erc4626BatchItem {
        Erc4626BatchItem {
            vault_address,
            success: false,
            data: None,
            error: Some("failed".to_string()),
        }
    }

    fn successful_batch_item(vault_address: Address, asset_address: Address) -> Erc4626BatchItem {
        Erc4626BatchItem {
            vault_address,
            success: true,
            data: Some(Erc4626ShareAssetConversion {
                share_token_address: vault_address,
                share_token_decimals: 18,
                asset_address,
                asset_decimals: 18,
                shares: U256::from(1_000_000_000_000_000_000u128),
                shares_display: "1".to_string(),
                assets: U256::from(1_000_000_000_000_000_000u128),
                assets_display: "1".to_string(),
            }),
            error: None,
        }
    }

    fn metadata() -> WrapRatioMetadata {
        WrapRatioMetadata {
            block_number: 123,
            block_timestamp: Some(456),
            captured_at: "2026-06-02T13:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_is_st0x_token_reads_category_extension() {
        let st0x = token(
            WT_MSTR,
            Some(json!({
                "category": "ST0x",
                "unwrappedAddress": format!("{T_MSTR:#x}")
            })),
        );
        let usdc = token(WT_BAD, None);

        assert!(is_st0x_token(&st0x));
        assert!(!is_st0x_token(&usdc));
    }

    #[test]
    fn test_unwrapped_address_parses_registry_extension() {
        let wrapped = token(
            WT_MSTR,
            Some(json!({
                "category": "ST0x",
                "unwrappedAddress": format!("{T_MSTR:#x}")
            })),
        );

        let parsed = unwrapped_address(&wrapped).expect("unwrapped address should parse");
        assert_eq!(parsed, T_MSTR);
    }

    #[test]
    fn test_unwrapped_address_rejects_missing_and_invalid_values() {
        let missing = token(WT_MSTR, Some(json!({ "category": "ST0x" })));
        let invalid = token(
            WT_MSTR,
            Some(json!({
                "category": "ST0x",
                "unwrappedAddress": "not-an-address"
            })),
        );

        assert!(matches!(
            unwrapped_address(&missing),
            Err(WrapRatioLookupError::MissingUnwrappedAddress)
        ));
        assert!(matches!(
            unwrapped_address(&invalid),
            Err(WrapRatioLookupError::InvalidUnwrappedAddress(_))
        ));
    }

    #[test]
    fn test_find_wrap_ratio_item_rejects_missing_and_duplicate_items() {
        let items = vec![batch_item(WT_BAD)];
        let missing = find_wrap_ratio_item(&items, WT_MSTR).expect_err("missing item");
        assert!(matches!(missing, WrapRatioLookupError::MissingBatchItem));

        let items = vec![batch_item(WT_MSTR), batch_item(WT_MSTR)];
        let duplicate = find_wrap_ratio_item(&items, WT_MSTR).expect_err("duplicate item");
        assert!(matches!(
            duplicate,
            WrapRatioLookupError::DuplicateBatchItem
        ));
    }

    #[test]
    fn test_build_wrap_ratio_response_maps_successful_item() {
        let item = successful_batch_item(WT_MSTR, T_MSTR);
        let response =
            build_wrap_ratio_response(&item, T_MSTR, &metadata()).expect("ratio should build");

        assert_eq!(response.share_address, WT_MSTR);
        assert_eq!(response.asset_address, T_MSTR);
        assert_eq!(response.assets_per_share, "1.0");
        assert_eq!(response.block_number, 123);
        assert_eq!(response.block_timestamp, Some(456));
        assert_eq!(response.captured_at, "2026-06-02T13:00:00Z");
    }

    #[test]
    fn test_build_wrap_ratio_response_rejects_asset_mismatch() {
        let item = successful_batch_item(WT_MSTR, WT_BAD);

        let error =
            build_wrap_ratio_response(&item, T_MSTR, &metadata()).expect_err("asset mismatch");
        assert!(matches!(error, WrapRatioLookupError::AssetMismatch));
    }

    #[test]
    fn test_batch_item_errors_map_to_api_and_batch_messages() {
        let lookup_error = WrapRatioLookupError::BatchItem("rpc failed".to_string());
        assert_eq!(lookup_error.batch_message(), "failed to read ERC4626 ratio");

        let api_error = WrapRatioLookupError::MissingUnwrappedAddress.into_api_error();
        assert!(
            matches!(api_error, ApiError::Internal(message) if message == "token is missing unwrappedAddress")
        );
    }
}
