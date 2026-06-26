use super::{RaindexSwapDataSource, SwapDataSource};
use crate::app_state::ApplicationState;
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::routes::swap::denomination::{
    normalize_calldata_request_values, normalize_calldata_response, CalldataRequestNormalization,
};
use crate::types::swap::{
    SwapCalldataMode, SwapCalldataRequest, SwapCalldataResponse, SwapCalldataV2Request,
};
use alloy::primitives::Address;
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
        (status = 422, description = "Request body could not be parsed", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/calldata", data = "<request>")]
pub async fn post_swap_calldata(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    span: TracingSpan,
    request: Json<SwapCalldataRequest>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let raindex = shared_raindex.read().await;
        let ds = RaindexSwapDataSource {
            client: raindex.client(),
            caches: &app_state.response_caches,
            pool: pool.inner(),
        };
        let response = process_swap_calldata(&ds, req).await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    post,
    path = "/v2/swap/calldata",
    tag = "Swap",
    security(("basicAuth" = [])),
    request_body = SwapCalldataV2Request,
    responses(
        (status = 200, description = "Swap calldata", body = SwapCalldataResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "No liquidity found", body = ApiErrorResponse),
        (status = 422, description = "Request body could not be parsed", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/calldata", data = "<request>")]
pub async fn post_swap_calldata_v2(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    span: TracingSpan,
    request: Json<SwapCalldataV2Request>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let raindex = shared_raindex.read().await;
        let ds = RaindexSwapDataSource {
            client: raindex.client(),
            caches: &app_state.response_caches,
            pool: pool.inner(),
        };
        let response = process_swap_calldata_v2(&ds, req).await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[derive(Debug)]
struct SwapCalldataBuildRequest {
    taker: Address,
    input_token: Address,
    output_token: Address,
    mode: TakeOrdersMode,
    amount: String,
    amount_field: &'static str,
    price_cap: String,
    price_cap_field: &'static str,
    denomination: crate::types::swap::SwapDenomination,
}

impl From<SwapCalldataRequest> for SwapCalldataBuildRequest {
    fn from(req: SwapCalldataRequest) -> Self {
        Self {
            taker: req.taker,
            input_token: req.input_token,
            output_token: req.output_token,
            mode: TakeOrdersMode::BuyUpTo,
            amount: req.output_amount,
            amount_field: "output_amount",
            price_cap: req.maximum_io_ratio,
            price_cap_field: "maximum_io_ratio",
            denomination: req.denomination,
        }
    }
}

impl From<SwapCalldataV2Request> for SwapCalldataBuildRequest {
    fn from(req: SwapCalldataV2Request) -> Self {
        Self {
            taker: req.taker,
            input_token: req.input_token,
            output_token: req.output_token,
            mode: req.mode.into(),
            amount: req.amount,
            amount_field: "amount",
            price_cap: req.price_cap,
            price_cap_field: "price_cap",
            denomination: req.denomination,
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

async fn process_swap_calldata(
    ds: &dyn SwapDataSource,
    req: SwapCalldataRequest,
) -> Result<SwapCalldataResponse, ApiError> {
    process_swap_calldata_build(ds, req.into()).await
}

async fn process_swap_calldata_v2(
    ds: &dyn SwapDataSource,
    req: SwapCalldataV2Request,
) -> Result<SwapCalldataResponse, ApiError> {
    process_swap_calldata_build(ds, req.into()).await
}

async fn process_swap_calldata_build(
    ds: &dyn SwapDataSource,
    req: SwapCalldataBuildRequest,
) -> Result<SwapCalldataResponse, ApiError> {
    ds.validate_supported_tokens(req.input_token, req.output_token)
        .await?;

    let (amount, price_cap, wrap_ratios) = normalize_calldata_request_values(
        ds,
        CalldataRequestNormalization {
            denomination: req.denomination,
            input_token: req.input_token,
            output_token: req.output_token,
            mode: req.mode,
            amount: req.amount,
            amount_field: req.amount_field,
            price_cap: req.price_cap,
            price_cap_field: req.price_cap_field,
        },
    )
    .await?;

    let take_req = TakeOrdersRequest {
        taker: req.taker.to_string(),
        chain_id: crate::CHAIN_ID,
        sell_token: req.input_token.to_string(),
        buy_token: req.output_token.to_string(),
        mode: req.mode,
        amount,
        price_cap,
    };

    let response = ds.get_calldata(take_req).await?;
    normalize_calldata_response(&wrap_ratios, req.denomination, req.input_token, response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::swap::test_fixtures::MockSwapDataSource;
    use crate::test_helpers::TestClientBuilder;
    use crate::types::common::Approval;
    use crate::types::swap::{SwapCalldataMode, SwapDenomination};
    use crate::wrap_ratio::WrapRatioValue;
    use alloy::primitives::{address, Address, Bytes, U256};
    use async_trait::async_trait;
    use rocket::http::{ContentType, Status};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    const USDC: Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    const WETH: Address = address!("4200000000000000000000000000000000000006");
    const TAKER: Address = address!("1111111111111111111111111111111111111111");
    const ORDERBOOK: Address = address!("d2938e7c9fe3597f78832ce780feb61945c377d7");
    const WT_MSTR: Address = address!("Ff05e1BD696900DC6A52cA35cA61bB1024eDA8e2");
    const WT_COIN: Address = address!("EeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE");

    fn calldata_request(output_amount: &str, max_ratio: &str) -> SwapCalldataRequest {
        SwapCalldataRequest {
            taker: TAKER,
            input_token: USDC,
            output_token: WETH,
            output_amount: output_amount.to_string(),
            maximum_io_ratio: max_ratio.to_string(),
            denomination: SwapDenomination::Wrapped,
        }
    }

    fn calldata_v2_request(
        mode: SwapCalldataMode,
        amount: &str,
        price_cap: &str,
    ) -> SwapCalldataV2Request {
        SwapCalldataV2Request {
            taker: TAKER,
            input_token: USDC,
            output_token: WETH,
            mode,
            amount: amount.to_string(),
            price_cap: price_cap.to_string(),
            denomination: SwapDenomination::Wrapped,
        }
    }

    fn unwrapped_calldata_request(
        input_token: Address,
        output_token: Address,
        output_amount: &str,
        max_ratio: &str,
    ) -> SwapCalldataRequest {
        SwapCalldataRequest {
            taker: TAKER,
            input_token,
            output_token,
            output_amount: output_amount.to_string(),
            maximum_io_ratio: max_ratio.to_string(),
            denomination: SwapDenomination::Unwrapped,
        }
    }

    fn unwrapped_calldata_v2_request(
        input_token: Address,
        output_token: Address,
        mode: SwapCalldataMode,
        amount: &str,
        price_cap: &str,
    ) -> SwapCalldataV2Request {
        SwapCalldataV2Request {
            taker: TAKER,
            input_token,
            output_token,
            mode,
            amount: amount.to_string(),
            price_cap: price_cap.to_string(),
            denomination: SwapDenomination::Unwrapped,
        }
    }

    fn ready_response() -> SwapCalldataResponse {
        SwapCalldataResponse {
            to: ORDERBOOK,
            data: Bytes::from(vec![0xab, 0xcd, 0xef]),
            value: U256::ZERO,
            estimated_input: "150".to_string(),
            denomination: SwapDenomination::Wrapped,
            approvals: vec![],
        }
    }

    fn approval_response() -> SwapCalldataResponse {
        SwapCalldataResponse {
            to: ORDERBOOK,
            data: Bytes::new(),
            value: U256::ZERO,
            estimated_input: "1000".to_string(),
            denomination: SwapDenomination::Wrapped,
            approvals: vec![Approval {
                token: USDC,
                spender: ORDERBOOK,
                amount: "1000".to_string(),
                symbol: String::new(),
                approval_data: Bytes::from(vec![0x09, 0x5e, 0xa7, 0xb3]),
            }],
        }
    }

    fn wrap_ratio(share_address: Address, assets_per_share: &str) -> WrapRatioValue {
        WrapRatioValue {
            share_address,
            assets_per_share: assets_per_share.to_string(),
        }
    }

    fn capture_ds(
        response: SwapCalldataResponse,
        wrap_ratios: HashMap<Address, WrapRatioValue>,
    ) -> (
        MockCalldataDataSource,
        Arc<Mutex<Option<TakeOrdersRequest>>>,
    ) {
        capture_ds_with_wrap_result(response, Ok(wrap_ratios))
    }

    fn capture_ds_with_wrap_result(
        response: SwapCalldataResponse,
        wrap_ratios: Result<HashMap<Address, WrapRatioValue>, ApiError>,
    ) -> (
        MockCalldataDataSource,
        Arc<Mutex<Option<TakeOrdersRequest>>>,
    ) {
        let captured_request = Arc::new(Mutex::new(None));
        (
            MockCalldataDataSource {
                base: MockSwapDataSource {
                    supported_tokens: Ok(()),
                    orders: Ok(vec![]),
                    candidates: vec![],
                    calldata_result: Ok(response),
                },
                wrap_ratios,
                captured_request: Arc::clone(&captured_request),
            },
            captured_request,
        )
    }

    struct MockCalldataDataSource {
        base: MockSwapDataSource,
        wrap_ratios: Result<HashMap<Address, WrapRatioValue>, ApiError>,
        captured_request: Arc<Mutex<Option<TakeOrdersRequest>>>,
    }

    #[async_trait]
    impl SwapDataSource for MockCalldataDataSource {
        async fn validate_supported_tokens(
            &self,
            input_token: Address,
            output_token: Address,
        ) -> Result<(), ApiError> {
            self.base
                .validate_supported_tokens(input_token, output_token)
                .await
        }

        async fn get_orders_for_pair(
            &self,
            input_token: Address,
            output_token: Address,
        ) -> Result<Vec<rain_orderbook_common::raindex_client::orders::RaindexOrder>, ApiError>
        {
            self.base
                .get_orders_for_pair(input_token, output_token)
                .await
        }

        async fn build_candidates_for_pair(
            &self,
            orders: &[rain_orderbook_common::raindex_client::orders::RaindexOrder],
            input_token: Address,
            output_token: Address,
        ) -> Result<Vec<rain_orderbook_common::take_orders::TakeOrderCandidate>, ApiError> {
            self.base
                .build_candidates_for_pair(orders, input_token, output_token)
                .await
        }

        async fn get_calldata(
            &self,
            request: TakeOrdersRequest,
        ) -> Result<SwapCalldataResponse, ApiError> {
            *self.captured_request.lock().unwrap() = Some(request);
            self.base.calldata_result.clone()
        }

        async fn get_wrap_ratios_for_tokens(
            &self,
            token_addresses: &[Address],
        ) -> Result<HashMap<Address, WrapRatioValue>, ApiError> {
            let wrap_ratios = self.wrap_ratios.clone()?;
            Ok(token_addresses
                .iter()
                .filter_map(|address| {
                    wrap_ratios
                        .get(address)
                        .map(|ratio| (*address, ratio.clone()))
                })
                .collect())
        }
    }

    fn captured_take_orders_request(
        captured_request: &Arc<Mutex<Option<TakeOrdersRequest>>>,
    ) -> TakeOrdersRequest {
        captured_request.lock().unwrap().clone().unwrap()
    }

    fn no_take_orders_request_was_made(captured_request: &Arc<Mutex<Option<TakeOrdersRequest>>>) {
        assert!(captured_request.lock().unwrap().is_none());
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_ready() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
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
        assert_eq!(result.denomination, SwapDenomination::Wrapped);
        assert!(result.approvals.is_empty());
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_needs_approval() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(approval_response()),
        };
        let result = process_swap_calldata(&ds, calldata_request("100", "2.5"))
            .await
            .unwrap();

        assert_eq!(result.to, ORDERBOOK);
        assert!(result.data.is_empty());
        assert_eq!(result.denomination, SwapDenomination::Wrapped);
        assert_eq!(result.approvals.len(), 1);
        assert_eq!(result.approvals[0].token, USDC);
        assert_eq!(result.approvals[0].spender, ORDERBOOK);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_default_denomination_preserves_request() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let result = process_swap_calldata(&ds, calldata_request("100", "2.5"))
            .await
            .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.sell_token, USDC.to_string());
        assert_eq!(request.buy_token, WETH.to_string());
        assert_eq!(request.mode, TakeOrdersMode::BuyUpTo);
        assert_eq!(request.amount, "100");
        assert_eq!(request.price_cap, "2.5");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.denomination, SwapDenomination::Wrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_spend_exact_preserves_request() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let result = process_swap_calldata_v2(
            &ds,
            calldata_v2_request(SwapCalldataMode::SpendExact, "100", "2.5"),
        )
        .await
        .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.sell_token, USDC.to_string());
        assert_eq!(request.buy_token, WETH.to_string());
        assert_eq!(request.mode, TakeOrdersMode::SpendExact);
        assert_eq!(request.amount, "100");
        assert_eq!(request.price_cap, "2.5");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.denomination, SwapDenomination::Wrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_spend_up_to_preserves_request() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let result = process_swap_calldata_v2(
            &ds,
            calldata_v2_request(SwapCalldataMode::SpendUpTo, "75", "3"),
        )
        .await
        .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.mode, TakeOrdersMode::SpendUpTo);
        assert_eq!(request.amount, "75");
        assert_eq!(request.price_cap, "3");
        assert_eq!(result.denomination, SwapDenomination::Wrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_buy_up_to_preserves_request() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let result = process_swap_calldata_v2(
            &ds,
            calldata_v2_request(SwapCalldataMode::BuyUpTo, "50", "2"),
        )
        .await
        .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.mode, TakeOrdersMode::BuyUpTo);
        assert_eq!(request.amount, "50");
        assert_eq!(request.price_cap, "2");
        assert_eq!(result.denomination, SwapDenomination::Wrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_explicit_wrapped_preserves_request() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let mut request = calldata_request("100", "2.5");
        request.denomination = SwapDenomination::Wrapped;
        let result = process_swap_calldata(&ds, request).await.unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.amount, "100");
        assert_eq!(request.price_cap, "2.5");
        assert_eq!(result.denomination, SwapDenomination::Wrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_converts_wrapped_output_amount() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result =
            process_swap_calldata(&ds, unwrapped_calldata_request(USDC, WT_MSTR, "100", "2.5"))
                .await
                .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.sell_token, USDC.to_string());
        assert_eq!(request.buy_token, WT_MSTR.to_string());
        assert_eq!(request.amount, "50");
        assert_eq!(request.price_cap, "5");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_converts_wrapped_input_ratio_and_response() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result =
            process_swap_calldata(&ds, unwrapped_calldata_request(WT_MSTR, WETH, "100", "2.5"))
                .await
                .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.sell_token, WT_MSTR.to_string());
        assert_eq!(request.buy_token, WETH.to_string());
        assert_eq!(request.amount, "100");
        assert_eq!(request.price_cap, "1.25");
        assert_eq!(result.estimated_input, "300");
        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_unwrapped_spend_converts_wrapped_input_amount() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result = process_swap_calldata_v2(
            &ds,
            unwrapped_calldata_v2_request(
                WT_MSTR,
                WETH,
                SwapCalldataMode::SpendExact,
                "100",
                "2.5",
            ),
        )
        .await
        .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.sell_token, WT_MSTR.to_string());
        assert_eq!(request.buy_token, WETH.to_string());
        assert_eq!(request.mode, TakeOrdersMode::SpendExact);
        assert_eq!(request.amount, "50");
        assert_eq!(request.price_cap, "1.25");
        assert_eq!(result.estimated_input, "300");
        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_unwrapped_buy_converts_wrapped_output_amount() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_COIN, wrap_ratio(WT_COIN, "4"))]),
        );
        let result = process_swap_calldata_v2(
            &ds,
            unwrapped_calldata_v2_request(USDC, WT_COIN, SwapCalldataMode::BuyUpTo, "100", "2.5"),
        )
        .await
        .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.mode, TakeOrdersMode::BuyUpTo);
        assert_eq!(request.amount, "25");
        assert_eq!(request.price_cap, "10");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_converts_both_wrapped_sides() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([
                (WT_MSTR, wrap_ratio(WT_MSTR, "2")),
                (WT_COIN, wrap_ratio(WT_COIN, "4")),
            ]),
        );
        let result = process_swap_calldata(
            &ds,
            unwrapped_calldata_request(WT_MSTR, WT_COIN, "100", "2.5"),
        )
        .await
        .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.amount, "25");
        assert_eq!(request.price_cap, "5");
        assert_eq!(result.estimated_input, "300");
        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_noop_for_non_wrapped_tokens() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let result =
            process_swap_calldata(&ds, unwrapped_calldata_request(USDC, WETH, "100.0", "2.50"))
                .await
                .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.amount, "100.0");
        assert_eq!(request.price_cap, "2.50");
        assert_eq!(result.estimated_input, "150");
        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_keeps_approval_amount_wrapped() {
        let (ds, captured_request) = capture_ds(
            SwapCalldataResponse {
                estimated_input: "1000".to_string(),
                approvals: vec![Approval {
                    token: WT_MSTR,
                    spender: ORDERBOOK,
                    amount: "1000".to_string(),
                    symbol: "wtMSTR".to_string(),
                    approval_data: Bytes::from(vec![0x09, 0x5e, 0xa7, 0xb3]),
                }],
                ..approval_response()
            },
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result =
            process_swap_calldata(&ds, unwrapped_calldata_request(WT_MSTR, WETH, "100", "2.5"))
                .await
                .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.price_cap, "1.25");
        assert_eq!(result.estimated_input, "2000");
        assert_eq!(result.denomination, SwapDenomination::Unwrapped);
        assert_eq!(result.approvals.len(), 1);
        assert_eq!(result.approvals[0].token, WT_MSTR);
        assert_eq!(result.approvals[0].amount, "1000");
        assert_eq!(result.approvals[0].symbol, "wtMSTR");
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_invalid_output_amount_is_bad_request() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result = process_swap_calldata(
            &ds,
            unwrapped_calldata_request(USDC, WT_MSTR, "not-a-number", "2.5"),
        )
        .await;

        assert!(matches!(result, Err(ApiError::BadRequest(msg)) if msg == "invalid output_amount"));
        no_take_orders_request_was_made(&captured_request);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_invalid_maximum_io_ratio_is_bad_request() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result = process_swap_calldata(
            &ds,
            unwrapped_calldata_request(USDC, WT_MSTR, "100", "not-a-number"),
        )
        .await;

        assert!(
            matches!(result, Err(ApiError::BadRequest(msg)) if msg == "invalid maximum_io_ratio")
        );
        no_take_orders_request_was_made(&captured_request);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_unwrapped_invalid_amount_is_bad_request() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result = process_swap_calldata_v2(
            &ds,
            unwrapped_calldata_v2_request(
                WT_MSTR,
                WETH,
                SwapCalldataMode::SpendUpTo,
                "not-a-number",
                "2.5",
            ),
        )
        .await;

        assert!(matches!(result, Err(ApiError::BadRequest(msg)) if msg == "invalid amount"));
        no_take_orders_request_was_made(&captured_request);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_unwrapped_invalid_price_cap_is_bad_request() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result = process_swap_calldata_v2(
            &ds,
            unwrapped_calldata_v2_request(
                WT_MSTR,
                WETH,
                SwapCalldataMode::SpendUpTo,
                "100",
                "not-a-number",
            ),
        )
        .await;

        assert!(matches!(result, Err(ApiError::BadRequest(msg)) if msg == "invalid price_cap"));
        no_take_orders_request_was_made(&captured_request);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_wrap_ratio_lookup_failure() {
        let (ds, captured_request) = capture_ds_with_wrap_result(
            ready_response(),
            Err(ApiError::Internal("failed to read wrap ratios".into())),
        );
        let result =
            process_swap_calldata(&ds, unwrapped_calldata_request(WT_MSTR, WETH, "100", "2.5"))
                .await;

        assert!(
            matches!(result, Err(ApiError::Internal(msg)) if msg == "failed to read wrap ratios")
        );
        no_take_orders_request_was_made(&captured_request);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_malformed_wrap_ratio_is_internal_error() {
        let (ds, captured_request) = capture_ds(
            ready_response(),
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "not-a-number"))]),
        );
        let result =
            process_swap_calldata(&ds, unwrapped_calldata_request(USDC, WT_MSTR, "100", "2.5"))
                .await;

        assert!(
            matches!(result, Err(ApiError::Internal(msg)) if msg == "failed to read wrapped token ratio")
        );
        no_take_orders_request_was_made(&captured_request);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_unwrapped_invalid_estimated_input_is_internal_error() {
        let (ds, captured_request) = capture_ds(
            SwapCalldataResponse {
                estimated_input: "not-a-number".to_string(),
                ..ready_response()
            },
            HashMap::from([(WT_MSTR, wrap_ratio(WT_MSTR, "2"))]),
        );
        let result =
            process_swap_calldata(&ds, unwrapped_calldata_request(WT_MSTR, WETH, "100", "2.5"))
                .await;

        let request = captured_take_orders_request(&captured_request);
        assert_eq!(request.price_cap, "1.25");
        assert!(
            matches!(result, Err(ApiError::Internal(msg)) if msg == "failed to read estimated_input")
        );
    }

    #[test]
    fn test_swap_calldata_request_defaults_to_wrapped_denomination() {
        let request: SwapCalldataRequest = serde_json::from_str(
            r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100","maximumIoRatio":"2.5"}"#,
        )
        .unwrap();

        assert_eq!(request.denomination, SwapDenomination::Wrapped);
    }

    #[test]
    fn test_swap_calldata_v2_request_defaults_to_wrapped_denomination() {
        let request: SwapCalldataV2Request = serde_json::from_str(
            r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","mode":"spendExact","amount":"100","priceCap":"2.5"}"#,
        )
        .unwrap();

        assert_eq!(request.mode, SwapCalldataMode::SpendExact);
        assert_eq!(request.denomination, SwapDenomination::Wrapped);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_not_found() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
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
            supported_tokens: Ok(()),
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
            supported_tokens: Ok(()),
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("failed to generate calldata".into())),
        };
        let result = process_swap_calldata(&ds, calldata_request("100", "2.5")).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_rejects_unsupported_tokens() {
        let ds = MockSwapDataSource {
            supported_tokens: Err(ApiError::BadRequest(
                "unsupported token for this API".into(),
            )),
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(ready_response()),
        };
        let result = process_swap_calldata(&ds, calldata_request("100", "2.5")).await;
        assert!(
            matches!(result, Err(ApiError::BadRequest(msg)) if msg.contains("unsupported token"))
        );
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

    #[rocket::async_test]
    async fn test_swap_calldata_v2_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v2/swap/calldata")
            .header(ContentType::JSON)
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","mode":"spendExact","amount":"100","priceCap":"2.5"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_swap_calldata_400_for_unsupported_tokens() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = crate::test_helpers::seed_api_key(&client).await;
        let header = crate::test_helpers::basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/swap/calldata")
            .header(ContentType::JSON)
            .header(rocket::http::Header::new("Authorization", header))
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100","maximumIoRatio":"2.5"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn test_swap_calldata_v2_400_for_unsupported_tokens() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = crate::test_helpers::seed_api_key(&client).await;
        let header = crate::test_helpers::basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v2/swap/calldata")
            .header(ContentType::JSON)
            .header(rocket::http::Header::new("Authorization", header))
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","mode":"spendExact","amount":"100","priceCap":"2.5"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn test_swap_calldata_422_for_invalid_denomination() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = crate::test_helpers::seed_api_key(&client).await;
        let header = crate::test_helpers::basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/swap/calldata")
            .header(ContentType::JSON)
            .header(rocket::http::Header::new("Authorization", header))
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100","maximumIoRatio":"2.5","denomination":"invalid"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }

    #[rocket::async_test]
    async fn test_swap_calldata_v2_422_for_buy_exact_mode() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = crate::test_helpers::seed_api_key(&client).await;
        let header = crate::test_helpers::basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v2/swap/calldata")
            .header(ContentType::JSON)
            .header(rocket::http::Header::new("Authorization", header))
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","mode":"buyExact","amount":"100","priceCap":"2.5"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }
}
