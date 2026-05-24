use super::{RaindexSwapDataSource, SwapDataSource};
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::denomination::WrappedTokenIndex;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::routes::quote_denomination::{reverse_convert_calldata_ratio, CurrentRateLookup};
use crate::routes::tokens::SftSubgraphUrl;
use crate::types::swap::{SwapCalldataRequest, SwapCalldataResponse};
use crate::types::trades::Denomination;
use crate::wrapped_rates::SubgraphRateFetcher;
use rain_orderbook_common::raindex_client::take_orders::TakeOrdersRequest;
use rain_orderbook_common::take_orders::TakeOrdersMode;
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    post,
    path = "/v1/swap/calldata",
    tag = "Swap",
    security(("basicAuth" = [])),
    request_body = SwapCalldataRequest,
    params(
        ("denomination" = Option<Denomination>, Query, description =
            "Optional `denomination` override (fallback to body field; body wins if both set)."),
    ),
    responses(
        (status = 200, description = "Swap calldata", body = SwapCalldataResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "No liquidity found", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/calldata?<denomination>", data = "<request>")]
#[allow(clippy::too_many_arguments)]
pub async fn post_swap_calldata(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    pool: &State<DbPool>,
    sft_subgraph_url: &State<SftSubgraphUrl>,
    span: TracingSpan,
    denomination: Option<Denomination>,
    request: Json<SwapCalldataRequest>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
    let req = request.into_inner();
    let query_denomination = denomination;
    async move {
        tracing::info!(body = ?req, query_denomination = ?query_denomination, "request received");
        // Body wins if both provided; query is the fallback so SDK consumers
        // can override the default without rewriting the JSON body.
        let denomination = req.denomination.or(query_denomination).unwrap_or_default();

        // For tStock: reverse-convert the caller's `maximum_io_ratio` to
        // wtStock before submitting to the contract. The contract is always
        // wrapped-token-denominated. We compute the lookup outside the
        // `process_swap_calldata` helper so that helper stays trivially
        // testable without DB/subgraph dependencies.
        let (submitted_ratio, aps) = if denomination == Denomination::Unwrapped {
            let wrapped = WrappedTokenIndex::load(shared_raindex.inner())
                .await?
                .into_map();
            let fetcher = SubgraphRateFetcher::new(sft_subgraph_url.inner().0.clone());
            let mut lookup = CurrentRateLookup::new(pool.inner(), &fetcher, wrapped);
            reverse_convert_calldata_ratio(
                &req.maximum_io_ratio,
                req.input_token,
                req.output_token,
                denomination,
                &mut lookup,
            )
            .await?
        } else {
            (req.maximum_io_ratio.clone(), None)
        };

        let raindex = shared_raindex.read().await;
        let ds = RaindexSwapDataSource {
            client: raindex.client(),
        };
        let mut on_chain_req = req.clone();
        on_chain_req.maximum_io_ratio = submitted_ratio.clone();
        let mut response = process_swap_calldata(&ds, on_chain_req).await?;
        response.denomination = denomination;
        response.assets_per_share = aps;
        response.submitted_io_ratio = submitted_ratio;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn process_swap_calldata(
    ds: &dyn SwapDataSource,
    req: SwapCalldataRequest,
) -> Result<SwapCalldataResponse, ApiError> {
    let take_req = TakeOrdersRequest {
        taker: req.taker.to_string(),
        chain_id: crate::CHAIN_ID,
        sell_token: req.input_token.to_string(),
        buy_token: req.output_token.to_string(),
        mode: TakeOrdersMode::BuyUpTo,
        amount: req.output_amount,
        price_cap: req.maximum_io_ratio,
    };

    ds.get_calldata(take_req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::swap::test_fixtures::MockSwapDataSource;
    use crate::test_helpers::TestClientBuilder;
    use crate::types::common::Approval;
    use alloy::primitives::{address, Address, Bytes, U256};
    use rocket::http::{ContentType, Status};

    const USDC: Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    const WETH: Address = address!("4200000000000000000000000000000000000006");
    const TAKER: Address = address!("1111111111111111111111111111111111111111");
    const ORDERBOOK: Address = address!("d2938e7c9fe3597f78832ce780feb61945c377d7");

    fn calldata_request(output_amount: &str, max_ratio: &str) -> SwapCalldataRequest {
        SwapCalldataRequest {
            taker: TAKER,
            input_token: USDC,
            output_token: WETH,
            output_amount: output_amount.to_string(),
            maximum_io_ratio: max_ratio.to_string(),
            denomination: None,
        }
    }

    fn ready_response() -> SwapCalldataResponse {
        SwapCalldataResponse {
            to: ORDERBOOK,
            data: Bytes::from(vec![0xab, 0xcd, 0xef]),
            value: U256::ZERO,
            estimated_input: "150".to_string(),
            approvals: vec![],
            denomination: Denomination::Wrapped,
            assets_per_share: None,
            submitted_io_ratio: String::new(),
        }
    }

    fn approval_response() -> SwapCalldataResponse {
        SwapCalldataResponse {
            to: ORDERBOOK,
            data: Bytes::new(),
            value: U256::ZERO,
            estimated_input: "1000".to_string(),
            approvals: vec![Approval {
                token: USDC,
                spender: ORDERBOOK,
                amount: "1000".to_string(),
                symbol: String::new(),
                approval_data: Bytes::from(vec![0x09, 0x5e, 0xa7, 0xb3]),
            }],
            denomination: Denomination::Wrapped,
            assets_per_share: None,
            submitted_io_ratio: String::new(),
        }
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_ready() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(ready_response()),
        };
        let result = process_swap_calldata(&ds, calldata_request("100", "2.5"))
            .await
            .unwrap();

        assert_eq!(result.to, ORDERBOOK);
        assert!(!result.data.is_empty());
        assert_eq!(result.value, U256::ZERO);
        assert_eq!(result.estimated_input, "150");
        assert!(result.approvals.is_empty());
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_needs_approval() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(approval_response()),
        };
        let result = process_swap_calldata(&ds, calldata_request("100", "2.5"))
            .await
            .unwrap();

        assert_eq!(result.to, ORDERBOOK);
        assert!(result.data.is_empty());
        assert_eq!(result.approvals.len(), 1);
        assert_eq!(result.approvals[0].token, USDC);
        assert_eq!(result.approvals[0].spender, ORDERBOOK);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_not_found() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Err(ApiError::NotFound(
                "no liquidity found for this pair".into(),
            )),
        };
        let result = process_swap_calldata(&ds, calldata_request("100", "2.5")).await;
        assert!(matches!(result, Err(ApiError::NotFound(msg)) if msg.contains("no liquidity")));
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_bad_request() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Err(ApiError::BadRequest("invalid parameters".into())),
        };
        let result = process_swap_calldata(&ds, calldata_request("not-a-number", "2.5")).await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_internal_error() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("failed to generate calldata".into())),
        };
        let result = process_swap_calldata(&ds, calldata_request("100", "2.5")).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_swap_calldata_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/swap/calldata")
            .header(ContentType::JSON)
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100","maximumIoRatio":"2.5"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }
}
