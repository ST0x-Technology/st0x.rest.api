use super::{RaindexSwapDataSource, SwapCalldataDataSource};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::swap::{SwapCalldataRequest, SwapCalldataResponse};
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
    responses(
        (status = 200, description = "Swap calldata", body = SwapCalldataResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "No liquidity found", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/calldata", data = "<request>")]
pub async fn post_swap_calldata(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    request: Json<SwapCalldataRequest>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let response = raindex
            .run_with_client(move |client| async move {
                let ds = RaindexSwapDataSource { client: &client };
                process_swap_calldata(&ds, req).await
            })
            .await
            .map_err(ApiError::from)??;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn process_swap_calldata(
    ds: &dyn SwapCalldataDataSource,
    req: SwapCalldataRequest,
) -> Result<SwapCalldataResponse, ApiError> {
    let take_req = TakeOrdersRequest {
        taker: req.taker.to_string(),
        chain_id: 8453,
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
    use crate::routes::swap::test_fixtures::MockSwapCalldataDataSource;
    use crate::test_helpers::{
        basic_auth_header, mock_invalid_raindex_config, seed_api_key, TestClientBuilder,
    };
    use crate::types::common::Approval;
    use alloy::primitives::{address, Address, Bytes, U256};
    use rocket::http::{ContentType, Header, Status};

    const USDC: Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    const WETH: Address = address!("4200000000000000000000000000000000000006");
    const ORDERBOOK: Address = address!("d2938e7c9fe3597f78832ce780feb61945c377d7");
    const TAKER: Address = address!("1111111111111111111111111111111111111111");

    fn calldata_request() -> SwapCalldataRequest {
        SwapCalldataRequest {
            taker: TAKER,
            input_token: USDC,
            output_token: WETH,
            output_amount: "100".to_string(),
            maximum_io_ratio: "0.0006".to_string(),
        }
    }

    fn ready_response() -> SwapCalldataResponse {
        SwapCalldataResponse {
            to: ORDERBOOK,
            data: Bytes::from(vec![0x01, 0x02, 0x03]),
            value: U256::ZERO,
            estimated_input: "150".to_string(),
            approvals: vec![],
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
                approval_data: Bytes::from(vec![0x09, 0x5e]),
            }],
        }
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_ready() {
        let ds = MockSwapCalldataDataSource {
            result: Ok(ready_response()),
        };
        let result = process_swap_calldata(&ds, calldata_request())
            .await
            .unwrap();

        assert_eq!(result.to, ORDERBOOK);
        assert_eq!(result.data, Bytes::from(vec![0x01, 0x02, 0x03]));
        assert_eq!(result.value, U256::ZERO);
        assert_eq!(result.estimated_input, "150");
        assert!(result.approvals.is_empty());
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_needs_approval() {
        let ds = MockSwapCalldataDataSource {
            result: Ok(approval_response()),
        };
        let result = process_swap_calldata(&ds, calldata_request())
            .await
            .unwrap();

        assert_eq!(result.approvals.len(), 1);
        assert_eq!(result.approvals[0].token, USDC);
        assert_eq!(result.approvals[0].spender, ORDERBOOK);
        assert_eq!(result.approvals[0].amount, "1000");
        assert!(result.data.is_empty());
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_not_found() {
        let ds = MockSwapCalldataDataSource {
            result: Err(ApiError::NotFound("no liquidity".into())),
        };
        let result = process_swap_calldata(&ds, calldata_request()).await;
        assert!(matches!(result, Err(ApiError::NotFound(_))));
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_bad_request() {
        let ds = MockSwapCalldataDataSource {
            result: Err(ApiError::BadRequest("same token pair".into())),
        };
        let result = process_swap_calldata(&ds, calldata_request()).await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_internal_error() {
        let ds = MockSwapCalldataDataSource {
            result: Err(ApiError::Internal("something broke".into())),
        };
        let result = process_swap_calldata(&ds, calldata_request()).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_swap_calldata_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/swap/calldata")
            .header(ContentType::JSON)
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100","maximumIoRatio":"0.0006"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_swap_calldata_500_when_client_init_fails() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/swap/calldata")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100","maximumIoRatio":"0.0006"}"#)
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
