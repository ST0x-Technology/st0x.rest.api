mod calldata;
mod denomination;
mod quote;
mod slippage;

use crate::analytics::{Analytics, AnalyticsEvent, ApiVersion, SwapFailure};
use crate::cache::RouteResponseCaches;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorCode};
use crate::types::swap::{SwapCalldataMode, SwapCalldataResponse, SwapDenomination};
use crate::wrap_ratio::{
    persist_wrap_ratio_snapshots_best_effort, read_wrap_ratio_responses_for_addresses,
    wrap_ratio_values_from_responses, WrapRatioValue,
};
use alloy::primitives::Address;
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::orders::{
    GetOrdersFilters, GetOrdersTokenFilter, RaindexOrder,
};
use rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest;
use rain_orderbook_common::raindex_client::types::ChainIds;
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_common::raindex_client::RaindexError;
use rain_orderbook_common::rpc_client::RpcClientError;
use rain_orderbook_common::take_orders::{
    build_take_order_candidates_for_pair, NoopInjector, TakeOrderCandidate, TakeOrdersMode,
};
use rocket::Route;
use std::collections::HashMap;
use std::future::Future;

struct SwapAnalyticsContext {
    input_token: Address,
    output_token: Address,
    requested_amount: String,
    denomination: serde_json::Value,
    api_version: ApiVersion,
    mode: Option<serde_json::Value>,
    taker: Option<Address>,
}

impl SwapAnalyticsContext {
    fn failure(&self) -> SwapFailure<'_> {
        SwapFailure {
            input_token: self.input_token,
            output_token: self.output_token,
            requested_amount: &self.requested_amount,
            denomination: self.denomination.clone(),
            api_version: self.api_version,
            mode: self.mode.clone(),
            taker: self.taker,
        }
    }
}

fn snapshot_swap_context(
    analytics: &Analytics,
    build: impl FnOnce() -> SwapAnalyticsContext,
) -> Option<SwapAnalyticsContext> {
    analytics.is_enabled().then(build)
}

async fn capture_swap_outcome<T>(
    analytics: &Analytics,
    context: Option<SwapAnalyticsContext>,
    operation: impl Future<Output = Result<T, ApiError>>,
    build_failure: impl FnOnce(&SwapAnalyticsContext, &ApiError) -> AnalyticsEvent,
    build_success: impl FnOnce(&SwapAnalyticsContext, &T) -> AnalyticsEvent,
) -> Result<T, ApiError> {
    match operation.await {
        Ok(response) => {
            if let Some(context) = context.as_ref() {
                analytics.capture(|| build_success(context, &response));
            }
            Ok(response)
        }
        Err(error) => {
            if let Some(context) = context.as_ref() {
                analytics.capture(|| build_failure(context, &error));
            }
            Err(error)
        }
    }
}

#[async_trait]
pub(crate) trait SwapDataSource: Send + Sync {
    async fn validate_supported_tokens(
        &self,
        input_token: Address,
        output_token: Address,
    ) -> Result<(), ApiError>;

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
        counterparty: Address,
    ) -> Result<Vec<TakeOrderCandidate>, ApiError>;

    async fn get_calldata(
        &self,
        request: TakeOrdersRequest,
    ) -> Result<SwapCalldataResponse, ApiError>;

    async fn get_wrap_ratios_for_tokens(
        &self,
        _token_addresses: &[Address],
    ) -> Result<HashMap<Address, WrapRatioValue>, ApiError> {
        Ok(HashMap::new())
    }
}

pub(crate) struct RaindexSwapDataSource<'a> {
    pub client: &'a RaindexClient,
    pub caches: &'a RouteResponseCaches,
    pub pool: &'a DbPool,
}

fn swap_chain_ids() -> ChainIds {
    ChainIds(vec![crate::CHAIN_ID])
}

fn swap_candidates_cache_key(
    orders: &[RaindexOrder],
    input_token: Address,
    output_token: Address,
) -> String {
    let mut order_keys = orders
        .iter()
        .map(|order| {
            format!(
                "{}:{}:{}",
                order.chain_id(),
                order.raindex(),
                order.order_hash()
            )
        })
        .collect::<Vec<_>>();
    order_keys.sort_unstable();
    let order_keys = order_keys.join(",");
    format!("swap-candidates/latest/default/{input_token}/{output_token}/{order_keys}")
}

