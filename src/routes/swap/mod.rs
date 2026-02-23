mod calldata;
mod quote;

use crate::error::ApiError;
use crate::types::swap::SwapCalldataResponse;
use alloy::primitives::Address;
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::orders::{
    GetOrdersFilters, GetOrdersTokenFilter, RaindexOrder,
};
use rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest;
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_common::raindex_client::RaindexError;
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

    async fn get_calldata(
        &self,
        request: TakeOrdersRequest,
    ) -> Result<SwapCalldataResponse, ApiError>;
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

    async fn get_calldata(
        &self,
        request: TakeOrdersRequest,
    ) -> Result<SwapCalldataResponse, ApiError> {
        let result = self
            .client
            .get_take_orders_calldata(request)
            .await
            .map_err(map_raindex_error)?;

        if let Some(approval_info) = result.approval_info() {
            let formatted_amount = approval_info.formatted_amount().to_string();
            Ok(SwapCalldataResponse {
                to: approval_info.spender(),
                data: alloy::primitives::Bytes::new(),
                value: alloy::primitives::U256::ZERO,
                estimated_input: formatted_amount.clone(),
                approvals: vec![crate::types::common::Approval {
                    token: approval_info.token(),
                    spender: approval_info.spender(),
                    amount: formatted_amount,
                    symbol: String::new(),
                    approval_data: approval_info.calldata().clone(),
                }],
            })
        } else if let Some(take_orders_info) = result.take_orders_info() {
            let expected_sell = take_orders_info.expected_sell().format().map_err(|e| {
                tracing::error!(error = %e, "failed to format expected sell");
                ApiError::Internal("failed to format expected sell".into())
            })?;
            Ok(SwapCalldataResponse {
                to: take_orders_info.orderbook(),
                data: take_orders_info.calldata().clone(),
                value: alloy::primitives::U256::ZERO,
                estimated_input: expected_sell,
                approvals: vec![],
            })
        } else {
            Err(ApiError::Internal(
                "unexpected calldata result state".into(),
            ))
        }
    }
}

fn map_raindex_error(e: RaindexError) -> ApiError {
    match &e {
        RaindexError::NoLiquidity | RaindexError::InsufficientLiquidity { .. } => {
            tracing::warn!(error = %e, "no liquidity found");
            ApiError::NotFound("no liquidity found for this pair".into())
        }
        RaindexError::SameTokenPair
        | RaindexError::NonPositiveAmount
        | RaindexError::NegativePriceCap
        | RaindexError::FromHexError(_)
        | RaindexError::Float(_) => {
            tracing::warn!(error = %e, "invalid request parameters");
            ApiError::BadRequest(e.to_string())
        }
        _ => {
            tracing::error!(error = %e, "calldata generation failed");
            ApiError::Internal("failed to generate calldata".into())
        }
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
    use crate::types::swap::SwapCalldataResponse;
    use alloy::primitives::Address;
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::orders::RaindexOrder;
    use rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest;
    use rain_orderbook_common::take_orders::TakeOrderCandidate;

    pub struct MockSwapDataSource {
        pub orders: Result<Vec<RaindexOrder>, ApiError>,
        pub candidates: Vec<TakeOrderCandidate>,
        pub calldata_result: Result<SwapCalldataResponse, ApiError>,
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

        async fn get_calldata(
            &self,
            _request: TakeOrdersRequest,
        ) -> Result<SwapCalldataResponse, ApiError> {
            self.calldata_result.clone()
        }
    }
}
