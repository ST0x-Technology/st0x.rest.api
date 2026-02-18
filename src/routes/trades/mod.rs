pub(crate) mod get_by_address;
pub(crate) mod get_by_tx;

use crate::error::ApiError;
use alloy::primitives::B256;
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::trades::RaindexTrade;
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
use rocket::Route;

#[async_trait(?Send)]
pub(crate) trait TradesTxDataSource {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<Vec<RaindexTrade>, ApiError>;
}

pub(crate) struct RaindexTradesTxDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait(?Send)]
impl TradesTxDataSource for RaindexTradesTxDataSource<'_> {
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
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_by_tx::get_trades_by_tx,
        get_by_address::get_trades_by_address
    ]
}
