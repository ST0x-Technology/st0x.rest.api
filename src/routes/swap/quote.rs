use super::{RaindexSwapDataSource, SwapDataSource, SwapQuoteCache};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::swap::{SwapQuoteRequest, SwapQuoteResponse};
use rain_math_float::Float;
use rain_orderbook_common::take_orders::simulate_buy_over_candidates;
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
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/quote", data = "<request>")]
pub async fn post_swap_quote(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    swap_cache: &State<SwapQuoteCache>,
    span: TracingSpan,
    request: Json<SwapQuoteRequest>,
) -> Result<Json<SwapQuoteResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let cache_key = (req.input_token, req.output_token, req.output_amount.clone());
        let req_for_fetch = req.clone();
        let response = swap_cache
            .get_or_try_insert(cache_key, || async move {
                let raindex = shared_raindex.read().await;
                let ds = RaindexSwapDataSource {
                    client: raindex.client(),
                };
                process_swap_quote(&ds, req_for_fetch).await
            })
            .await
            .map_err(ApiError::from)?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn process_swap_quote(
    ds: &dyn SwapDataSource,
    req: SwapQuoteRequest,
) -> Result<SwapQuoteResponse, ApiError> {
    let orders = ds
        .get_orders_for_pair(req.input_token, req.output_token)
        .await?;

    if orders.is_empty() {
        return Err(ApiError::NotFound(
            "no liquidity found for this pair".into(),
        ));
    }

    let candidates = ds
        .build_candidates_for_pair(&orders, req.input_token, req.output_token)
        .await?;

    if candidates.is_empty() {
        return Err(ApiError::NotFound("no valid quotes available".into()));
    }

    let buy_target = Float::parse(req.output_amount.clone()).map_err(|e| {
        tracing::error!(error = %e, "failed to parse output_amount");
        ApiError::BadRequest("invalid output_amount".into())
    })?;

    let price_cap = Float::max_positive_value().map_err(|e| {
        tracing::error!(error = %e, "failed to create price cap");
        ApiError::Internal("failed to create price cap".into())
    })?;

    let sim = simulate_buy_over_candidates(candidates, buy_target, price_cap).map_err(|e| {
        tracing::error!(error = %e, "failed to simulate swap");
        ApiError::Internal("failed to simulate swap".into())
    })?;

    if sim.legs.is_empty() {
        return Err(ApiError::NotFound("no valid quotes available".into()));
    }

    let blended_ratio = sim.total_input.div(sim.total_output).map_err(|e| {
        tracing::error!(error = %e, "failed to compute blended ratio");
        ApiError::Internal("failed to compute ratio".into())
    })?;

    let formatted_output = sim.total_output.format().map_err(|e| {
        tracing::error!(error = %e, "failed to format estimated output");
        ApiError::Internal("failed to format estimated output".into())
    })?;

    let formatted_input = sim.total_input.format().map_err(|e| {
        tracing::error!(error = %e, "failed to format estimated input");
        ApiError::Internal("failed to format estimated input".into())
    })?;

    let formatted_ratio = blended_ratio.format().map_err(|e| {
        tracing::error!(error = %e, "failed to format ratio");
        ApiError::Internal("failed to format ratio".into())
    })?;

    Ok(SwapQuoteResponse {
        input_token: req.input_token,
        output_token: req.output_token,
        output_amount: req.output_amount,
        estimated_output: formatted_output,
        estimated_input: formatted_input,
        estimated_io_ratio: formatted_ratio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::swap::test_fixtures::MockSwapDataSource;
    use crate::test_helpers::{mock_candidate, mock_order, TestClientBuilder};
    use alloy::primitives::address;
    use rocket::http::{ContentType, Status};

    const USDC: alloy::primitives::Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    const WETH: alloy::primitives::Address = address!("4200000000000000000000000000000000000006");

    fn quote_request(output_amount: &str) -> SwapQuoteRequest {
        SwapQuoteRequest {
            input_token: USDC,
            output_token: WETH,
            output_amount: output_amount.to_string(),
        }
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_success() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("1000", "1.5")],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await.unwrap();

        assert_eq!(result.input_token, USDC);
        assert_eq!(result.output_token, WETH);
        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_output, "100");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.estimated_io_ratio, "1.5");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_multi_leg() {
        let ds = MockSwapDataSource {
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
    async fn test_process_swap_quote_no_liquidity() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert!(matches!(result, Err(ApiError::NotFound(msg)) if msg.contains("no liquidity")));
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_no_candidates() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![mock_order()]),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert!(matches!(result, Err(ApiError::NotFound(msg)) if msg.contains("no valid quotes")));
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_invalid_output_amount() {
        let ds = MockSwapDataSource {
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
            orders: Err(ApiError::Internal("failed".into())),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("unused".into())),
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
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
    async fn test_swap_cache_returns_cached_response_without_fetch() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let cache = crate::routes::swap::swap_quote_cache();
        let key = (USDC, WETH, "100".to_string());
        let cached = SwapQuoteResponse {
            input_token: USDC,
            output_token: WETH,
            output_amount: "100".to_string(),
            estimated_output: "100".to_string(),
            estimated_input: "150".to_string(),
            estimated_io_ratio: "1.5".to_string(),
        };
        cache.insert(key.clone(), cached.clone()).await;

        // Fetch closure should not run on a cache hit; if it does the test
        // notices via the counter.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_inner = Arc::clone(&calls);
        let result: Result<SwapQuoteResponse, std::sync::Arc<ApiError>> = cache
            .get_or_try_insert(key, || async move {
                calls_inner.fetch_add(1, Ordering::SeqCst);
                Err::<SwapQuoteResponse, _>(ApiError::Internal("should not be called".into()))
            })
            .await;

        let response = result.expect("cache hit should bypass fetch");
        assert_eq!(response.estimated_io_ratio, "1.5");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[rocket::async_test]
    async fn test_swap_cache_runs_fetch_on_miss_and_populates() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let cache = crate::routes::swap::swap_quote_cache();
        let key = (USDC, WETH, "200".to_string());
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_inner = Arc::clone(&calls);

        let response: Result<SwapQuoteResponse, std::sync::Arc<ApiError>> = cache
            .get_or_try_insert(key.clone(), || async move {
                calls_inner.fetch_add(1, Ordering::SeqCst);
                Ok(SwapQuoteResponse {
                    input_token: USDC,
                    output_token: WETH,
                    output_amount: "200".to_string(),
                    estimated_output: "200".to_string(),
                    estimated_input: "300".to_string(),
                    estimated_io_ratio: "1.5".to_string(),
                })
            })
            .await;

        let r = response.expect("fetch result populated cache");
        assert_eq!(r.estimated_input, "300");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Subsequent get sees cached value without re-fetching.
        let cached = cache.get(&key).await.expect("cached value present");
        assert_eq!(cached.estimated_input, "300");
    }
}
