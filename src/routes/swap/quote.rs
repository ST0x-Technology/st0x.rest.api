use super::{RaindexSwapDataSource, SwapDataSource};
use crate::analytics::{swap_quoted_event, swap_quoted_v2_event, Analytics};
use crate::app_state::ApplicationState;
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorCode, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::routes::swap::denomination::{
    denormalize_calldata_price_cap, normalize_calldata_price_cap,
    normalize_calldata_request_amount, normalize_quote_amounts, CalldataAmountNormalization,
};
use crate::types::swap::{
    SwapQuoteRequest, SwapQuoteResponse, SwapQuoteV2Request, SwapQuoteV2RequestBody,
    SwapQuoteV2Response,
};
use alloy::primitives::Address;
use rain_math_float::Float;
use rain_orderbook_common::take_orders::{
    simulate_buy_over_candidates, ParsedTakeOrdersMode, TakeOrdersMode,
};
use rocket::serde::json::Json;
use rocket::State;
use std::ops::Div;
use tracing::Instrument;

#[utoipa::path(
    post,
    path = "/v1/swap/quote",
    tag = "Swap",
    security(("basicAuth" = [])),
    request_body = SwapQuoteRequest,
    responses(
        (status = 200, description = "Swap quote", body = SwapQuoteResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "No liquidity found", body = ApiErrorResponse),
        (status = 422, description = "Request body could not be parsed", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
        (status = 502, description = "Order source unavailable", body = ApiErrorResponse),
    )
)]
#[post("/quote", data = "<request>")]
#[allow(clippy::too_many_arguments)] // Rocket handler: args are guards + managed state + body.
pub async fn post_swap_quote(
    _global: GlobalRateLimit,
    key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    analytics: &State<Analytics>,
    span: TracingSpan,
    request: Json<SwapQuoteRequest>,
) -> Result<Json<SwapQuoteResponse>, ApiError> {
    async move {
        let req = request.into_inner();
        tracing::info!(body = ?req, "request received");
        let raindex = shared_raindex.read().await;
        let ds = RaindexSwapDataSource {
            client: raindex.client(),
            caches: &app_state.response_caches,
            pool: pool.inner(),
        };
        let response = handle_swap_quote(&ds, &key, analytics.inner(), req).await?;

        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    post,
    path = "/v2/swap/quote",
    tag = "Swap",
    summary = "Simulate a mode-based swap",
    description = "Returns the SDK-backed executable-route simulation used by V2 calldata. Provide exactly one of priceCap or slippageBps. referenceIoRatio is an optional input-token-per-output-token guard that is valid only with slippageBps.",
    security(("basicAuth" = [])),
    request_body = SwapQuoteV2RequestBody,
    responses(
        (status = 200, description = "Mode-based swap quote", body = SwapQuoteV2Response),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "No liquidity found", body = ApiErrorResponse),
        (status = 422, description = "Request body could not be parsed", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/quote", data = "<request>")]
#[allow(clippy::too_many_arguments)] // Rocket handler: args are guards + managed state + body.
pub async fn post_swap_quote_v2(
    _global: GlobalRateLimit,
    key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    analytics: &State<Analytics>,
    span: TracingSpan,
    request: Json<SwapQuoteV2Request>,
) -> Result<Json<SwapQuoteV2Response>, ApiError> {
    async move {
        let req = request.into_inner();
        tracing::info!(
            mode = ?req.mode,
            denomination = ?req.denomination,
            has_taker = req.taker.is_some(),
            "request received"
        );
        let raindex = shared_raindex.read().await;
        let ds = RaindexSwapDataSource {
            client: raindex.client(),
            caches: &app_state.response_caches,
            pool: pool.inner(),
        };
        let response = handle_swap_quote_v2(&ds, &key, analytics.inner(), req).await?;

        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn handle_swap_quote(
    ds: &dyn SwapDataSource,
    key: &AuthenticatedKey,
    analytics: &Analytics,
    req: SwapQuoteRequest,
) -> Result<SwapQuoteResponse, ApiError> {
    let response = process_swap_quote(ds, req).await?;

    analytics.capture(|| {
        swap_quoted_event(
            key,
            response.input_token,
            response.output_token,
            serde_json::to_value(response.denomination).unwrap_or(serde_json::Value::Null),
            &response,
        )
    });

    Ok(response)
}

async fn handle_swap_quote_v2(
    ds: &dyn SwapDataSource,
    key: &AuthenticatedKey,
    analytics: &Analytics,
    req: SwapQuoteV2Request,
) -> Result<SwapQuoteV2Response, ApiError> {
    let response = process_swap_quote_v2(ds, req).await?;

    analytics.capture(|| swap_quoted_v2_event(key, &response));

    Ok(response)
}

async fn process_swap_quote(
    ds: &dyn SwapDataSource,
    req: SwapQuoteRequest,
) -> Result<SwapQuoteResponse, ApiError> {
    ds.validate_supported_tokens(req.input_token, req.output_token)
        .await
        .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::SwapQuoteFailed))?;

    let orders = ds
        .get_orders_for_pair(req.input_token, req.output_token)
        .await
        .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::OrdersQueryFailed))?;

    if orders.is_empty() {
        return Err(no_liquidity_error());
    }

    let candidates = ds
        .build_candidates_for_pair(&orders, req.input_token, req.output_token, Address::ZERO)
        .await
        .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::SwapQuoteFailed))?;

    if candidates.is_empty() {
        return Err(no_liquidity_error());
    }

    let buy_target = Float::parse(req.output_amount.clone()).map_err(|e| {
        tracing::error!(error = %e, "failed to parse output_amount");
        ApiError::BadRequest("invalid output_amount".into())
    })?;

    let price_cap = Float::max_positive_value().map_err(|e| {
        tracing::error!(error = %e, "failed to create price cap");
        quote_failed_error()
    })?;

    let sim = simulate_buy_over_candidates(candidates, buy_target, price_cap).map_err(|e| {
        tracing::error!(error = %e, "failed to simulate swap");
        quote_failed_error()
    })?;

    if sim.legs.is_empty() {
        return Err(no_liquidity_error());
    }

    let (estimated_input, estimated_output) = normalize_quote_amounts(
        ds,
        req.denomination,
        req.input_token,
        req.output_token,
        sim.total_input,
        sim.total_output,
    )
    .await
    .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::SwapQuoteFailed))?;

    let blended_ratio = estimated_input.div(estimated_output).map_err(|e| {
        tracing::error!(error = %e, "failed to compute blended ratio");
        quote_failed_error()
    })?;

    let formatted_output = estimated_output.format().map_err(|e| {
        tracing::error!(error = %e, "failed to format estimated output");
        quote_failed_error()
    })?;

    let formatted_input = estimated_input.format().map_err(|e| {
        tracing::error!(error = %e, "failed to format estimated input");
        quote_failed_error()
    })?;

    let formatted_ratio = blended_ratio.format().map_err(|e| {
        tracing::error!(error = %e, "failed to format ratio");
        quote_failed_error()
    })?;

    Ok(SwapQuoteResponse {
        input_token: req.input_token,
        output_token: req.output_token,
        output_amount: req.output_amount,
        denomination: req.denomination,
        estimated_output: formatted_output,
        estimated_input: formatted_input,
        estimated_io_ratio: formatted_ratio,
    })
}

