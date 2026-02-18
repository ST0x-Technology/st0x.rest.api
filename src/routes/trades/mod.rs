pub(crate) mod get_by_address;
pub(crate) mod get_by_tx;

use crate::error::ApiError;
use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::trades::{RaindexTrade, RaindexTradesListResult};
use rain_orderbook_common::raindex_client::types::{
    OrderbookIdentifierParams, PaginationParams, TimeFilter,
};
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
        let orderbooks = self.client.get_all_orderbooks().map_err(|e| {
            tracing::error!(error = %e, "failed to get orderbooks");
            ApiError::Internal("failed to get orderbooks".into())
        })?;

        let mut all_trades: Vec<RaindexTrade> = Vec::new();
        for ob_cfg in orderbooks.values() {
            let chain_id = ob_cfg.network.chain_id;
            let address = ob_cfg.address;
            match self
                .client
                .get_trades_for_transaction(chain_id, address, tx_hash, None, None)
                .await
            {
                Ok(trades) => all_trades.extend(trades),
                Err(RaindexError::TradesIndexingTimeout { tx_hash, attempts }) => {
                    tracing::info!(
                        tx_hash = %tx_hash,
                        attempts = attempts,
                        "transaction not yet indexed"
                    );
                    return Err(ApiError::NotYetIndexed(format!(
                        "transaction {tx_hash:#x} not yet indexed after {attempts} attempts"
                    )));
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to query trades for transaction");
                    return Err(ApiError::Internal("failed to query trades".into()));
                }
            }
        }
        Ok(all_trades)
    }

    async fn get_trades_for_owner(
        &self,
        owner: Address,
        pagination: PaginationParams,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError> {
        let orderbooks = self.client.get_all_orderbooks().map_err(|e| {
            tracing::error!(error = %e, "failed to get orderbooks");
            ApiError::Internal("failed to get orderbooks".into())
        })?;

        let mut all_trades: Vec<RaindexTrade> = Vec::new();
        let mut total_count: u64 = 0;

        for ob_cfg in orderbooks.values() {
            let ob_id_params =
                OrderbookIdentifierParams::new(ob_cfg.network.chain_id, ob_cfg.address.to_string());
            match self
                .client
                .get_trades_for_owner(
                    ob_id_params,
                    owner.to_string(),
                    pagination.clone(),
                    time_filter.clone(),
                )
                .await
            {
                Ok(result) => {
                    all_trades.extend(result.trades());
                    total_count += result.total_count();
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to query trades for owner");
                    return Err(ApiError::Internal("failed to query trades".into()));
                }
            }
        }

        Ok(RaindexTradesListResult::new(all_trades, total_count))
    }
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_by_tx::get_trades_by_tx,
        get_by_address::get_trades_by_address
    ]
}
