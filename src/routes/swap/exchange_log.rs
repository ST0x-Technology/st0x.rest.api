use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use alloy::primitives::Address;
use serde::Serialize;
use std::time::Instant;

/// A query-friendly record of one typed swap request and the exact response body.
///
/// This deliberately accepts typed payloads rather than Rocket's raw request so
/// headers such as `Authorization` can never enter the exchange log.
pub(super) struct SwapExchangeLog {
    request_id: String,
    endpoint: &'static str,
    api_version: &'static str,
    operation: &'static str,
    api_client_key_id: String,
    api_client_label: String,
    api_client_owner: String,
    input_token: String,
    output_token: String,
    request: String,
    started_at: Instant,
}

impl SwapExchangeLog {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new<T: Serialize>(
        request_id: &str,
        endpoint: &'static str,
        api_version: &'static str,
        operation: &'static str,
        key: &AuthenticatedKey,
        input_token: Address,
        output_token: Address,
        request: &T,
    ) -> Self {
        Self {
            request_id: request_id.to_string(),
            endpoint,
            api_version,
            operation,
            api_client_key_id: key.key_id.clone(),
            api_client_label: key.label.clone(),
            api_client_owner: key.owner.clone(),
            input_token: format!("{input_token:#x}"),
            output_token: format!("{output_token:#x}"),
            request: serialize_payload("request", request),
            started_at: Instant::now(),
        }
    }

    pub(super) fn record<T: Serialize>(&self, result: &Result<T, ApiError>) {
        let (outcome, status_code, response) = match result {
            Ok(response) => ("success", 200, serialize_payload("response", response)),
            Err(error) => {
                let status_code = error.code().status().code;
                let response = ApiErrorResponse::from_error(self.request_id.clone(), error);
                (
                    "error",
                    status_code,
                    serialize_payload("error response", &response),
                )
            }
        };
        let duration_ms = self.started_at.elapsed().as_secs_f64() * 1000.0;

        tracing::info!(
            event = "swap_http_exchange",
            request_id = %self.request_id,
            endpoint = self.endpoint,
            api_version = self.api_version,
            operation = self.operation,
            outcome,
            status_code,
            duration_ms,
            api_client_key_id = %self.api_client_key_id,
            api_client_label = %self.api_client_label,
            api_client_owner = %self.api_client_owner,
            input_token = %self.input_token,
            output_token = %self.output_token,
            request = %self.request,
            response = %response,
            "swap HTTP exchange completed"
        );
    }
}

fn serialize_payload<T: Serialize>(kind: &'static str, payload: &T) -> String {
    match serde_json::to_string(payload) {
        Ok(json) => json,
        Err(error) => {
            tracing::error!(
                %error,
                payload_kind = kind,
                "failed to serialize swap HTTP exchange payload"
            );
            serde_json::json!({ "serializationError": error.to_string() }).to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ApiError, ApiErrorCode};
    use crate::types::swap::{
        SwapCalldataRequest, SwapCalldataResponse, SwapDenomination, SwapQuoteRequest,
        SwapQuoteResponse,
    };
    use alloy::primitives::{address, bytes, U256};
    use tracing_test::traced_test;

    const INPUT_TOKEN: Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    const OUTPUT_TOKEN: Address = address!("4200000000000000000000000000000000000006");
    const TAKER: Address = address!("1234567890abcdef1234567890abcdef12345678");

    fn key() -> AuthenticatedKey {
        AuthenticatedKey {
            id: 7,
            key_id: "site-key".to_string(),
            label: "ST0x Website".to_string(),
            owner: "st0x".to_string(),
            is_admin: false,
        }
    }

    #[test]
    #[traced_test]
    fn records_complete_quote_request_and_response() {
        let request = SwapQuoteRequest {
            input_token: INPUT_TOKEN,
            output_token: OUTPUT_TOKEN,
            output_amount: "0.5".to_string(),
            denomination: SwapDenomination::Wrapped,
        };
        let response = SwapQuoteResponse {
            input_token: INPUT_TOKEN,
            output_token: OUTPUT_TOKEN,
            output_amount: "0.5".to_string(),
            denomination: SwapDenomination::Wrapped,
            estimated_output: "0.5".to_string(),
            estimated_input: "1250.75".to_string(),
            estimated_io_ratio: "2501.5".to_string(),
        };
        let exchange = SwapExchangeLog::new(
            "request-123",
            "/v1/swap/quote",
            "v1",
            "quote",
            &key(),
            INPUT_TOKEN,
            OUTPUT_TOKEN,
            &request,
        );

        exchange.record(&Ok(response));

        assert!(logs_contain("swap_http_exchange"));
        assert!(logs_contain("request-123"));
        assert!(logs_contain("site-key"));
        assert!(logs_contain("outputAmount"));
        assert!(logs_contain("estimatedInput"));
        assert!(logs_contain("1250.75"));
        assert!(!logs_contain("Authorization"));
    }

    #[test]
    #[traced_test]
    fn records_complete_calldata_response() {
        let request = SwapCalldataRequest {
            taker: TAKER,
            input_token: INPUT_TOKEN,
            output_token: OUTPUT_TOKEN,
            output_amount: "0.5".to_string(),
            maximum_io_ratio: "2501.5".to_string(),
            denomination: SwapDenomination::Wrapped,
        };
        let response = SwapCalldataResponse {
            to: OUTPUT_TOKEN,
            data: bytes!("abcdef"),
            value: U256::ZERO,
            estimated_input: "1250.75".to_string(),
            denomination: SwapDenomination::Wrapped,
            approvals: vec![],
        };
        let exchange = SwapExchangeLog::new(
            "request-calldata",
            "/v2/swap/calldata",
            "v2",
            "calldata",
            &key(),
            INPUT_TOKEN,
            OUTPUT_TOKEN,
            &request,
        );

        exchange.record(&Ok(response));

        assert!(logs_contain("request-calldata"));
        assert!(logs_contain("maximumIoRatio"));
        assert!(logs_contain("0xabcdef"));
        assert!(logs_contain("approvals"));
    }

    #[test]
    #[traced_test]
    fn records_the_exact_public_error_response() {
        let request = SwapQuoteRequest {
            input_token: INPUT_TOKEN,
            output_token: OUTPUT_TOKEN,
            output_amount: "0.5".to_string(),
            denomination: SwapDenomination::Wrapped,
        };
        let exchange = SwapExchangeLog::new(
            "request-error",
            "/v1/swap/quote",
            "v1",
            "quote",
            &key(),
            INPUT_TOKEN,
            OUTPUT_TOKEN,
            &request,
        );

        exchange.record::<SwapQuoteResponse>(&Err(ApiError::coded(
            ApiErrorCode::SwapNoLiquidity,
            "no executable liquidity found",
        )));

        assert!(logs_contain("request-error"));
        assert!(logs_contain("SWAP_NO_LIQUIDITY"));
        assert!(logs_contain("no executable liquidity found"));
        assert!(logs_contain("404"));
    }
}
