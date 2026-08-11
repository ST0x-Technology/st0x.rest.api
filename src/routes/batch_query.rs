use std::str::FromStr;

use alloy::primitives::Address;
use rain_orderbook_common::raindex_client::RaindexClient;

use crate::error::ApiError;

pub(crate) fn validate_configured_chain(
    client: &RaindexClient,
    chain_id: u32,
) -> Result<(), ApiError> {
    let configured_chain_ids = client.get_unique_chain_ids().map_err(|error| {
        tracing::error!(%error, "failed to read configured chain IDs");
        ApiError::Internal("failed to validate chainId".into())
    })?;
    if configured_chain_ids.contains(&chain_id) {
        Ok(())
    } else {
        tracing::warn!(chain_id, "unsupported chainId");
        Err(ApiError::BadRequest("unsupported chainId".into()))
    }
}

pub(crate) fn parse_canonical_addresses(
    field: &'static str,
    values: Vec<String>,
    max: usize,
) -> Result<Vec<Address>, ApiError> {
    if values.len() > max {
        return Err(ApiError::BadRequest(format!(
            "{field} must contain at most {max} entries"
        )));
    }

    let original_len = values.len();
    let mut addresses = values
        .into_iter()
        .map(|value| {
            Address::from_str(&value).map_err(|error| {
                tracing::warn!(field, input = %value, %error, "invalid address in batch query");
                ApiError::BadRequest(format!("invalid address in {field}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    addresses.sort_unstable();
    addresses.dedup();
    if addresses.len() != original_len {
        tracing::info!(
            field,
            supplied_count = original_len,
            canonical_count = addresses.len(),
            "deduplicated batch address filter"
        );
    }
    Ok(addresses)
}
