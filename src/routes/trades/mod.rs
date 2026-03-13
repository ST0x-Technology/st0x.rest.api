pub(crate) mod get_by_address;
pub(crate) mod get_by_tx;

use crate::error::ApiError;
use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::trades::{RaindexTrade, RaindexTradesListResult};
use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
use rocket::Route;

#[async_trait(?Send)]
pub(crate) trait TradesDataSource {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<Vec<RaindexTrade>, ApiError>;
    async fn get_trades_for_owner(
        &self,
        owner: Address,
        pagination: PaginationParams,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError>;
}

pub(crate) struct RaindexTradesDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait(?Send)]
impl TradesDataSource for RaindexTradesDataSource<'_> {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<Vec<RaindexTrade>, ApiError> {
        match self
            .client
            .get_trades_for_transaction(None, None, tx_hash)
            .await
        {
            Ok(result) => Ok(result.trades().to_vec()),
            Err(RaindexError::TransactionIndexingTimeout { tx_hash, attempts }) => {
                tracing::info!(
                    tx_hash = %tx_hash,
                    attempts = attempts,
                    "transaction not yet indexed"
                );
                Err(ApiError::NotYetIndexed(format!(
                    "transaction {tx_hash:#x} not yet indexed after {attempts} attempts"
                )))
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to query trades for transaction");
                Err(ApiError::Internal("failed to query trades".into()))
            }
        }
    }

    async fn get_trades_for_owner(
        &self,
        owner: Address,
        pagination: PaginationParams,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError> {
        self.client
            .get_trades_for_owner(None, None, owner, pagination, time_filter)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades for owner");
                ApiError::Internal("failed to query trades".into())
            })
    }
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_by_tx::get_trades_by_tx,
        get_by_address::get_trades_by_address
    ]
}