#[async_trait]
impl<'a> SwapDataSource for RaindexSwapDataSource<'a> {
    async fn validate_supported_tokens(
        &self,
        input_token: Address,
        output_token: Address,
    ) -> Result<(), ApiError> {
        let tokens = self.client.get_all_tokens().map_err(|e| {
            tracing::error!(error = %e, "failed to retrieve curated tokens");
            ApiError::Internal("failed to read the token registry".into())
        })?;

        let input_supported = tokens.values().any(|token| token.address == input_token);
        let output_supported = tokens.values().any(|token| token.address == output_token);

        if input_supported && output_supported {
            tracing::info!(input_token = %input_token, output_token = %output_token, "validated supported swap tokens");
            return Ok(());
        }

        tracing::warn!(
            input_token = %input_token,
            output_token = %output_token,
            input_supported,
            output_supported,
            "swap request rejected for unsupported curated tokens"
        );
        Err(ApiError::coded(
            ApiErrorCode::SwapUnsupportedToken,
            "one or both swap tokens are unsupported",
        ))
    }

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
            has_positive_output_vault_balance: Some(true),
            ..Default::default()
        };
        self.client
            .get_orders(Some(swap_chain_ids()), Some(filters), None, None)
            .await
            .map(|r| r.orders().to_vec())
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query orders for pair");
                ApiError::coded(
                    ApiErrorCode::OrdersQueryFailed,
                    "the order source could not serve this request",
                )
            })
    }

    async fn build_candidates_for_pair(
        &self,
        orders: &[RaindexOrder],
        input_token: Address,
        output_token: Address,
        counterparty: Address,
    ) -> Result<Vec<TakeOrderCandidate>, ApiError> {
        let fetch = || async {
            build_take_order_candidates_for_pair(
                orders,
                input_token,
                output_token,
                None,
                None,
                counterparty,
                &NoopInjector,
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to build order candidates");
                ApiError::coded(
                    ApiErrorCode::SwapQuoteFailed,
                    "the swap quote could not be generated",
                )
            })
        };

        if !self.caches.is_enabled() || counterparty != Address::ZERO {
            return fetch().await;
        }

        self.caches
            .swap_candidates
            .get_or_try_insert(
                swap_candidates_cache_key(orders, input_token, output_token),
                fetch,
            )
            .await
            .map_err(|e| (*e).clone())
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
                denomination: SwapDenomination::Wrapped,
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
                ApiError::coded(
                    ApiErrorCode::SwapCalldataFailed,
                    "swap calldata could not be generated",
                )
            })?;
            Ok(SwapCalldataResponse {
                to: take_orders_info.raindex(),
                data: take_orders_info.calldata().clone(),
                value: alloy::primitives::U256::ZERO,
                estimated_input: expected_sell,
                denomination: SwapDenomination::Wrapped,
                approvals: vec![],
            })
        } else {
            tracing::error!("calldata provider returned an unexpected result state");
            Err(ApiError::coded(
                ApiErrorCode::SwapCalldataFailed,
                "swap calldata could not be generated",
            ))
        }
    }

    async fn get_wrap_ratios_for_tokens(
        &self,
        token_addresses: &[Address],
    ) -> Result<HashMap<Address, WrapRatioValue>, ApiError> {
        let tokens: Vec<_> = self
            .client
            .get_all_tokens()
            .map_err(|e| {
                tracing::error!(error = %e, "failed to retrieve curated tokens");
                ApiError::Internal("failed to read the token registry".into())
            })?
            .into_values()
            .collect();

        let responses = read_wrap_ratio_responses_for_addresses(&tokens, token_addresses).await?;
        persist_wrap_ratio_snapshots_best_effort(self.pool, &responses).await;
        Ok(wrap_ratio_values_from_responses(responses))
    }
}

/// Reject a swap whose input and output token are the same, before any order lookup.
///
/// Such a request is always a caller bug, but it is not a *cheap* one: the token is
/// individually supported, so `validate_supported_tokens` passes, and the pair matches
/// every order holding that token. For a stablecoin leg that is the entire book, which
/// then gets quoted over RPC before the simulation finds no executable leg and returns
/// "no liquidity" — a ~15s round trip to deliver a misleading answer to a malformed
/// question. Failing fast makes the error self-explanatory and stops one request from
/// costing a full-book RPC sweep.
pub(crate) fn ensure_distinct_tokens(
    input_token: Address,
    output_token: Address,
) -> Result<(), ApiError> {
    if input_token != output_token {
        return Ok(());
    }
    tracing::warn!(
        token = %input_token,
        "swap request rejected: input and output token are identical"
    );
    Err(same_token_error())
}

