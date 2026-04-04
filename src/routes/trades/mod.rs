pub(crate) mod get_by_address;
pub(crate) mod get_by_tx;

use crate::error::ApiError;
use alloy::primitives::B256;
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::trades::RaindexTradesListResult;
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
use rocket::Route;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxIndexState {
    Indexed,
    NotYetIndexed,
}

#[async_trait]
pub(crate) trait TradesDataSource: Send + Sync {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<RaindexTradesListResult, ApiError>;

    async fn check_tx_index_state(&self, tx_hash: B256) -> Result<TxIndexState, ApiError>;
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

    async fn check_tx_index_state(&self, tx_hash: B256) -> Result<TxIndexState, ApiError> {
        let orderbooks = self.client.get_all_orderbooks().map_err(|e| {
            tracing::error!(error = %e, "failed to get orderbooks");
            ApiError::Internal("failed to query transaction".into())
        })?;

        let mut saw_timeout = false;

        for orderbook in orderbooks.values() {
            match self
                .client
                .get_transaction(
                    orderbook.network.chain_id,
                    orderbook.address,
                    tx_hash,
                    Some(1),
                    Some(0),
                )
                .await
            {
                Ok(_) => return Ok(TxIndexState::Indexed),
                Err(RaindexError::TransactionIndexingTimeout { .. }) => {
                    saw_timeout = true;
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        tx_hash = %tx_hash,
                        chain_id = orderbook.network.chain_id,
                        orderbook = %orderbook.address,
                        "failed to query transaction status"
                    );
                    return Err(ApiError::Internal("failed to query transaction".into()));
                }
            }
        }

        if saw_timeout {
            return Ok(TxIndexState::NotYetIndexed);
        }

        Ok(TxIndexState::Indexed)
    }
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_by_tx::get_trades_by_tx,
        get_by_address::get_trades_by_address
    ]
}
