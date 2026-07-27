//! Event builders and shared attribution helpers.
//!
//! Every event carries `api_client_{key_id,label,owner}` so volume and users can be
//! sliced by client (site vs. integrator). "Site vs. integrator" is defined in
//! PostHog by filtering on `api_client_owner` rather than hardcoded here.

use super::AnalyticsEvent;
use crate::auth::{AuthClientInfo, AuthenticatedKey};
use crate::types::swap::{SwapCalldataResponse, SwapQuoteResponse, SwapQuoteV2Response};
use alloy::primitives::Address;
use serde_json::{Map, Value};

/// Which swap-calldata API version produced an event.
pub(crate) enum ApiVersion {
    V1,
    V2,
}

impl ApiVersion {
    fn as_str(&self) -> &'static str {
        match self {
            ApiVersion::V1 => "v1",
            ApiVersion::V2 => "v2",
        }
    }
}

/// Client-attribution properties present on every API analytics event.
fn base_props(info: &AuthClientInfo) -> Map<String, Value> {
    let mut props = Map::new();
    props.insert("api_client_key_id".to_string(), info.key_id.clone().into());
    props.insert("api_client_label".to_string(), info.label.clone().into());
    props.insert("api_client_owner".to_string(), info.owner.clone().into());
    props
}

/// `distinct_id` for events attributed to the calling client rather than an end user.
fn client_distinct_id(info: &AuthClientInfo) -> String {
    format!("client:{}", info.key_id)
}

/// Collapse high-cardinality path segments (`0x…` addresses/hashes, numeric ids) into
/// placeholders so `endpoint` stays low-cardinality in analytics.
fn normalize_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.len() >= 10
                && segment
                    .get(..2)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("0x"))
                && segment[2..].chars().all(|c| c.is_ascii_hexdigit())
            {
                "{address}"
            } else if !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()) {
                "{id}"
            } else {
                segment
            }
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Generic per-request usage event, emitted for every authenticated request.
pub(crate) fn api_request_event(
    info: &AuthClientInfo,
    method: &str,
    path: &str,
    status_code: i32,
    latency_ms: f64,
) -> AnalyticsEvent {
    let mut props = base_props(info);
    props.insert("method".to_string(), method.into());
    props.insert("endpoint".to_string(), normalize_path(path).into());
    props.insert("status_code".to_string(), status_code.into());
    props.insert("latency_ms".to_string(), latency_ms.into());
    AnalyticsEvent {
        event: "api_request",
        distinct_id: client_distinct_id(info),
        properties: props,
    }
}

/// Swap quote (trade intent) event. Attributed to the client (quotes carry no wallet).
pub(crate) fn swap_quoted_event(
    key: &AuthenticatedKey,
    input_token: Address,
    output_token: Address,
    denomination: Value,
    resp: &SwapQuoteResponse,
) -> AnalyticsEvent {
    let info = key.client_info();
    let mut props = base_props(&info);
    props.insert("input_token".to_string(), token_str(input_token).into());
    props.insert("output_token".to_string(), token_str(output_token).into());
    props.insert("denomination".to_string(), denomination);
    props.insert(
        "estimated_input".to_string(),
        resp.estimated_input.clone().into(),
    );
    props.insert(
        "estimated_output".to_string(),
        resp.estimated_output.clone().into(),
    );
    props.insert(
        "estimated_io_ratio".to_string(),
        resp.estimated_io_ratio.clone().into(),
    );
    AnalyticsEvent {
        event: "swap_quoted",
        distinct_id: client_distinct_id(&info),
        properties: props,
    }
}

/// Mode-based swap quote event. Attributed to the client, with the optional
/// taker deliberately omitted from analytics because this is still quote intent.
pub(crate) fn swap_quoted_v2_event(
    key: &AuthenticatedKey,
    resp: &SwapQuoteV2Response,
) -> AnalyticsEvent {
    let info = key.client_info();
    let mut props = base_props(&info);
    props.insert(
        "input_token".to_string(),
        token_str(resp.input_token).into(),
    );
    props.insert(
        "output_token".to_string(),
        token_str(resp.output_token).into(),
    );
    props.insert(
        "denomination".to_string(),
        serde_json::to_value(resp.denomination).unwrap_or(Value::Null),
    );
    props.insert(
        "mode".to_string(),
        serde_json::to_value(resp.mode).unwrap_or(Value::Null),
    );
    props.insert(
        "estimated_input".to_string(),
        resp.estimated_input.clone().into(),
    );
    props.insert(
        "estimated_output".to_string(),
        resp.estimated_output.clone().into(),
    );
    props.insert(
        "estimated_io_ratio".to_string(),
        resp.estimated_io_ratio.clone().into(),
    );
    props.insert("fully_filled".to_string(), resp.fully_filled.into());
    props.insert(
        "resolved_price_cap".to_string(),
        resp.resolved_price_cap.clone().into(),
    );
    AnalyticsEvent {
        event: "swap_quoted",
        distinct_id: client_distinct_id(&info),
        properties: props,
    }
}

