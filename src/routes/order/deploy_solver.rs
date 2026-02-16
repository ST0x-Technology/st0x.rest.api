use super::helpers::map_deployment_to_response;
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::order::{DeployOrderResponse, DeploySolverOrderRequest};
use rain_orderbook_app_settings::order::VaultType;
use rain_orderbook_js_api::registry::DotrainRegistry;
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

const ORDER_KEY: &str = "st0x-solver";
const DEPLOYMENT_KEY: &str = "base";
const INPUT_TOKEN_KEY: &str = "input-token";
const OUTPUT_TOKEN_KEY: &str = "output-token";
const DEPOSIT_TOKEN_KEY: &str = "output-token";
const FIELD_AMOUNT: &str = "amount";
const FIELD_IO_RATIO: &str = "io-ratio";

#[utoipa::path(
    post,
    path = "/v1/order/solver",
    tag = "Order",
    security(("basicAuth" = [])),
    request_body = DeploySolverOrderRequest,
    responses(
        (status = 200, description = "Solver order deployment result", body = DeployOrderResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/solver", data = "<request>")]
pub async fn post_order_solver(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    request: Json<DeploySolverOrderRequest>,
) -> Result<Json<DeployOrderResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let response = raindex
            .run_with_registry(
                move |registry| async move { process_deploy_solver(registry, req).await },
            )
            .await
            .map_err(ApiError::from)??;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn process_deploy_solver(
    registry: DotrainRegistry,
    req: DeploySolverOrderRequest,
) -> Result<DeployOrderResponse, ApiError> {
    let mut gui = registry
        .get_gui(
            ORDER_KEY.to_string(),
            DEPLOYMENT_KEY.to_string(),
            None,
            None,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create GUI");
            ApiError::Internal("failed to initialize order configuration".into())
        })?;

    gui.set_select_token(INPUT_TOKEN_KEY.to_string(), req.input_token.to_string())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set input token");
            ApiError::BadRequest(format!("invalid input token: {e}"))
        })?;

    gui.set_select_token(OUTPUT_TOKEN_KEY.to_string(), req.output_token.to_string())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set output token");
            ApiError::BadRequest(format!("invalid output token: {e}"))
        })?;

    gui.set_field_value(FIELD_AMOUNT.to_string(), req.amount.clone())
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set amount");
            ApiError::BadRequest(format!("invalid amount: {e}"))
        })?;

    gui.set_field_value(FIELD_IO_RATIO.to_string(), req.io_ratio)
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set io ratio");
            ApiError::BadRequest(format!("invalid io ratio: {e}"))
        })?;

    gui.set_deposit(DEPOSIT_TOKEN_KEY.to_string(), req.amount)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set deposit");
            ApiError::BadRequest(format!("invalid deposit: {e}"))
        })?;

    if let Some(vault_id) = req.input_vault_id {
        gui.set_vault_id(
            VaultType::Input,
            INPUT_TOKEN_KEY.to_string(),
            Some(vault_id.to_string()),
        )
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set input vault id");
            ApiError::BadRequest(format!("invalid input vault id: {e}"))
        })?;
    }

    if let Some(vault_id) = req.output_vault_id {
        gui.set_vault_id(
            VaultType::Output,
            OUTPUT_TOKEN_KEY.to_string(),
            Some(vault_id.to_string()),
        )
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set output vault id");
            ApiError::BadRequest(format!("invalid output vault id: {e}"))
        })?;
    }

    let args = gui
        .get_deployment_transaction_args(req.owner.to_string())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to get deployment transaction args");
            ApiError::Internal(format!("failed to build deployment transaction: {e}"))
        })?;

    map_deployment_to_response(args)
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{
        basic_auth_header, mock_invalid_raindex_config, seed_api_key, TestClientBuilder,
    };
    use rocket::http::{ContentType, Header, Status};

    #[rocket::async_test]
    async fn test_deploy_solver_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/order/solver")
            .header(ContentType::JSON)
            .body(r#"{"owner":"0x0000000000000000000000000000000000000001","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","amount":"1000000","ioRatio":"0.0005"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_deploy_solver_500_when_registry_fails() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/order/solver")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(r#"{"owner":"0x0000000000000000000000000000000000000001","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","amount":"1000000","ioRatio":"0.0005"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
    }
}
