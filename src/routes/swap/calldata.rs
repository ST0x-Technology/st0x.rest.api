use super::{RaindexSwapDataSource, SwapDataSource};
use crate::analytics::{swap_calldata_generated_event, Analytics, ApiVersion};
use crate::app_state::ApplicationState;
use crate::attribution::{attribution_message_hash, Attribution, AttributionSigner};
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::routes::swap::denomination::{
    denormalize_calldata_price_cap, normalize_calldata_price_cap,
    normalize_calldata_request_amount, normalize_calldata_request_values,
    normalize_calldata_response, CalldataAmountNormalization, CalldataRequestNormalization,
};
use crate::types::swap::{
    SwapCalldataRequest, SwapCalldataResponse, SwapCalldataV2Request, SwapCalldataV2RequestBody,
    SwapCalldataV2Response,
};
use alloy::primitives::{keccak256, Address, Bytes, Signature};
use alloy::sol_types::{SolCall, SolValue};
use rain_math_float::Float;
use rain_orderbook_bindings::IRaindexV6::takeOrders4Call;
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
#[allow(clippy::too_many_arguments)] // Rocket handler: args are guards + managed state + body.
pub async fn post_swap_calldata(
    _global: GlobalRateLimit,
    key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    analytics: &State<Analytics>,
    span: TracingSpan,
    request: Json<SwapCalldataRequest>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
    async move {
        let req = request.into_inner();
        tracing::info!(body = ?req, "request received");
        let attribution = app_state.attribution.for_api_key(&key.key_id, req.taker);
        let raindex = shared_raindex.read().await;
        let ds = RaindexSwapDataSource {
            client: raindex.client(),
            caches: &app_state.response_caches,
            pool: pool.inner(),
        };
        let response = handle_swap_calldata(
            &ds,
            &key,
            analytics.inner(),
            &app_state.attribution.signer,
            &attribution,
            req,
        )
        .await?;

        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    post,
    path = "/v2/swap/calldata",
    tag = "Swap",
    summary = "Build ready-to-send swap calldata",
    description = "Builds SDK calldata for one executable route. Provide exactly one of priceCap or slippageBps. When approvals are returned, submit them and retry with the response's resolvedPriceCap as priceCap so the original limit remains fixed.",
    security(("basicAuth" = [])),
    request_body = SwapCalldataV2RequestBody,
    responses(
        (status = 200, description = "Swap calldata", body = SwapCalldataV2Response),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "No liquidity found", body = ApiErrorResponse),
        (status = 422, description = "Request body could not be parsed", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/calldata", data = "<request>")]
#[allow(clippy::too_many_arguments)] // Rocket handler: args are guards + managed state + body.
pub async fn post_swap_calldata_v2(
    _global: GlobalRateLimit,
    key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    app_state: &State<ApplicationState>,
    pool: &State<DbPool>,
    analytics: &State<Analytics>,
    span: TracingSpan,
    request: Json<SwapCalldataV2Request>,
) -> Result<Json<SwapCalldataV2Response>, ApiError> {
    async move {
        let req = request.into_inner();
        tracing::info!(
            mode = ?req.mode,
            denomination = ?req.denomination,
            "request received"
        );
        let attribution = app_state.attribution.for_api_key(&key.key_id, req.taker);
        let raindex = shared_raindex.read().await;
        let ds = RaindexSwapDataSource {
            client: raindex.client(),
            caches: &app_state.response_caches,
            pool: pool.inner(),
        };
        let response = handle_swap_calldata_v2(
            &ds,
            &key,
            analytics.inner(),
            &app_state.attribution.signer,
            &attribution,
            req,
        )
        .await?;

        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn handle_swap_calldata(
    ds: &dyn SwapDataSource,
    key: &AuthenticatedKey,
    analytics: &Analytics,
    signer: &AttributionSigner,
    attribution: &Attribution,
    req: SwapCalldataRequest,
) -> Result<SwapCalldataResponse, ApiError> {
    let analytics_context = analytics.is_enabled().then(|| {
        (
            req.taker,
            req.input_token,
            req.output_token,
            serde_json::to_value(req.denomination).unwrap_or(serde_json::Value::Null),
        )
    });
    let mut response = process_swap_calldata(ds, req).await?;
    embed_and_validate_attribution(&mut response, signer, attribution).await?;

    if let Some((taker, input_token, output_token, denomination)) = analytics_context {
        analytics.capture(|| {
            swap_calldata_generated_event(
                key,
                taker,
                input_token,
                output_token,
                denomination,
                ApiVersion::V1,
                None,
                &response,
            )
        });
    }

    Ok(response)
}

async fn handle_swap_calldata_v2(
    ds: &dyn SwapDataSource,
    key: &AuthenticatedKey,
    analytics: &Analytics,
    signer: &AttributionSigner,
    attribution: &Attribution,
    req: SwapCalldataV2Request,
) -> Result<SwapCalldataV2Response, ApiError> {
    let analytics_context = analytics.is_enabled().then(|| {
        (
            req.taker,
            req.input_token,
            req.output_token,
            serde_json::to_value(req.denomination).unwrap_or(serde_json::Value::Null),
            serde_json::to_value(req.mode).ok(),
        )
    });
    let mut response = process_swap_calldata_v2(ds, req).await?;
    embed_and_validate_attribution(&mut response.calldata, signer, attribution).await?;

    if let Some((taker, input_token, output_token, denomination, mode)) = analytics_context {
        analytics.capture(|| {
            swap_calldata_generated_event(
                key,
                taker,
                input_token,
                output_token,
                denomination,
                ApiVersion::V2,
                mode,
                &response.calldata,
            )
        });
    }

    Ok(response)
}

async fn embed_and_validate_attribution(
    response: &mut SwapCalldataResponse,
    signer: &AttributionSigner,
    attribution: &Attribution,
) -> Result<(), ApiError> {
    if !response.approvals.is_empty() || response.data.is_empty() {
        return validate_attribution_calldata(response, signer.address(), attribution);
    }

    let mut decoded = takeOrders4Call::abi_decode(&response.data).map_err(|error| {
        tracing::error!(
            %error,
            api_key_hash = %attribution.api_key_hash,
            "failed to decode generated takeOrders4 calldata for attribution"
        );
        ApiError::Internal("generated swap calldata is missing attribution".into())
    })?;
    for order_config in &mut decoded.config.orders {
        let order_hash = keccak256(order_config.order.abi_encode());
        let context = signer
            .sign_context(attribution, order_hash)
            .await
            .map_err(|error| {
                tracing::error!(
                    %error,
                    %order_hash,
                    api_key_hash = %attribution.api_key_hash,
                    "failed to sign REST attribution context"
                );
                ApiError::Internal("failed to sign swap calldata attribution".into())
            })?;
        order_config.signedContext.push(context);
    }
    response.data = Bytes::from(decoded.abi_encode());

    validate_attribution_calldata(response, signer.address(), attribution)
}

fn validate_attribution_calldata(
    response: &SwapCalldataResponse,
    signer_address: Address,
    attribution: &Attribution,
) -> Result<(), ApiError> {
    if !response.approvals.is_empty() {
        if !response.data.is_empty() {
            tracing::error!(
                api_key_hash = %attribution.api_key_hash,
                "swap response unexpectedly contains approvals and executable calldata"
            );
            return Err(ApiError::Internal(
                "generated swap calldata has an invalid state".into(),
            ));
        }
        tracing::info!(
            api_key_hash = %attribution.api_key_hash,
            "swap calldata requires approval; executable attribution is not expected"
        );
        return Ok(());
    }

    if response.data.is_empty() {
        tracing::error!(
            api_key_hash = %attribution.api_key_hash,
            "swap response contains neither approvals nor executable calldata"
        );
        return Err(ApiError::Internal(
            "generated swap calldata is missing attribution".into(),
        ));
    }
    let decoded = takeOrders4Call::abi_decode(&response.data).map_err(|error| {
        tracing::error!(
            %error,
            api_key_hash = %attribution.api_key_hash,
            "failed to decode generated takeOrders4 calldata for attribution validation"
        );
        ApiError::Internal("generated swap calldata is missing attribution".into())
    })?;
    if decoded.config.orders.is_empty() {
        tracing::error!(
            api_key_hash = %attribution.api_key_hash,
            "generated takeOrders4 calldata contains no orders"
        );
        return Err(ApiError::Internal(
            "generated swap calldata is missing attribution".into(),
        ));
    }

    for order_config in &decoded.config.orders {
        let order_hash = keccak256(order_config.order.abi_encode());
        let expected_context = attribution.context_for_order(order_hash);
        let mut matching_attribution = order_config.signedContext.iter().filter(|context| {
            context.signer == signer_address && context.context.as_slice() == expected_context
        });
        let first_match = matching_attribution.next();
        let has_duplicate = matching_attribution.next().is_some();
        let signature_is_valid = if let Some(context) = first_match {
            let context_hash = attribution_message_hash(&expected_context);
            Signature::try_from(context.signature.as_ref())
                .and_then(|signature| signature.recover_address_from_msg(context_hash.as_slice()))
                .is_ok_and(|recovered| recovered == signer_address)
        } else {
            false
        };
        if first_match.is_none() || has_duplicate || !signature_is_valid {
            tracing::error!(
                %order_hash,
                api_key_hash = %attribution.api_key_hash,
                signer = %signer_address,
                duplicate_attribution_context = has_duplicate,
                "generated order is missing its exact REST attribution context"
            );
            return Err(ApiError::Internal(
                "generated swap calldata is missing attribution".into(),
            ));
        }
    }

    Ok(())
}

#[derive(Debug)]
struct SwapCalldataBuildRequest {
    taker: Address,
    input_token: Address,
    output_token: Address,
    mode: TakeOrdersMode,
    amount: String,
    amount_field: &'static str,
    price_limit: SwapCalldataPriceLimit,
    denomination: crate::types::swap::SwapDenomination,
}

#[derive(Debug)]
enum SwapCalldataPriceLimit {
    Explicit {
        value: String,
        field: &'static str,
    },
    SlippageBps {
        slippage_bps: u16,
        reference_io_ratio: Option<String>,
    },
}

struct SwapCalldataBuildResult {
    calldata: SwapCalldataResponse,
    resolved_price_cap: String,
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
            price_limit: SwapCalldataPriceLimit::Explicit {
                value: req.maximum_io_ratio,
                field: "maximum_io_ratio",
            },
            denomination: req.denomination,
        }
    }
}

impl TryFrom<SwapCalldataV2Request> for SwapCalldataBuildRequest {
    type Error = ApiError;

    fn try_from(req: SwapCalldataV2Request) -> Result<Self, Self::Error> {
        let price_limit = match (req.price_cap, req.slippage_bps, req.reference_io_ratio) {
            (Some(value), None, None) => SwapCalldataPriceLimit::Explicit {
                value,
                field: "price_cap",
            },
            (Some(_), None, Some(_)) => {
                tracing::warn!(
                    "swap calldata rejected because reference_io_ratio was provided without slippage_bps"
                );
                return Err(ApiError::BadRequest(
                    "reference_io_ratio requires slippage_bps".into(),
                ));
            }
            (None, Some(slippage_bps @ 1..=5000), reference_io_ratio) => {
                SwapCalldataPriceLimit::SlippageBps {
                    slippage_bps,
                    reference_io_ratio,
                }
            }
            (None, Some(_), _) => {
                tracing::warn!("swap calldata rejected for out-of-range slippage_bps");
                return Err(ApiError::BadRequest(
                    "slippage_bps must be between 1 and 5000".into(),
                ));
            }
            _ => {
                tracing::warn!("swap calldata rejected without exactly one price limit");
                return Err(ApiError::BadRequest(
                    "provide exactly one of price_cap or slippage_bps".into(),
                ));
            }
        };

        Ok(Self {
            taker: req.taker,
            input_token: req.input_token,
            output_token: req.output_token,
            mode: req.mode.into(),
            amount: req.amount,
            amount_field: "amount",
            price_limit,
            denomination: req.denomination,
        })
    }
}

async fn process_swap_calldata(
    ds: &dyn SwapDataSource,
    req: SwapCalldataRequest,
) -> Result<SwapCalldataResponse, ApiError> {
    Ok(process_swap_calldata_build(ds, req.into()).await?.calldata)
}

async fn process_swap_calldata_v2(
    ds: &dyn SwapDataSource,
    req: SwapCalldataV2Request,
) -> Result<SwapCalldataV2Response, ApiError> {
    let result = process_swap_calldata_build(ds, req.try_into()?).await?;
    Ok(SwapCalldataV2Response {
        calldata: result.calldata,
        resolved_price_cap: result.resolved_price_cap,
    })
}

async fn process_swap_calldata_build(
    ds: &dyn SwapDataSource,
    req: SwapCalldataBuildRequest,
) -> Result<SwapCalldataBuildResult, ApiError> {
    ds.validate_supported_tokens(req.input_token, req.output_token)
        .await?;

    let (amount, price_cap, resolved_price_cap, wrap_ratios) = match req.price_limit {
        SwapCalldataPriceLimit::Explicit { value, field } => {
            let resolved_price_cap = value.clone();
            let (amount, price_cap, wrap_ratios) = normalize_calldata_request_values(
                ds,
                CalldataRequestNormalization {
                    denomination: req.denomination,
                    input_token: req.input_token,
                    output_token: req.output_token,
                    mode: req.mode,
                    amount: req.amount,
                    amount_field: req.amount_field,
                    price_cap: value,
                    price_cap_field: field,
                },
            )
            .await?;
            (amount, price_cap, resolved_price_cap, wrap_ratios)
        }
        SwapCalldataPriceLimit::SlippageBps {
            slippage_bps,
            reference_io_ratio,
        } => {
            let (amount, wrap_ratios) = normalize_calldata_request_amount(
                ds,
                CalldataAmountNormalization {
                    denomination: req.denomination,
                    input_token: req.input_token,
                    output_token: req.output_token,
                    mode: req.mode,
                    amount: req.amount,
                    amount_field: req.amount_field,
                },
            )
            .await?;
            let reference_io_ratio = reference_io_ratio
                .map(|reference_io_ratio| {
                    normalize_calldata_price_cap(
                        reference_io_ratio,
                        "reference_io_ratio",
                        req.denomination,
                        req.input_token,
                        req.output_token,
                        &wrap_ratios,
                    )
                    .and_then(|reference_io_ratio| {
                        Float::parse(reference_io_ratio).map_err(|error| {
                            tracing::warn!(
                                %error,
                                "swap calldata rejected for invalid reference_io_ratio"
                            );
                            ApiError::BadRequest("invalid reference_io_ratio".into())
                        })
                    })
                })
                .transpose()?;
            let orders = ds
                .get_orders_for_pair(req.input_token, req.output_token)
                .await?;
            if orders.is_empty() {
                return Err(ApiError::NotFound(
                    "no liquidity found for this pair".into(),
                ));
            }
            let candidates = ds
                .build_candidates_for_pair(&orders, req.input_token, req.output_token, req.taker)
                .await?;
            let price_cap = super::slippage::resolve_slippage_price_cap(
                candidates,
                req.mode,
                &amount,
                slippage_bps,
                reference_io_ratio,
            )?;
            let resolved_price_cap = denormalize_calldata_price_cap(
                price_cap,
                req.denomination,
                req.input_token,
                req.output_token,
                &wrap_ratios,
            )?;
            tracing::info!(
                slippage_bps,
                resolved_price_cap = %resolved_price_cap,
                denomination = ?req.denomination,
                "resolved swap slippage price cap"
            );
            let price_cap = price_cap.format().map_err(|error| {
                tracing::error!(%error, "failed to format resolved slippage price cap");
                ApiError::Internal("failed to resolve slippage".into())
            })?;
            (amount, price_cap, resolved_price_cap, wrap_ratios)
        }
    };

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
    let calldata =
        normalize_calldata_response(&wrap_ratios, req.denomination, req.input_token, response)?;
    Ok(SwapCalldataBuildResult {
        calldata,
        resolved_price_cap,
    })
}

#[cfg(all(test, feature = "oracle-integration-tests"))]
mod oracle_integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{Analytics, RecordingSink};
    use crate::auth::AuthenticatedKey;
    use crate::routes::swap::test_fixtures::MockSwapDataSource;
    use crate::test_helpers::{mock_candidate, mock_order, TestClientBuilder};
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
    type CapturedTakeOrdersRequest = Arc<Mutex<Option<TakeOrdersRequest>>>;
    type CapturedCounterparty = Arc<Mutex<Option<Address>>>;

    fn test_key() -> AuthenticatedKey {
        AuthenticatedKey {
            id: 1,
            key_id: "test-client".to_string(),
            label: "Test client".to_string(),
            owner: "test-owner".to_string(),
            is_admin: false,
        }
    }

    fn test_attribution_state() -> crate::attribution::AttributionState {
        crate::attribution::AttributionState::new(
            crate::attribution::AttributionSigner::from_hex_key(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .expect("test attribution signer"),
        )
    }

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
            price_cap: Some(price_cap.to_string()),
            slippage_bps: None,
            reference_io_ratio: None,
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
            price_cap: Some(price_cap.to_string()),
            slippage_bps: None,
            reference_io_ratio: None,
            denomination: SwapDenomination::Unwrapped,
        }
    }

    fn slippage_v2_request(
        mode: SwapCalldataMode,
        amount: &str,
        slippage_bps: u16,
    ) -> SwapCalldataV2Request {
        SwapCalldataV2Request {
            taker: TAKER,
            input_token: USDC,
            output_token: WETH,
            mode,
            amount: amount.to_string(),
            price_cap: None,
            slippage_bps: Some(slippage_bps),
            reference_io_ratio: None,
            denomination: SwapDenomination::Wrapped,
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
    ) -> (MockCalldataDataSource, CapturedTakeOrdersRequest) {
        capture_ds_with_wrap_result(response, Ok(wrap_ratios))
    }

    fn capture_ds_with_wrap_result(
        response: SwapCalldataResponse,
        wrap_ratios: Result<HashMap<Address, WrapRatioValue>, ApiError>,
    ) -> (MockCalldataDataSource, CapturedTakeOrdersRequest) {
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
                captured_counterparty: Arc::new(Mutex::new(None)),
            },
            captured_request,
        )
    }

    fn capture_slippage_ds(
        wrap_ratios: HashMap<Address, WrapRatioValue>,
    ) -> (
        MockCalldataDataSource,
        CapturedTakeOrdersRequest,
        CapturedCounterparty,
    ) {
        let captured_request = Arc::new(Mutex::new(None));
        let captured_counterparty = Arc::new(Mutex::new(None));
        (
            MockCalldataDataSource {
                base: MockSwapDataSource {
                    supported_tokens: Ok(()),
                    orders: Ok(vec![mock_order()]),
                    candidates: vec![mock_candidate("100", "2")],
                    calldata_result: Ok(ready_response()),
                },
                wrap_ratios: Ok(wrap_ratios),
                captured_request: Arc::clone(&captured_request),
                captured_counterparty: Arc::clone(&captured_counterparty),
            },
            captured_request,
            captured_counterparty,
        )
    }

    struct MockCalldataDataSource {
        base: MockSwapDataSource,
        wrap_ratios: Result<HashMap<Address, WrapRatioValue>, ApiError>,
        captured_request: CapturedTakeOrdersRequest,
        captured_counterparty: CapturedCounterparty,
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
            counterparty: Address,
        ) -> Result<Vec<rain_orderbook_common::take_orders::TakeOrderCandidate>, ApiError> {
            *self.captured_counterparty.lock().unwrap() = Some(counterparty);
            self.base
                .build_candidates_for_pair(orders, input_token, output_token, counterparty)
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
        captured_request: &CapturedTakeOrdersRequest,
    ) -> TakeOrdersRequest {
        captured_request.lock().unwrap().clone().unwrap()
    }

    fn no_take_orders_request_was_made(captured_request: &CapturedTakeOrdersRequest) {
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
    async fn test_handle_swap_calldata_captures_v1_analytics() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(approval_response()),
        };
        let recording = RecordingSink::new();
        let analytics = Analytics::new(Arc::new(recording.clone()));
        let key = test_key();
        let attribution_state = test_attribution_state();
        let attribution = attribution_state.for_api_key(&key.key_id, TAKER);

        handle_swap_calldata(
            &ds,
            &key,
            &analytics,
            &attribution_state.signer,
            &attribution,
            calldata_request("100", "2.5"),
        )
        .await
        .expect("successful calldata");

        let event = recording
            .events()
            .into_iter()
            .find(|event| event.event == "swap_calldata_generated")
            .expect("swap_calldata_generated event");
        assert_eq!(event.distinct_id, TAKER.to_string().to_lowercase());
        assert_eq!(event.properties["api_version"], "v1");
        assert!(event.properties.get("mode").is_none());
    }

    #[rocket::async_test]
    async fn test_handle_swap_calldata_captures_v2_analytics() {
        let ds = MockSwapDataSource {
            supported_tokens: Ok(()),
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(approval_response()),
        };
        let recording = RecordingSink::new();
        let analytics = Analytics::new(Arc::new(recording.clone()));
        let key = test_key();
        let attribution_state = test_attribution_state();
        let attribution = attribution_state.for_api_key(&key.key_id, TAKER);

        handle_swap_calldata_v2(
            &ds,
            &key,
            &analytics,
            &attribution_state.signer,
            &attribution,
            calldata_v2_request(SwapCalldataMode::SpendExact, "100", "2.5"),
        )
        .await
        .expect("successful calldata");

        let event = recording
            .events()
            .into_iter()
            .find(|event| event.event == "swap_calldata_generated")
            .expect("swap_calldata_generated event");
        assert_eq!(event.properties["api_version"], "v2");
        assert_eq!(event.properties["mode"], "spendExact");
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
    async fn test_approval_response_does_not_require_executable_attribution() {
        let state = test_attribution_state();
        let attribution = state.for_api_key("customer-key", TAKER);
        let mut response = approval_response();

        embed_and_validate_attribution(&mut response, &state.signer, &attribution)
            .await
            .unwrap();
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
        assert_eq!(result.calldata.estimated_input, "150");
        assert_eq!(result.calldata.denomination, SwapDenomination::Wrapped);
        assert_eq!(result.resolved_price_cap, "2.5");
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
        assert_eq!(result.calldata.denomination, SwapDenomination::Wrapped);
        assert_eq!(result.resolved_price_cap, "3");
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
        assert_eq!(result.calldata.denomination, SwapDenomination::Wrapped);
        assert_eq!(result.resolved_price_cap, "2");
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_resolves_optional_slippage() {
        let (ds, captured_request, captured_counterparty) = capture_slippage_ds(HashMap::new());
        let result = process_swap_calldata_v2(
            &ds,
            slippage_v2_request(SwapCalldataMode::SpendExact, "100", 50),
        )
        .await
        .unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.price_cap, "2.01");
        assert_eq!(result.resolved_price_cap, "2.01");
        assert_eq!(*captured_counterparty.lock().unwrap(), Some(TAKER));
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_applies_reference_price_guard() {
        let (ds, captured_request, _) = capture_slippage_ds(HashMap::new());
        let mut request = slippage_v2_request(SwapCalldataMode::SpendExact, "100", 50);
        request.reference_io_ratio = Some("1".to_string());

        let result = process_swap_calldata_v2(&ds, request).await;

        assert!(
            matches!(result, Err(ApiError::NotFound(message)) if message == "no liquidity found for this pair")
        );
        no_take_orders_request_was_made(&captured_request);
    }

    #[test]
    fn test_swap_calldata_v2_response_flattens_calldata_fields() {
        let response = serde_json::to_value(SwapCalldataV2Response {
            calldata: ready_response(),
            resolved_price_cap: "2.01".to_string(),
        })
        .unwrap();

        assert_eq!(response["to"], ORDERBOOK.to_string().to_lowercase());
        assert_eq!(response["estimatedInput"], "150");
        assert_eq!(response["resolvedPriceCap"], "2.01");
        assert!(response.get("calldata").is_none());
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_denormalizes_resolved_slippage_cap() {
        let (ds, captured_request, _) =
            capture_slippage_ds(HashMap::from([(WT_COIN, wrap_ratio(WT_COIN, "4"))]));
        let mut request = slippage_v2_request(SwapCalldataMode::BuyUpTo, "100", 50);
        request.output_token = WT_COIN;
        request.denomination = SwapDenomination::Unwrapped;

        let result = process_swap_calldata_v2(&ds, request).await.unwrap();
        let request = captured_take_orders_request(&captured_request);

        assert_eq!(request.amount, "25");
        assert_eq!(request.price_cap, "2.01");
        assert_eq!(result.resolved_price_cap, "0.5025");
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_requires_exactly_one_price_limit() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let mut request = calldata_v2_request(SwapCalldataMode::SpendExact, "100", "2.5");
        request.slippage_bps = Some(50);
        let result = process_swap_calldata_v2(&ds, request).await;

        assert!(
            matches!(result, Err(ApiError::BadRequest(message)) if message == "provide exactly one of price_cap or slippage_bps")
        );
        no_take_orders_request_was_made(&captured_request);

        let mut request = slippage_v2_request(SwapCalldataMode::SpendExact, "100", 50);
        request.slippage_bps = None;
        let result = process_swap_calldata_v2(&ds, request).await;

        assert!(
            matches!(result, Err(ApiError::BadRequest(message)) if message == "provide exactly one of price_cap or slippage_bps")
        );
        no_take_orders_request_was_made(&captured_request);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_rejects_reference_ratio_without_slippage() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let mut request = calldata_v2_request(SwapCalldataMode::SpendExact, "100", "2.5");
        request.reference_io_ratio = Some("2".to_string());

        let result = process_swap_calldata_v2(&ds, request).await;

        assert!(
            matches!(result, Err(ApiError::BadRequest(message)) if message == "reference_io_ratio requires slippage_bps")
        );
        no_take_orders_request_was_made(&captured_request);
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_v2_rejects_slippage_out_of_range() {
        let (ds, captured_request) = capture_ds(ready_response(), HashMap::new());
        let result = process_swap_calldata_v2(
            &ds,
            slippage_v2_request(SwapCalldataMode::SpendExact, "100", 5001),
        )
        .await;

        assert!(
            matches!(result, Err(ApiError::BadRequest(message)) if message == "slippage_bps must be between 1 and 5000")
        );
        no_take_orders_request_was_made(&captured_request);
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
        assert_eq!(result.calldata.estimated_input, "300");
        assert_eq!(result.calldata.denomination, SwapDenomination::Unwrapped);
        assert_eq!(result.resolved_price_cap, "2.5");
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
        assert_eq!(result.calldata.estimated_input, "150");
        assert_eq!(result.calldata.denomination, SwapDenomination::Unwrapped);
        assert_eq!(result.resolved_price_cap, "2.5");
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