async fn process_swap_quote_v2(
    ds: &dyn SwapDataSource,
    req: SwapQuoteV2Request,
) -> Result<SwapQuoteV2Response, ApiError> {
    ds.validate_supported_tokens(req.input_token, req.output_token)
        .await
        .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::SwapQuoteFailed))?;

    let mode: TakeOrdersMode = req.mode.into();
    let (amount, wrap_ratios) = normalize_calldata_request_amount(
        ds,
        CalldataAmountNormalization {
            denomination: req.denomination,
            input_token: req.input_token,
            output_token: req.output_token,
            mode,
            amount: req.amount.clone(),
            amount_field: "amount",
        },
    )
    .await
    .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::SwapQuoteFailed))?;
    let parsed_mode =
        ParsedTakeOrdersMode::parse(mode, &amount).map_err(super::map_raindex_error)?;

    let orders = ds
        .get_orders_for_pair(req.input_token, req.output_token)
        .await
        .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::OrdersQueryFailed))?;
    if orders.is_empty() {
        return Err(no_liquidity_error());
    }

    let candidates = ds
        .build_candidates_for_pair(
            &orders,
            req.input_token,
            req.output_token,
            req.taker.unwrap_or(Address::ZERO),
        )
        .await
        .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::SwapQuoteFailed))?;
    if candidates.is_empty() {
        return Err(no_liquidity_error());
    }

    let price_cap = match (
        req.price_cap.as_ref(),
        req.slippage_bps,
        req.reference_io_ratio.as_ref(),
    ) {
        (Some(price_cap), None, None) => {
            let normalized = normalize_calldata_price_cap(
                price_cap.clone(),
                "price_cap",
                req.denomination,
                req.input_token,
                req.output_token,
                &wrap_ratios,
            )?;
            Float::parse(normalized).map_err(|error| {
                tracing::warn!(%error, "swap quote rejected for invalid price_cap");
                ApiError::BadRequest("invalid price_cap".into())
            })?
        }
        (Some(_), None, Some(_)) => {
            tracing::warn!(
                "swap quote rejected because reference_io_ratio was provided without slippage_bps"
            );
            return Err(ApiError::BadRequest(
                "reference_io_ratio requires slippage_bps".into(),
            ));
        }
        (None, Some(slippage_bps @ 1..=5000), reference_io_ratio) => {
            let reference_io_ratio = reference_io_ratio
                .map(|reference_io_ratio| {
                    normalize_calldata_price_cap(
                        reference_io_ratio.clone(),
                        "reference_io_ratio",
                        req.denomination,
                        req.input_token,
                        req.output_token,
                        &wrap_ratios,
                    )
                    .and_then(|normalized| {
                        Float::parse(normalized).map_err(|error| {
                            tracing::warn!(
                                %error,
                                "swap quote rejected for invalid reference_io_ratio"
                            );
                            ApiError::BadRequest("invalid reference_io_ratio".into())
                        })
                    })
                })
                .transpose()?;
            super::slippage::resolve_slippage_price_cap(
                candidates.clone(),
                mode,
                &amount,
                slippage_bps,
                reference_io_ratio,
            )?
        }
        (None, Some(_), _) => {
            tracing::warn!("swap quote rejected for out-of-range slippage_bps");
            return Err(ApiError::BadRequest(
                "slippage_bps must be between 1 and 5000".into(),
            ));
        }
        _ => {
            tracing::warn!("swap quote rejected without exactly one price limit");
            return Err(ApiError::BadRequest(
                "provide exactly one of price_cap or slippage_bps".into(),
            ));
        }
    };

    let is_buy_mode = parsed_mode.is_buy_mode();
    let is_exact_mode = parsed_mode.is_exact_mode();
    let target_amount = parsed_mode.target_amount();
    let simulation =
        super::slippage::select_best_raindex_simulation(candidates, parsed_mode, price_cap)?;
    let achieved_amount = if is_buy_mode {
        simulation.total_output
    } else {
        simulation.total_input
    };
    let fully_filled = achieved_amount.eq(target_amount).map_err(|error| {
        tracing::error!(%error, "failed to compare swap quote fill amount");
        quote_failed_error()
    })?;
    if is_exact_mode && !fully_filled {
        let requested = target_amount.format().map_err(|error| {
            tracing::error!(%error, "failed to format requested quote amount");
            quote_failed_error()
        })?;
        let available = achieved_amount.format().map_err(|error| {
            tracing::error!(%error, "failed to format available quote amount");
            quote_failed_error()
        })?;
        tracing::warn!(%requested, %available, "insufficient executable liquidity");
        return Err(no_liquidity_error());
    }

    let (estimated_input, estimated_output) = normalize_quote_amounts(
        ds,
        req.denomination,
        req.input_token,
        req.output_token,
        simulation.total_input,
        simulation.total_output,
    )
    .await
    .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::SwapQuoteFailed))?;
    let estimated_io_ratio = estimated_input.div(estimated_output).map_err(|error| {
        tracing::error!(%error, "failed to compute v2 swap quote ratio");
        quote_failed_error()
    })?;
    let resolved_price_cap = denormalize_calldata_price_cap(
        price_cap,
        req.denomination,
        req.input_token,
        req.output_token,
        &wrap_ratios,
    )
    .map_err(|error| map_quote_boundary_error(error, ApiErrorCode::SwapQuoteFailed))?;

    let format_estimate = |value: Float, label: &str| {
        value.format().map_err(|error| {
            tracing::error!(%error, label, "failed to format v2 swap quote");
            quote_failed_error()
        })
    };

    Ok(SwapQuoteV2Response {
        input_token: req.input_token,
        output_token: req.output_token,
        mode: req.mode,
        amount: req.amount,
        denomination: req.denomination,
        estimated_input: format_estimate(estimated_input, "estimated input")?,
        estimated_output: format_estimate(estimated_output, "estimated output")?,
        estimated_io_ratio: format_estimate(estimated_io_ratio, "estimated IO ratio")?,
        fully_filled,
        resolved_price_cap,
    })
}

