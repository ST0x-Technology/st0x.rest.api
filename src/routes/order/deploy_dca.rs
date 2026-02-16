use super::helpers::map_deployment_to_response;
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::order::{DeployDcaOrderRequest, DeployOrderResponse, PeriodUnit};
use rain_orderbook_app_settings::order::VaultType;
use rain_orderbook_js_api::registry::DotrainRegistry;
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

const ORDER_KEY: &str = "st0x-dca";
const DEPLOYMENT_KEY: &str = "base";
const INPUT_TOKEN_KEY: &str = "input-token";
const OUTPUT_TOKEN_KEY: &str = "output-token";
const DEPOSIT_TOKEN_KEY: &str = "output-token";
const FIELD_BUDGET_AMOUNT: &str = "budget-amount";
const FIELD_PERIOD: &str = "period";
const FIELD_PERIOD_UNIT: &str = "period-unit";
const FIELD_START_IO: &str = "start-io";
const FIELD_FLOOR_IO: &str = "floor-io";

#[utoipa::path(
    post,
    path = "/v1/order/dca",
    tag = "Order",
    security(("basicAuth" = [])),
    request_body = DeployDcaOrderRequest,
    responses(
        (status = 200, description = "DCA order deployment result", body = DeployOrderResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/dca", data = "<request>")]
pub async fn post_order_dca(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    request: Json<DeployDcaOrderRequest>,
) -> Result<Json<DeployOrderResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let response = raindex
            .run_with_registry(
                move |registry| async move { process_deploy_dca(registry, req).await },
            )
            .await
            .map_err(ApiError::from)??;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn process_deploy_dca(
    registry: DotrainRegistry,
    req: DeployDcaOrderRequest,
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

    gui.set_field_value(FIELD_BUDGET_AMOUNT.to_string(), req.budget_amount.clone())
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set budget amount");
            ApiError::BadRequest(format!("invalid budget amount: {e}"))
        })?;

    gui.set_field_value(FIELD_PERIOD.to_string(), req.period.to_string())
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set period");
            ApiError::BadRequest(format!("invalid period: {e}"))
        })?;

    let period_unit_value = match req.period_unit {
        PeriodUnit::Days => "days",
        PeriodUnit::Hours => "hours",
        PeriodUnit::Minutes => "minutes",
    };
    gui.set_field_value(FIELD_PERIOD_UNIT.to_string(), period_unit_value.to_string())
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set period unit");
            ApiError::BadRequest(format!("invalid period unit: {e}"))
        })?;

    gui.set_field_value(FIELD_START_IO.to_string(), req.start_io)
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set start io");
            ApiError::BadRequest(format!("invalid start io: {e}"))
        })?;

    gui.set_field_value(FIELD_FLOOR_IO.to_string(), req.floor_io)
        .map_err(|e| {
            tracing::error!(error = %e, "failed to set floor io");
            ApiError::BadRequest(format!("invalid floor io: {e}"))
        })?;

    gui.set_deposit(DEPOSIT_TOKEN_KEY.to_string(), req.budget_amount)
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
    async fn test_deploy_dca_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/order/dca")
            .header(ContentType::JSON)
            .body(r#"{"owner":"0x0000000000000000000000000000000000000001","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","budgetAmount":"1000000","period":4,"periodUnit":"hours","startIo":"0.0005","floorIo":"0.0003"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_deploy_dca_500_when_registry_fails() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/order/dca")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(r#"{"owner":"0x0000000000000000000000000000000000000001","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","budgetAmount":"1000000","period":4,"periodUnit":"hours","startIo":"0.0005","floorIo":"0.0003"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::InternalServerError);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
    }
}