fn same_token_error() -> ApiError {
    ApiError::coded(
        ApiErrorCode::SwapSameToken,
        "inputToken and outputToken must be different tokens",
    )
}

fn map_raindex_error(e: RaindexError) -> ApiError {
    match &e {
        RaindexError::NoLiquidity | RaindexError::InsufficientLiquidity { .. } => {
            tracing::warn!(error = %e, "no liquidity found");
            ApiError::coded(
                ApiErrorCode::SwapNoLiquidity,
                "no executable liquidity is available for this pair",
            )
        }
        // Kept distinct from the generic bucket below: `ensure_distinct_tokens` should
        // catch this at the edge, so reaching it here means a same-token pair slipped
        // past the guard and the caller still deserves to be told which mistake it was.
        RaindexError::SameTokenPair => {
            tracing::warn!(error = %e, "same-token pair reached the raindex layer");
            same_token_error()
        }
        RaindexError::NonPositiveAmount
        | RaindexError::NegativePriceCap
        | RaindexError::FromHexError(_)
        | RaindexError::Float(_) => {
            tracing::warn!(error = %e, "invalid request parameters");
            ApiError::BadRequest("invalid swap parameters".into())
        }
        RaindexError::RaindexSubgraphClientError(_) => {
            tracing::error!(error = %e, "order source failed during calldata generation");
            ApiError::coded(
                ApiErrorCode::OrdersQueryFailed,
                "the order source could not serve this request",
            )
        }
        RaindexError::RpcClientError(
            RpcClientError::Transport(_)
            | RpcClientError::RpcError { .. }
            | RpcClientError::RateLimited { .. },
        ) => {
            tracing::error!(error = %e, "RPC unavailable during calldata generation");
            ApiError::coded(
                ApiErrorCode::UpstreamUnavailable,
                "a required chain data provider is temporarily unavailable",
            )
        }
        RaindexError::PreflightError(_) => {
            tracing::error!(error = %e, "preflight failed without a typed failure category");
            ApiError::coded(
                ApiErrorCode::SwapCalldataFailed,
                "swap calldata could not be generated",
            )
        }
        _ => {
            tracing::error!(error = %e, "calldata generation failed");
            ApiError::coded(
                ApiErrorCode::SwapCalldataFailed,
                "swap calldata could not be generated",
            )
        }
    }
}

impl From<SwapCalldataMode> for TakeOrdersMode {
    fn from(mode: SwapCalldataMode) -> Self {
        match mode {
            SwapCalldataMode::BuyUpTo => TakeOrdersMode::BuyUpTo,
            SwapCalldataMode::SpendExact => TakeOrdersMode::SpendExact,
            SwapCalldataMode::SpendUpTo => TakeOrdersMode::SpendUpTo,
        }
    }
}

pub use calldata::*;
pub use quote::*;

pub fn routes() -> Vec<Route> {
    rocket::routes![quote::post_swap_quote, calldata::post_swap_calldata]
}