fn no_liquidity_error() -> ApiError {
    ApiError::coded(
        ApiErrorCode::SwapNoLiquidity,
        "no executable liquidity is available for this pair",
    )
}

fn quote_failed_error() -> ApiError {
    ApiError::coded(
        ApiErrorCode::SwapQuoteFailed,
        "the swap quote could not be generated",
    )
}

fn map_quote_boundary_error(error: ApiError, fallback_code: ApiErrorCode) -> ApiError {
    if matches!(error, ApiError::Coded { .. } | ApiError::BadRequest(_)) {
        return error;
    }
    tracing::error!(%error, code = %fallback_code, "swap quote boundary failed");
    match fallback_code {
        ApiErrorCode::OrdersQueryFailed => ApiError::coded(
            fallback_code,
            "the order source could not serve this request",
        ),
        _ => quote_failed_error(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{Analytics, RecordingSink};
    use crate::auth::AuthenticatedKey;
    use crate::routes::swap::test_fixtures::MockSwapDataSource;
    use crate::test_helpers::{mock_candidate, mock_order, TestClientBuilder};
    use crate::types::swap::{SwapCalldataMode, SwapDenomination};
    use crate::wrap_ratio::WrapRatioValue;
    use alloy::primitives::address;
    use async_trait::async_trait;
    use rocket::http::{ContentType, Status};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    const USDC: alloy::primitives::Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    const WETH: alloy::primitives::Address = address!("4200000000000000000000000000000000000006");

    fn test_key() -> AuthenticatedKey {
        AuthenticatedKey {
            id: 1,
            key_id: "test-client".to_string(),
            label: "Test client".to_string(),
            owner: "test-owner".to_string(),
            is_admin: false,
        }
    }

    fn quote_request(output_amount: &str) -> SwapQuoteRequest {
        SwapQuoteRequest {
            input_token: USDC,
            output_token: WETH,
            output_amount: output_amount.to_string(),
            denomination: SwapDenomination::Wrapped,
        }
    }

    fn quote_v2_request(mode: SwapCalldataMode, amount: &str) -> SwapQuoteV2Request {
        SwapQuoteV2Request {
            taker: None,
            input_token: USDC,
            output_token: WETH,
            mode,
            amount: amount.to_string(),
            price_cap: Some("2".to_string()),
            slippage_bps: None,
            reference_io_ratio: None,
            denomination: SwapDenomination::Wrapped,
        }
    }

    fn assert_error_code<T>(result: Result<T, ApiError>, expected: ApiErrorCode) {
        assert!(matches!(
            result,
            Err(ApiError::Coded { code, .. }) if code == expected
        ));
    }

    fn unwrapped_quote_request(
        input_token: alloy::primitives::Address,
        output_token: alloy::primitives::Address,
        output_amount: &str,
    ) -> SwapQuoteRequest {
        SwapQuoteRequest {
            input_token,
            output_token,
            output_amount: output_amount.to_string(),
            denomination: SwapDenomination::Unwrapped,
        }
    }

    fn wrap_ratio(
        share_address: alloy::primitives::Address,
        assets_per_share: &str,
    ) -> WrapRatioValue {
        WrapRatioValue {
            share_address,
            assets_per_share: assets_per_share.to_string(),
        }
    }

    struct MockQuoteDataSource {
        base: MockSwapDataSource,
        wrap_ratios: HashMap<alloy::primitives::Address, WrapRatioValue>,
    }

    struct RecordingCounterpartyDataSource {
        base: MockSwapDataSource,
        counterparties: Mutex<Vec<alloy::primitives::Address>>,
    }

    #[async_trait]
    impl SwapDataSource for RecordingCounterpartyDataSource {
        async fn validate_supported_tokens(
            &self,
            input_token: alloy::primitives::Address,
            output_token: alloy::primitives::Address,
        ) -> Result<(), ApiError> {
            self.base
                .validate_supported_tokens(input_token, output_token)
                .await
        }

        async fn get_orders_for_pair(
            &self,
            input_token: alloy::primitives::Address,
            output_token: alloy::primitives::Address,
        ) -> Result<Vec<rain_orderbook_common::raindex_client::orders::RaindexOrder>, ApiError>
        {
            self.base
                .get_orders_for_pair(input_token, output_token)
                .await
        }

        async fn build_candidates_for_pair(
            &self,
            orders: &[rain_orderbook_common::raindex_client::orders::RaindexOrder],
            input_token: alloy::primitives::Address,
            output_token: alloy::primitives::Address,
            counterparty: alloy::primitives::Address,
        ) -> Result<Vec<rain_orderbook_common::take_orders::TakeOrderCandidate>, ApiError> {
            self.counterparties
                .lock()
                .map_err(|_| ApiError::Internal("counterparty recorder poisoned".into()))?
                .push(counterparty);
            self.base
                .build_candidates_for_pair(orders, input_token, output_token, counterparty)
                .await
        }

        async fn get_calldata(
            &self,
            request: rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest,
        ) -> Result<crate::types::swap::SwapCalldataResponse, ApiError> {
            self.base.get_calldata(request).await
        }
    }

    #[async_trait]
    impl SwapDataSource for MockQuoteDataSource {
        async fn validate_supported_tokens(
            &self,
            input_token: alloy::primitives::Address,
            output_token: alloy::primitives::Address,
        ) -> Result<(), ApiError> {
            self.base
                .validate_supported_tokens(input_token, output_token)
                .await
        }

        async fn get_orders_for_pair(
            &self,
            input_token: alloy::primitives::Address,
            output_token: alloy::primitives::Address,
        ) -> Result<Vec<rain_orderbook_common::raindex_client::orders::RaindexOrder>, ApiError>
        {
            self.base
                .get_orders_for_pair(input_token, output_token)
                .await
        }

        async fn build_candidates_for_pair(
            &self,
            orders: &[rain_orderbook_common::raindex_client::orders::RaindexOrder],
            input_token: alloy::primitives::Address,
            output_token: alloy::primitives::Address,
            counterparty: alloy::primitives::Address,
        ) -> Result<Vec<rain_orderbook_common::take_orders::TakeOrderCandidate>, ApiError> {
            self.base
                .build_candidates_for_pair(orders, input_token, output_token, counterparty)
                .await
        }

        async fn get_calldata(
            &self,
            request: rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest,
        ) -> Result<crate::types::swap::SwapCalldataResponse, ApiError> {
            self.base.get_calldata(request).await
        }

        async fn get_wrap_ratios_for_tokens(
            &self,
            token_addresses: &[alloy::primitives::Address],
        ) -> Result<HashMap<alloy::primitives::Address, WrapRatioValue>, ApiError> {
            Ok(token_addresses
                .iter()
                .filter_map(|address| {
                    self.wrap_ratios
                        .get(address)
                        .map(|ratio| (*address, ratio.clone()))
                })
                .collect())
        }
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_success() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("1000", "1.5")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await.unwrap();

        assert_eq!(result.input_token, USDC);
        assert_eq!(result.output_token, WETH);
        assert_eq!(result.output_amount, "100");
        assert_eq!(result.denomination, SwapDenomination::Wrapped);
        assert_eq!(result.estimated_output, "100");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.estimated_io_ratio, "1.5");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_v2_buy_up_to() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("5", "1"), mock_candidate("5", "2")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };

        let result = process_swap_quote_v2(&ds, quote_v2_request(SwapCalldataMode::BuyUpTo, "8"))
            .await
            .unwrap();

        assert_eq!(result.estimated_input, "11");
        assert_eq!(result.estimated_output, "8");
        assert_eq!(result.estimated_io_ratio, "1.375");
        assert!(result.fully_filled);
        assert_eq!(result.resolved_price_cap, "2");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_v2_spend_up_to() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("5", "1"), mock_candidate("5", "2")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };

        let result = process_swap_quote_v2(&ds, quote_v2_request(SwapCalldataMode::SpendUpTo, "8"))
            .await
            .unwrap();

        assert_eq!(result.estimated_input, "8");
        assert_eq!(result.estimated_output, "6.5");
        assert_eq!(
            result.estimated_io_ratio,
            "1.2307692307692307692307692307692307692307692307692307692307692307692"
        );
        assert!(result.fully_filled);
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_v2_reports_partial_up_to_fill() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("3", "2")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };

        let result = process_swap_quote_v2(&ds, quote_v2_request(SwapCalldataMode::BuyUpTo, "5"))
            .await
            .unwrap();

        assert_eq!(result.estimated_input, "6");
        assert_eq!(result.estimated_output, "3");
        assert!(!result.fully_filled);
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_v2_rejects_partial_exact_fill() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("3", "2")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };

        let result =
            process_swap_quote_v2(&ds, quote_v2_request(SwapCalldataMode::SpendExact, "8")).await;

        assert_error_code(result, ApiErrorCode::SwapNoLiquidity);
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_v2_resolves_same_slippage_cap_as_calldata() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("5", "1"), mock_candidate("5", "2")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let mut request = quote_v2_request(SwapCalldataMode::BuyUpTo, "8");
        request.price_cap = None;
        request.slippage_bps = Some(100);

        let result = process_swap_quote_v2(&ds, request).await.unwrap();

        assert_eq!(result.resolved_price_cap, "2.02");
        assert_eq!(result.estimated_input, "11");
        assert_eq!(result.estimated_output, "8");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_v2_uses_taker_for_oracle_candidates() {
        let taker = address!("1234567890AbcdEF1234567890aBcdef12345678");
        let ds = RecordingCounterpartyDataSource {
            base: MockSwapDataSource {
                supported_tokens: Ok(()),
                orders: Ok(vec![mock_order()]),
                candidates: vec![mock_candidate("5", "1")],
                calldata_result: Err(ApiError::Internal("unused".into())),
            },
            counterparties: Mutex::new(Vec::new()),
        };
        let mut request = quote_v2_request(SwapCalldataMode::BuyUpTo, "5");
        request.taker = Some(taker);

        process_swap_quote_v2(&ds, request).await.unwrap();

        assert_eq!(
            *ds.counterparties.lock().unwrap(),
            vec![taker],
            "quote candidates must use the wallet context passed by the website"
        );
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_v2_validates_price_limit_options() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("5", "1")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };

        let mut both = quote_v2_request(SwapCalldataMode::BuyUpTo, "5");
        both.slippage_bps = Some(100);
        assert!(matches!(
            process_swap_quote_v2(&ds, both).await,
            Err(ApiError::BadRequest(message))
                if message.contains("exactly one")
        ));

        let mut neither = quote_v2_request(SwapCalldataMode::BuyUpTo, "5");
        neither.price_cap = None;
        assert!(matches!(
            process_swap_quote_v2(&ds, neither).await,
            Err(ApiError::BadRequest(message))
                if message.contains("exactly one")
        ));

        let mut out_of_range = quote_v2_request(SwapCalldataMode::BuyUpTo, "5");
        out_of_range.price_cap = None;
        out_of_range.slippage_bps = Some(5001);
        assert!(matches!(
            process_swap_quote_v2(&ds, out_of_range).await,
            Err(ApiError::BadRequest(message))
                if message.contains("between 1 and 5000")
        ));

        let mut reference_without_slippage = quote_v2_request(SwapCalldataMode::BuyUpTo, "5");
        reference_without_slippage.reference_io_ratio = Some("1".to_string());
        assert!(matches!(
            process_swap_quote_v2(&ds, reference_without_slippage).await,
            Err(ApiError::BadRequest(message))
                if message.contains("requires slippage_bps")
        ));
    }

    #[rocket::async_test]
    async fn test_handle_swap_quote_v2_captures_analytics() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("5", "1")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let recording = RecordingSink::new();
        let analytics = Analytics::new(Arc::new(recording.clone()));

        let response = handle_swap_quote_v2(
            &ds,
            &test_key(),
            &analytics,
            quote_v2_request(SwapCalldataMode::BuyUpTo, "5"),
        )
        .await
        .expect("successful quote");

        assert!(response.fully_filled);
        let event = recording
            .events()
            .into_iter()
            .find(|event| event.event == "swap_quoted")
            .expect("swap_quoted event");
        assert_eq!(event.distinct_id, "client:test-client");
        assert_eq!(event.properties["estimated_output"], "5");
        assert_eq!(event.properties["fully_filled"], true);
    }

    #[rocket::async_test]
    async fn test_handle_swap_quote_captures_analytics() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("1000", "1.5")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let recording = RecordingSink::new();
        let analytics = Analytics::new(Arc::new(recording.clone()));

        let response = handle_swap_quote(&ds, &test_key(), &analytics, quote_request("100"))
            .await
            .expect("successful quote");

        assert_eq!(response.estimated_input, "150");
        let event = recording
            .events()
            .into_iter()
            .find(|event| event.event == "swap_quoted")
            .expect("swap_quoted event");
        assert_eq!(event.distinct_id, "client:test-client");
        assert_eq!(event.properties["estimated_input"], "150");
        assert_eq!(event.properties["api_client_owner"], "test-owner");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_multi_leg() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("50", "2"), mock_candidate("50", "3")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await.unwrap();

        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_output, "100");
        assert_eq!(result.estimated_input, "250");
        assert_eq!(result.estimated_io_ratio, "2.5");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_partial_fill() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("30", "2")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await.unwrap();

        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_output, "30");
        assert_eq!(result.estimated_input, "60");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_picks_best_ratio() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![
                mock_candidate("1000", "3"),
                mock_candidate("1000", "1.5"),
                mock_candidate("1000", "2"),
            ],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("10")).await.unwrap();

        assert_eq!(result.estimated_io_ratio, "1.5");
        assert_eq!(result.estimated_input, "15");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_unwrapped_converts_input_amount_and_ratio() {
        let wt_mstr = address!("Ff05e1BD696900DC6A52cA35cA61bB1024eDA8e2");
        let ds = MockQuoteDataSource {
            base: MockSwapDataSource {
                supported_tokens: Ok(()),
                orders: Ok(vec![mock_order()]),
                candidates: vec![mock_candidate("1000", "1.5")],
                calldata_result: Err(ApiError::Internal("unused".into())),
            },
            wrap_ratios: HashMap::from([(wt_mstr, wrap_ratio(wt_mstr, "2"))]),
        };

        let result = process_swap_quote(&ds, unwrapped_quote_request(wt_mstr, WETH, "100"))
            .await
            .unwrap();

        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_output, "100");
        assert_eq!(result.estimated_input, "300");
        assert_eq!(result.estimated_io_ratio, "3");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_unwrapped_converts_output_amount_and_ratio() {
        let wt_mstr = address!("Ff05e1BD696900DC6A52cA35cA61bB1024eDA8e2");
        let ds = MockQuoteDataSource {
            base: MockSwapDataSource {
                supported_tokens: Ok(()),
                orders: Ok(vec![mock_order()]),
                candidates: vec![mock_candidate("1000", "1.5")],
                calldata_result: Err(ApiError::Internal("unused".into())),
            },
            wrap_ratios: HashMap::from([(wt_mstr, wrap_ratio(wt_mstr, "2"))]),
        };

        let result = process_swap_quote(&ds, unwrapped_quote_request(USDC, wt_mstr, "100"))
            .await
            .unwrap();

        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_output, "200");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.estimated_io_ratio, "0.75");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_unwrapped_converts_both_sides() {
        let wt_mstr = address!("Ff05e1BD696900DC6A52cA35cA61bB1024eDA8e2");
        let wt_coin = address!("EeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE");
        let ds = MockQuoteDataSource {
            base: MockSwapDataSource {
                supported_tokens: Ok(()),
                orders: Ok(vec![mock_order()]),
                candidates: vec![mock_candidate("1000", "1.5")],
                calldata_result: Err(ApiError::Internal("unused".into())),
            },
            wrap_ratios: HashMap::from([
                (wt_mstr, wrap_ratio(wt_mstr, "2")),
                (wt_coin, wrap_ratio(wt_coin, "3")),
            ]),
        };

        let result = process_swap_quote(&ds, unwrapped_quote_request(wt_mstr, wt_coin, "100"))
            .await
            .unwrap();

        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_output, "300");
        assert_eq!(result.estimated_input, "300");
        assert_eq!(result.estimated_io_ratio, "1");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_unwrapped_noop_for_non_wrapped_tokens() {
        let ds = MockQuoteDataSource {
            base: MockSwapDataSource {
                supported_tokens: Ok(()),
                orders: Ok(vec![mock_order()]),
                candidates: vec![mock_candidate("1000", "1.5")],
                calldata_result: Err(ApiError::Internal("unused".into())),
            },
            wrap_ratios: HashMap::new(),
        };

        let result = process_swap_quote(&ds, unwrapped_quote_request(USDC, WETH, "100"))
            .await
            .unwrap();

        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_output, "100");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.estimated_io_ratio, "1.5");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_no_liquidity() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert_error_code(result, ApiErrorCode::SwapNoLiquidity);
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_no_candidates() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert_error_code(result, ApiErrorCode::SwapNoLiquidity);
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_invalid_output_amount() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("1000", "1.5")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("not-a-number")).await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_query_failure() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Err(ApiError::Internal("failed".into())),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert_error_code(result, ApiErrorCode::OrdersQueryFailed);
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_rejects_unsupported_tokens() {
        let ds = MockSwapDataSource {
            supported_tokens: Err(ApiError::coded(
                ApiErrorCode::SwapUnsupportedToken,
                "one or both swap tokens are unsupported",
            )),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("1000", "1.5")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert_error_code(result, ApiErrorCode::SwapUnsupportedToken);
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_registry_failure_is_not_retryable() {
        let ds = MockSwapDataSource {
            supported_tokens: Err(ApiError::Internal("invalid local registry".into())),
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("1000", "1.5")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert_error_code(result, ApiErrorCode::SwapQuoteFailed);
    }

    #[rocket::async_test]
    async fn test_swap_quote_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/swap/quote")
            .header(ContentType::JSON)
            .body(r#"{"inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_swap_quote_400_for_unsupported_tokens() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = crate::test_helpers::seed_api_key(&client).await;
        let header = crate::test_helpers::basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/swap/quote")
            .header(ContentType::JSON)
            .header(rocket::http::Header::new("Authorization", header))
            .body(r#"{"inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
        let body = response.into_json::<ApiErrorResponse>().await.unwrap();
        assert_eq!(body.error.code, ApiErrorCode::SwapUnsupportedToken);
        assert!(!body.request_id.is_empty());
    }

    #[rocket::async_test]
    async fn test_swap_quote_422_for_invalid_denomination() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = crate::test_helpers::seed_api_key(&client).await;
        let header = crate::test_helpers::basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/swap/quote")
            .header(ContentType::JSON)
            .header(rocket::http::Header::new("Authorization", header))
            .body(r#"{"inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100","denomination":"invalid"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }
}
