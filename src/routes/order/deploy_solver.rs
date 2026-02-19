use super::helpers::map_deployment_to_response;
use super::{OrderDeployer, RealOrderDeployer};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::order::{DeployOrderResponse, DeploySolverOrderRequest};
use rain_orderbook_app_settings::order::VaultType;
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
            .run_with_registry(move |registry| async move {
                let gui = registry
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
                let mut gui = RealOrderDeployer { gui };
                process_deploy_solver(&mut gui, req).await
            })
            .await
            .map_err(ApiError::from)??;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

pub(crate) async fn process_deploy_solver(
    gui: &mut dyn OrderDeployer,
    req: DeploySolverOrderRequest,
) -> Result<DeployOrderResponse, ApiError> {
    gui.set_select_token(INPUT_TOKEN_KEY.to_string(), req.input_token.to_string())
        .await?;

    gui.set_select_token(OUTPUT_TOKEN_KEY.to_string(), req.output_token.to_string())
        .await?;

    gui.set_field_value(FIELD_AMOUNT.to_string(), req.amount.clone())?;

    gui.set_field_value(FIELD_IO_RATIO.to_string(), req.io_ratio)?;

    gui.set_deposit(DEPOSIT_TOKEN_KEY.to_string(), req.amount)
        .await?;

    if let Some(vault_id) = req.input_vault_id {
        gui.set_vault_id(
            VaultType::Input,
            INPUT_TOKEN_KEY.to_string(),
            Some(vault_id.to_string()),
        )?;
    }

    if let Some(vault_id) = req.output_vault_id {
        gui.set_vault_id(
            VaultType::Output,
            OUTPUT_TOKEN_KEY.to_string(),
            Some(vault_id.to_string()),
        )?;
    }

    let args = gui
        .get_deployment_transaction_args(req.owner.to_string())
        .await?;

    map_deployment_to_response(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::{
        mock_deployment_args, mock_deployment_args_with_approval, MockOrderDeployer, MOCK_ORDERBOOK,
    };
    use crate::test_helpers::{
        basic_auth_header, mock_invalid_raindex_config, seed_api_key, TestClientBuilder,
    };
    use alloy::primitives::{Address, U256};
    use rocket::http::{ContentType, Header, Status};

    fn solver_request(
        input_vault_id: Option<U256>,
        output_vault_id: Option<U256>,
    ) -> DeploySolverOrderRequest {
        DeploySolverOrderRequest {
            owner: Address::left_padding_from(&[1]),
            input_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
                .parse()
                .unwrap(),
            output_token: "0x4200000000000000000000000000000000000006"
                .parse()
                .unwrap(),
            amount: "1000000".to_string(),
            io_ratio: "0.0005".to_string(),
            input_vault_id,
            output_vault_id,
        }
    }

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

    #[rocket::async_test]
    async fn test_deploy_solver_success() {
        let args = mock_deployment_args();
        let mut gui = MockOrderDeployer::success(args);
        let req = solver_request(None, None);
        let response = process_deploy_solver(&mut gui, req).await.unwrap();
        assert_eq!(response.to, MOCK_ORDERBOOK);
        assert_eq!(
            response.data,
            alloy::primitives::Bytes::from(vec![0x01, 0x02, 0x03])
        );
        assert_eq!(response.value, U256::ZERO);
        assert!(response.approvals.is_empty());
    }

    #[rocket::async_test]
    async fn test_deploy_solver_success_with_approvals() {
        let args = mock_deployment_args_with_approval();
        let mut gui = MockOrderDeployer::success(args);
        let req = solver_request(None, None);
        let response = process_deploy_solver(&mut gui, req).await.unwrap();
        assert_eq!(response.approvals.len(), 1);
        let approval = &response.approvals[0];
        assert_eq!(approval.symbol, "USDC");
        assert_eq!(approval.amount, "1000000");
        assert_eq!(approval.spender, MOCK_ORDERBOOK);
    }

    #[rocket::async_test]
    async fn test_deploy_solver_success_with_vault_ids() {
        let args = mock_deployment_args();
        let mut gui = MockOrderDeployer::success(args);
        let req = solver_request(Some(U256::from(1)), Some(U256::from(2)));
        let response = process_deploy_solver(&mut gui, req).await.unwrap();
        assert_eq!(response.to, MOCK_ORDERBOOK);
        assert!(response.approvals.is_empty());
    }

    #[rocket::async_test]
    async fn test_deploy_solver_deployment_args_fails() {
        let mut gui = MockOrderDeployer {
            deployment_args_result: Err(ApiError::Internal(
                "failed to build deployment transaction".into(),
            )),
            ..MockOrderDeployer::success(mock_deployment_args())
        };
        let req = solver_request(None, None);
        let result = process_deploy_solver(&mut gui, req).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }
}