pub fn routes_v2() -> Vec<Route> {
    rocket::routes![quote::post_swap_quote_v2, calldata::post_swap_calldata_v2]
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_distinct_tokens, map_raindex_error, snapshot_swap_context,
        swap_candidates_cache_key, swap_chain_ids,
    };
    use crate::analytics::Analytics;
    use crate::error::{ApiError, ApiErrorCode};
    use alloy::primitives::address;
    use rain_orderbook_common::raindex_client::orders::RaindexOrder;
    use rain_orderbook_common::raindex_client::RaindexError;
    use rain_orderbook_common::rpc_client::RpcClientError;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn mock_order(chain_id: u32, order_hash: &str) -> RaindexOrder {
        let mut value = crate::test_helpers::order_json();
        value["chainId"] = json!(chain_id);
        value["orderHash"] = json!(order_hash);
        serde_json::from_value(value).expect("deserialize mock order")
    }

    #[test]
    fn test_ensure_distinct_tokens_accepts_distinct_and_rejects_equal_tokens() {
        let usdc = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        let weth = address!("4200000000000000000000000000000000000006");
        assert!(ensure_distinct_tokens(usdc, weth).is_ok());
        assert!(matches!(
            ensure_distinct_tokens(usdc, usdc),
            Err(ApiError::Coded { code, .. }) if code == ApiErrorCode::SwapSameToken
        ));
    }

    #[test]
    fn test_disabled_analytics_skips_swap_context_snapshot() {
        let built = AtomicBool::new(false);

        let context = snapshot_swap_context(&Analytics::disabled(), || {
            built.store(true, Ordering::Relaxed);
            unreachable!("disabled analytics must not build a swap context")
        });

        assert!(context.is_none());
        assert!(!built.load(Ordering::Relaxed));
    }

    /// A same-token pair that somehow reaches the raindex layer must still surface as
    /// `SWAP_SAME_TOKEN`, not be flattened into the generic invalid-parameters bucket.
    #[test]
    fn test_same_token_pair_from_raindex_keeps_its_code() {
        assert!(matches!(
            map_raindex_error(RaindexError::SameTokenPair),
            ApiError::Coded { code, .. } if code == ApiErrorCode::SwapSameToken
        ));
    }

    #[test]
    fn test_swap_candidates_cache_key_is_order_insensitive() {
        let input_token = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        let output_token = address!("4200000000000000000000000000000000000006");
        let order_a = mock_order(
            8453,
            "0x0000000000000000000000000000000000000000000000000000000000000001",
        );
        let order_b = mock_order(
            8453,
            "0x0000000000000000000000000000000000000000000000000000000000000002",
        );

        assert_eq!(
            swap_candidates_cache_key(
                &[order_a.clone(), order_b.clone()],
                input_token,
                output_token,
            ),
            swap_candidates_cache_key(&[order_b, order_a], input_token, output_token)
        );
    }

    #[test]
    fn test_swap_orders_are_scoped_to_calldata_chain() {
        assert_eq!(swap_chain_ids().0, vec![crate::CHAIN_ID]);
    }

    #[test]
    fn test_raindex_no_liquidity_maps_to_stable_code() {
        assert!(matches!(
            map_raindex_error(RaindexError::NoLiquidity),
            ApiError::Coded {
                code: ApiErrorCode::SwapNoLiquidity,
                ..
            }
        ));
    }

    #[test]
    fn test_ambiguous_raindex_preflight_maps_to_server_failure() {
        let error = map_raindex_error(RaindexError::PreflightError(
            "sensitive rpc detail".to_string(),
        ));
        assert!(matches!(
            error,
            ApiError::Coded {
                code: ApiErrorCode::SwapCalldataFailed,
                public_message: "swap calldata could not be generated",
            }
        ));
    }

    #[test]
    fn test_typed_rpc_failure_maps_to_upstream_unavailable() {
        let error = map_raindex_error(RaindexError::RpcClientError(RpcClientError::RpcError {
            message: "sensitive rpc detail".to_string(),
        }));
        assert!(matches!(
            error,
            ApiError::Coded {
                code: ApiErrorCode::UpstreamUnavailable,
                public_message: "a required chain data provider is temporarily unavailable",
            }
        ));
    }

    #[test]
    fn test_rpc_configuration_failure_is_not_retryable() {
        let error = map_raindex_error(RaindexError::RpcClientError(RpcClientError::Config {
            message: "bad local config".to_string(),
        }));
        assert!(matches!(
            error,
            ApiError::Coded {
                code: ApiErrorCode::SwapCalldataFailed,
                ..
            }
        ));
    }
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
        pub supported_tokens: Result<(), ApiError>,
        pub orders: Result<Vec<RaindexOrder>, ApiError>,
        pub candidates: Vec<TakeOrderCandidate>,
        pub calldata_result: Result<SwapCalldataResponse, ApiError>,
    }

    #[async_trait]
    impl SwapDataSource for MockSwapDataSource {
        async fn validate_supported_tokens(
            &self,
            _input_token: Address,
            _output_token: Address,
        ) -> Result<(), ApiError> {
            self.supported_tokens.clone()
        }

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
            _counterparty: Address,
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