/// Swap calldata (execution-intent) event — the closest proxy the API has to a real
/// trade, and the one event carrying the end-user wallet (`taker`). `distinct_id` is
/// the lowercased wallet so a trader is one PostHog person across the site and every
/// integrator (cross-venue uniqueness).
#[allow(clippy::too_many_arguments)]
pub(crate) fn swap_calldata_generated_event(
    key: &AuthenticatedKey,
    taker: Address,
    input_token: Address,
    output_token: Address,
    denomination: Value,
    api_version: ApiVersion,
    mode: Option<Value>,
    resp: &SwapCalldataResponse,
) -> AnalyticsEvent {
    let info = key.client_info();
    let taker_id = token_str(taker);
    let mut props = base_props(&info);
    props.insert("taker".to_string(), taker_id.clone().into());
    props.insert("input_token".to_string(), token_str(input_token).into());
    props.insert("output_token".to_string(), token_str(output_token).into());
    props.insert("denomination".to_string(), denomination);
    props.insert(
        "estimated_input".to_string(),
        resp.estimated_input.clone().into(),
    );
    props.insert("value".to_string(), resp.value.to_string().into());
    props.insert("api_version".to_string(), api_version.as_str().into());
    if let Some(mode) = mode {
        props.insert("mode".to_string(), mode);
    }
    AnalyticsEvent {
        event: "swap_calldata_generated",
        distinct_id: taker_id,
        properties: props,
    }
}

/// Lowercased `0x…` address string. Matches the site's `posthog.identify()` form so
/// wallet identities line up across venues.
fn token_str(address: Address) -> String {
    address.to_string().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::swap::SwapDenomination;

    fn test_key() -> AuthenticatedKey {
        AuthenticatedKey {
            id: 1,
            key_id: "site-key".to_string(),
            label: "St0x Website".to_string(),
            owner: "st0x".to_string(),
            is_admin: false,
        }
    }

    fn addr(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    #[test]
    fn normalize_path_collapses_addresses_and_ids() {
        assert_eq!(
            normalize_path("/v1/tokens/0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913/details"),
            "/v1/tokens/{address}/details"
        );
        assert_eq!(normalize_path("/v1/tokens"), "/v1/tokens");
        assert_eq!(normalize_path("/v1/order/12345"), "/v1/order/{id}");
        assert_eq!(normalize_path("/health"), "/health");
    }

    #[test]
    fn api_request_event_carries_client_attribution() {
        let info = test_key().client_info();
        let event = api_request_event(&info, "GET", "/v1/tokens", 200, 4.2);
        assert_eq!(event.event, "api_request");
        assert_eq!(event.distinct_id, "client:site-key");
        assert_eq!(event.properties["method"], Value::from("GET"));
        assert_eq!(event.properties["endpoint"], Value::from("/v1/tokens"));
        assert_eq!(event.properties["status_code"], Value::from(200));
        assert_eq!(event.properties["api_client_owner"], Value::from("st0x"));
    }

    #[test]
    fn swap_quoted_event_is_client_scoped() {
        let resp = SwapQuoteResponse {
            input_token: addr(1),
            output_token: addr(2),
            output_amount: "0.5".to_string(),
            denomination: SwapDenomination::Wrapped,
            estimated_output: "0.5".to_string(),
            estimated_input: "1250.75".to_string(),
            estimated_io_ratio: "2501.5".to_string(),
        };
        let event = swap_quoted_event(&test_key(), addr(1), addr(2), Value::from("wrapped"), &resp);
        assert_eq!(event.event, "swap_quoted");
        assert_eq!(event.distinct_id, "client:site-key");
        assert_eq!(event.properties["estimated_input"], Value::from("1250.75"));
        assert_eq!(
            event.properties["api_client_label"],
            Value::from("St0x Website")
        );
    }

    #[test]
    fn swap_calldata_event_is_keyed_on_lowercased_taker() {
        let resp = SwapCalldataResponse {
            to: addr(9),
            data: Default::default(),
            value: alloy::primitives::U256::from(1000u64),
            estimated_input: "1250.75".to_string(),
            denomination: SwapDenomination::Wrapped,
            approvals: vec![],
        };
        let taker = addr(0xAB);
        let event = swap_calldata_generated_event(
            &test_key(),
            taker,
            addr(1),
            addr(2),
            Value::from("wrapped"),
            ApiVersion::V1,
            None,
            &resp,
        );
        assert_eq!(event.event, "swap_calldata_generated");
        // distinct_id is the lowercased wallet, matching the site's identify() form.
        assert_eq!(event.distinct_id, taker.to_string().to_lowercase());
        assert_eq!(
            event.distinct_id,
            event.properties["taker"].as_str().unwrap()
        );
        assert_eq!(event.properties["value"], Value::from("1000"));
        assert_eq!(event.properties["api_version"], Value::from("v1"));
        assert!(event.properties.get("mode").is_none());
    }
}
