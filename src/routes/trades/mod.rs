pub(crate) mod get_by_address;
pub(crate) mod get_by_tx;

use crate::error::ApiError;
use alloy::primitives::B256;
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::trades::RaindexTradesListResult;
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
use rocket::Route;

#[async_trait]
pub(crate) trait TradesDataSource: Send + Sync {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<RaindexTradesListResult, ApiError>;
}

pub(crate) struct RaindexTradesDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait]
impl TradesDataSource for RaindexTradesDataSource<'_> {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<RaindexTradesListResult, ApiError> {
        self.client
            .get_trades_for_transaction(None, None, tx_hash)
            .await
            .map_err(|e| match e {
                RaindexError::TransactionIndexingTimeout { tx_hash, attempts } => {
                    tracing::info!(
                        tx_hash = %tx_hash,
                        attempts = attempts,
                        "transaction not yet indexed"
                    );
                    ApiError::NotYetIndexed(format!(
                        "transaction {tx_hash:#x} not yet indexed after {attempts} attempts"
                    ))
                }
                other => {
                    tracing::error!(error = %other, "failed to query trades for transaction");
                    ApiError::Internal("failed to query trades".into())
                }
            })
    }
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_by_tx::get_trades_by_tx,
        get_by_address::get_trades_by_address
    ]
}
