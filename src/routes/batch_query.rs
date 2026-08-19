use std::str::FromStr;

use alloy::primitives::Address;
use rain_orderbook_common::raindex_client::RaindexClient;

use crate::error::ApiError;
use crate::routes::validate_raindex_chain_id;

pub(crate) fn validate_configured_chain(
    client: &RaindexClient,
    chain_id: u32,
) -> Result<(), ApiError> {
    validate_raindex_chain_id(client, chain_id).map(|_| ())
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
