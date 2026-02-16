mod cancel;
mod deploy_dca;
mod deploy_solver;
mod get_order;

use crate::error::ApiError;
use alloy::primitives::B256;
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::order_quotes::RaindexOrderQuote;
use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};
use rain_orderbook_common::raindex_client::trades::RaindexTrade;
use rain_orderbook_common::raindex_client::RaindexClient;
use rocket::Route;

#[async_trait(?Send)]
pub(crate) trait OrderDataSource {
    async fn get_orders_by_hash(&self, hash: B256) -> Result<Vec<RaindexOrder>, ApiError>;
    async fn get_order_quotes(
        &self,
        order: &RaindexOrder,
    ) -> Result<Vec<RaindexOrderQuote>, ApiError>;
    async fn get_order_trades(&self, order: &RaindexOrder) -> Result<Vec<RaindexTrade>, ApiError>;
}

pub(crate) struct RaindexOrderDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait(?Send)]
impl<'a> OrderDataSource for RaindexOrderDataSource<'a> {
    async fn get_orders_by_hash(&self, hash: B256) -> Result<Vec<RaindexOrder>, ApiError> {
        let filters = GetOrdersFilters {
            order_hash: Some(hash),
            ..Default::default()
        };
        self.client
            .get_orders(None, Some(filters), None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query orders");
                ApiError::Internal("failed to query orders".into())
            })
    }

    async fn get_order_quotes(
        &self,
        order: &RaindexOrder,
    ) -> Result<Vec<RaindexOrderQuote>, ApiError> {
        order.get_quotes(None, None).await.map_err(|e| {
            tracing::error!(error = %e, "failed to query order quotes");
            ApiError::Internal("failed to query order quotes".into())
        })
    }

    async fn get_order_trades(&self, order: &RaindexOrder) -> Result<Vec<RaindexTrade>, ApiError> {
        order.get_trades_list(None, None, None).await.map_err(|e| {
            tracing::error!(error = %e, "failed to query order trades");
            ApiError::Internal("failed to query order trades".into())
        })
    }
}

pub use cancel::*;
pub use deploy_dca::*;
pub use deploy_solver::*;
pub use get_order::*;

pub fn routes() -> Vec<Route> {
    rocket::routes![
        deploy_dca::post_order_dca,
        deploy_solver::post_order_solver,
        get_order::get_order,
        cancel::post_order_cancel
    ]
}
