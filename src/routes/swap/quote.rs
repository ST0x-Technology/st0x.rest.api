use super::{RaindexSwapDataSource, SwapDataSource};
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
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    request: Json<SwapQuoteRequest>,
) -> Result<Json<SwapQuoteResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let response = raindex
            .run_with_client(move |client| async move {
                let ds = RaindexSwapDataSource { client: &client };
                process_swap_quote(&ds, req).await
            })
            .await
            .map_err(ApiError::from)??;
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
        estimated_input: formatted_input,
        estimated_io_ratio: formatted_ratio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::mock_order;
    use crate::routes::swap::test_fixtures::{mock_candidate, MockSwapDataSource};
    use crate::test_helpers::{
        basic_auth_header, mock_invalid_raindex_config, seed_api_key, TestClientBuilder,
    };
    use alloy::primitives::address;
    use rocket::http::{ContentType, Header, Status};

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
        };
        let result = process_swap_quote(&ds, quote_request("100")).await.unwrap();

        assert_eq!(result.input_token, USDC);
        assert_eq!(result.output_token, WETH);
        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.estimated_io_ratio, "1.5");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_multi_leg() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("50", "2"), mock_candidate("50", "3")],
        };
        let result = process_swap_quote(&ds, quote_request("100")).await.unwrap();

        assert_eq!(result.output_amount, "100");
        assert_eq!(result.estimated_input, "250");
        assert_eq!(result.estimated_io_ratio, "2.5");
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_partial_fill() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("30", "2")],
        };
        let result = process_swap_quote(&ds, quote_request("100")).await.unwrap();

        assert_eq!(result.estimated_input, "60");
        assert_eq!(result.output_amount, "100");
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
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert!(matches!(result, Err(ApiError::NotFound(msg)) if msg.contains("no liquidity")));
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_no_candidates() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![mock_order()]),
            candidates: vec![],
        };
        let result = process_swap_quote(&ds, quote_request("100")).await;
        assert!(matches!(result, Err(ApiError::NotFound(msg)) if msg.contains("no valid quotes")));
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_invalid_output_amount() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![mock_order()]),
            candidates: vec![mock_candidate("1000", "1.5")],
        };
        let result = process_swap_quote(&ds, quote_request("not-a-number")).await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[rocket::async_test]
    async fn test_process_swap_quote_query_failure() {
        let ds = MockSwapDataSource {
            orders: Err(ApiError::Internal("failed".into())),
            candidates: vec![],
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
    async fn test_swap_quote_500_when_client_init_fails() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/swap/quote")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(r#"{"inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert_eq!(
            body["error"]["message"],
            "failed to initialize orderbook client"
        );
    }
}
