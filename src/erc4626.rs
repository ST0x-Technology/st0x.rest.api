use alloy::primitives::Address;
use alloy::providers::ProviderBuilder;
use rain_erc::erc4626::{self, Erc4626BatchResponse, Erc4626BatchVault};
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub(crate) enum Erc4626ReadError {
    #[error("no RPC URLs configured")]
    MissingRpc,
    #[error("all configured RPC URLs failed: {0}")]
    AllRpcsFailed(String),
}

pub(crate) async fn batch_share_ratios(
    rpcs: &[Url],
    vaults: Vec<Erc4626BatchVault>,
    multicall3_address: Option<Address>,
) -> Result<Erc4626BatchResponse, Erc4626ReadError> {
    if rpcs.is_empty() {
        return Err(Erc4626ReadError::MissingRpc);
    }

    let mut errors = Vec::new();
    for rpc in rpcs {
        let provider = ProviderBuilder::new().connect_http(rpc.clone());
        match erc4626::batch_share_ratios(&provider, vaults.clone(), multicall3_address).await {
            Ok(response) => return Ok(response),
            Err(error) => errors.push(format!("{rpc}: {error}")),
        }
    }

    Err(Erc4626ReadError::AllRpcsFailed(errors.join("; ")))
}
