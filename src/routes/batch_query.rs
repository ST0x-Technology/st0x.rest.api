use std::collections::HashSet;
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

pub(crate) fn unique_subgraph_count(
    client: &RaindexClient,
    chain_id: Option<u32>,
) -> Result<usize, ApiError> {
    unique_subgraph_count_for_raindexes(client, chain_id, &[])
}

pub(crate) fn unique_subgraph_count_for_raindexes(
    client: &RaindexClient,
    chain_id: Option<u32>,
    raindex_addresses: &[Address],
) -> Result<usize, ApiError> {
    let raindexes = client.get_all_raindexes().map_err(|error| {
        tracing::error!(chain_id, %error, "failed to inspect batch query subgraph scope");
        ApiError::Internal("failed to inspect batch query scope".into())
    })?;
    let requested = raindex_addresses.iter().copied().collect::<HashSet<_>>();
    let configured = raindexes
        .values()
        .filter(|raindex| chain_id.is_none_or(|id| raindex.network.chain_id == id))
        .filter(|raindex| requested.is_empty() || requested.contains(&raindex.address))
        .collect::<Vec<_>>();

    if !requested.is_empty()
        && configured
            .iter()
            .map(|raindex| raindex.address)
            .collect::<HashSet<_>>()
            != requested
    {
        tracing::warn!(
            chain_id,
            requested_count = requested.len(),
            configured_count = configured.len(),
            "raindexAddresses contains an address outside the selected network"
        );
        return Err(ApiError::BadRequest(
            "raindexAddresses contains an address not configured for chainId".into(),
        ));
    }

    Ok(configured
        .into_iter()
        .map(|raindex| raindex.subgraph.url.as_str())
        .collect::<HashSet<_>>()
        .len())
}

pub(crate) fn require_single_subgraph_for_pagination(
    resource: &'static str,
    chain_id: u32,
    subgraph_count: usize,
) -> Result<(), ApiError> {
    if subgraph_count == 1 {
        return Ok(());
    }

    tracing::warn!(
        resource,
        chain_id,
        subgraph_count,
        "stable batch pagination is unavailable for this query scope"
    );
    Err(ApiError::BadRequest(format!(
        "{resource} batch pagination requires a network backed by exactly one subgraph"
    )))
}
