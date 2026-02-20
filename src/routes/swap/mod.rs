mod calldata;
mod quote;

use crate::error::ApiError;
use alloy::primitives::Address;
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::orders::{
    GetOrdersFilters, GetOrdersTokenFilter, RaindexOrder,
};
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_common::take_orders::{
    build_take_order_candidates_for_pair, TakeOrderCandidate,
};
use rocket::Route;

#[async_trait(?Send)]
pub(crate) trait SwapDataSource {
    async fn get_orders_for_pair(
        &self,
        input_token: Address,
        output_token: Address,
    ) -> Result<Vec<RaindexOrder>, ApiError>;

    async fn build_candidates_for_pair(
        &self,
        orders: &[RaindexOrder],
        input_token: Address,
        output_token: Address,
    ) -> Result<Vec<TakeOrderCandidate>, ApiError>;
}

pub(crate) struct RaindexSwapDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait(?Send)]
impl<'a> SwapDataSource for RaindexSwapDataSource<'a> {
    async fn get_orders_for_pair(
        &self,
        input_token: Address,
        output_token: Address,
    ) -> Result<Vec<RaindexOrder>, ApiError> {
        let filters = GetOrdersFilters {
            active: Some(true),
            tokens: Some(GetOrdersTokenFilter {
                inputs: Some(vec![input_token]),
                outputs: Some(vec![output_token]),
            }),
            ..Default::default()
        };
        self.client
            .get_orders(None, Some(filters), None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query orders for pair");
                ApiError::Internal("failed to query orders".into())
            })
    }

    async fn build_candidates_for_pair(
        &self,
        orders: &[RaindexOrder],
        input_token: Address,
        output_token: Address,
    ) -> Result<Vec<TakeOrderCandidate>, ApiError> {
        build_take_order_candidates_for_pair(orders, input_token, output_token, None, None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to build order candidates");
                ApiError::Internal("failed to build order candidates".into())
            })
    }
}

pub use calldata::*;
pub use quote::*;

pub fn routes() -> Vec<Route> {
    rocket::routes![quote::post_swap_quote, calldata::post_swap_calldata]
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::SwapDataSource;
    use crate::error::ApiError;
    use alloy::primitives::Address;
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::orders::RaindexOrder;
    use rain_orderbook_common::take_orders::TakeOrderCandidate;

    pub struct MockSwapDataSource {
        pub orders: Result<Vec<RaindexOrder>, ApiError>,
        pub candidates: Vec<TakeOrderCandidate>,
    }

    #[async_trait(?Send)]
    impl SwapDataSource for MockSwapDataSource {
        async fn get_orders_for_pair(
            &self,
            _input_token: Address,
            _output_token: Address,
        ) -> Result<Vec<RaindexOrder>, ApiError> {
            match &self.orders {
                Ok(orders) => Ok(orders.clone()),
                Err(_) => Err(ApiError::Internal("failed to query orders".into())),
            }
        }

        async fn build_candidates_for_pair(
            &self,
            _orders: &[RaindexOrder],
            _input_token: Address,
            _output_token: Address,
        ) -> Result<Vec<TakeOrderCandidate>, ApiError> {
            Ok(self.candidates.clone())
        }
    }
}
