use super::{RaindexSwapDataSource, SwapDataSource};
use crate::auth::AuthenticatedKey;
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::swap::{SwapCalldataRequest, SwapCalldataResponse};
use alloy::hex::encode_prefixed;
use alloy::primitives::keccak256;
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
    key: AuthenticatedKey,
    pool: &State<DbPool>,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    span: TracingSpan,
    request: Json<SwapCalldataRequest>,
) -> Result<Json<SwapCalldataResponse>, ApiError> {
    let req = request.into_inner();
    let pool = pool.inner().clone();
    async move {
        tracing::info!(api_key_id = key.id, key_id = %key.key_id, body = ?req, "request received");
        let raindex = shared_raindex.read().await;
        let ds = RaindexSwapDataSource {
            client: raindex.client(),
        };
        let response = handle_swap_calldata(&ds, &pool, &key, req).await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn process_swap_calldata(
    ds: &dyn SwapDataSource,
    req: &SwapCalldataRequest,
) -> Result<SwapCalldataResponse, ApiError> {
    let take_req = TakeOrdersRequest {
        taker: req.taker.to_string(),
        chain_id: crate::CHAIN_ID,
        sell_token: req.input_token.to_string(),
        buy_token: req.output_token.to_string(),
        mode: TakeOrdersMode::BuyUpTo,
        amount: req.output_amount.clone(),
        price_cap: req.maximum_io_ratio.clone(),
    };

    ds.get_calldata(take_req).await
}

async fn handle_swap_calldata(
    ds: &dyn SwapDataSource,
    pool: &DbPool,
    key: &AuthenticatedKey,
    req: SwapCalldataRequest,
) -> Result<SwapCalldataResponse, ApiError> {
    let response = process_swap_calldata(ds, &req).await?;
    if let Err(error) = persist_issued_swap_calldata(pool, key, &req, &response).await {
        tracing::warn!(
            error = %error,
            api_key_id = key.id,
            key_id = %key.key_id,
            taker = %req.taker,
            to = %response.to,
            "continuing after issued swap calldata persistence failure"
        );
    }
    Ok(response)
}

async fn persist_issued_swap_calldata(
    pool: &DbPool,
    key: &AuthenticatedKey,
    request: &SwapCalldataRequest,
    response: &SwapCalldataResponse,
) -> Result<(), sqlx::Error> {
    if !should_persist_issued_swap_calldata(response) {
        return Ok(());
    }

    let record = crate::db::issued_swap_calldata::NewIssuedSwapCalldata {
        api_key_id: key.id,
        key_id: key.key_id.clone(),
        label: key.label.clone(),
        owner: key.owner.clone(),
        chain_id: crate::CHAIN_ID as i64,
        taker: format!("{:#x}", request.taker),
        to_address: format!("{:#x}", response.to),
        tx_value: response.value.to_string(),
        calldata: encode_prefixed(response.data.as_ref()),
        calldata_hash: encode_prefixed(keccak256(response.data.as_ref())),
        input_token: format!("{:#x}", request.input_token),
        output_token: format!("{:#x}", request.output_token),
        output_amount: request.output_amount.clone(),
        maximum_io_ratio: request.maximum_io_ratio.clone(),
        estimated_input: response.estimated_input.clone(),
    };

    crate::db::issued_swap_calldata::insert(pool, &record).await?;

    tracing::info!(
        api_key_id = key.id,
        key_id = %key.key_id,
        taker = %request.taker,
        to = %response.to,
        "persisted issued swap calldata"
    );

    Ok(())
}

fn should_persist_issued_swap_calldata(response: &SwapCalldataResponse) -> bool {
    !response.data.is_empty() && response.approvals.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::swap::test_fixtures::MockSwapDataSource;
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use crate::types::common::Approval;
    use alloy::hex::encode_prefixed;
    use alloy::primitives::{address, keccak256, Address, Bytes, U256};
    use rocket::http::{ContentType, Header, Status};
    use sqlx::FromRow;

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
        }
    }

    fn ready_response() -> SwapCalldataResponse {
        SwapCalldataResponse {
            to: ORDERBOOK,
            data: Bytes::from(vec![0xab, 0xcd, 0xef]),
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
                approval_data: Bytes::from(vec![0x09, 0x5e, 0xa7, 0xb3]),
            }],
        }
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_ready() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(ready_response()),
        };
        let request = calldata_request("100", "2.5");
        let result = process_swap_calldata(&ds, &request).await.unwrap();

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
        let request = calldata_request("100", "2.5");
        let result = process_swap_calldata(&ds, &request).await.unwrap();

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
        let request = calldata_request("100", "2.5");
        let result = process_swap_calldata(&ds, &request).await;
        assert!(matches!(result, Err(ApiError::NotFound(msg)) if msg.contains("no liquidity")));
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_bad_request() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Err(ApiError::BadRequest("invalid parameters".into())),
        };
        let request = calldata_request("not-a-number", "2.5");
        let result = process_swap_calldata(&ds, &request).await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[rocket::async_test]
    async fn test_process_swap_calldata_internal_error() {
        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Err(ApiError::Internal("failed to generate calldata".into())),
        };
        let request = calldata_request("100", "2.5");
        let result = process_swap_calldata(&ds, &request).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[derive(Debug, FromRow)]
    struct IssuedSwapCalldataRow {
        api_key_id: i64,
        key_id: String,
        label: String,
        owner: String,
        chain_id: i64,
        taker: String,
        to_address: String,
        tx_value: String,
        calldata: String,
        calldata_hash: String,
        input_token: String,
        output_token: String,
        output_amount: String,
        maximum_io_ratio: String,
        estimated_input: String,
    }

    #[derive(Debug, FromRow)]
    struct AuthenticatedKeyRow {
        id: i64,
        key_id: String,
        label: String,
        owner: String,
        is_admin: bool,
    }

    async fn fetch_issued_rows(pool: &DbPool) -> Vec<IssuedSwapCalldataRow> {
        sqlx::query_as::<_, IssuedSwapCalldataRow>(
            "SELECT
                api_key_id,
                key_id,
                label,
                owner,
                chain_id,
                taker,
                to_address,
                tx_value,
                calldata,
                calldata_hash,
                input_token,
                output_token,
                output_amount,
                maximum_io_ratio,
                estimated_input
             FROM issued_swap_calldata
             ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .expect("query issued swap calldata")
    }

    async fn fetch_authenticated_key(pool: &DbPool, key_id: &str) -> AuthenticatedKey {
        let row = sqlx::query_as::<_, AuthenticatedKeyRow>(
            "SELECT id, key_id, label, owner, is_admin FROM api_keys WHERE key_id = ?",
        )
        .bind(key_id)
        .fetch_one(pool)
        .await
        .expect("authenticated key");

        AuthenticatedKey {
            id: row.id,
            key_id: row.key_id,
            label: row.label,
            owner: row.owner,
            is_admin: row.is_admin,
        }
    }

    async fn issued_row_count(pool: &DbPool) -> i64 {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM issued_swap_calldata")
            .fetch_one(pool)
            .await
            .expect("query issued swap calldata count");
        row.0
    }

    #[rocket::async_test]
    async fn test_handle_swap_calldata_persists_executable_payload() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, _secret) = seed_api_key(&client).await;
        let pool = client.rocket().state::<DbPool>().expect("pool");
        let key = fetch_authenticated_key(pool, &key_id).await;

        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(ready_response()),
        };
        let request = calldata_request("100", "2.5");

        let response = handle_swap_calldata(&ds, pool, &key, request.clone())
            .await
            .expect("successful swap calldata");

        assert_eq!(response.to, ORDERBOOK);
        assert_eq!(response.data, ready_response().data);
        assert_eq!(response.value, U256::ZERO);
        assert_eq!(response.estimated_input, "150");
        assert!(response.approvals.is_empty());

        let rows = fetch_issued_rows(pool).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.api_key_id, key.id);
        assert_eq!(row.key_id, key.key_id.as_str());
        assert_eq!(row.label, key.label.as_str());
        assert_eq!(row.owner, key.owner.as_str());
        assert_eq!(row.chain_id, crate::CHAIN_ID as i64);
        assert_eq!(row.taker, format!("{:#x}", request.taker));
        assert_eq!(row.to_address, format!("{:#x}", ORDERBOOK));
        assert_eq!(row.tx_value, U256::ZERO.to_string());
        assert_eq!(row.calldata, encode_prefixed(response.data.as_ref()));
        assert_eq!(
            row.calldata_hash,
            encode_prefixed(keccak256(response.data.as_ref()))
        );
        assert_eq!(row.input_token, format!("{:#x}", request.input_token));
        assert_eq!(row.output_token, format!("{:#x}", request.output_token));
        assert_eq!(row.output_amount, request.output_amount);
        assert_eq!(row.maximum_io_ratio, request.maximum_io_ratio);
        assert_eq!(row.estimated_input, response.estimated_input);
    }

    #[rocket::async_test]
    async fn test_handle_swap_calldata_skips_approval_only_payload() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, _secret) = seed_api_key(&client).await;
        let pool = client.rocket().state::<DbPool>().expect("pool");
        let key = fetch_authenticated_key(pool, &key_id).await;

        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(approval_response()),
        };

        handle_swap_calldata(&ds, pool, &key, calldata_request("100", "2.5"))
            .await
            .expect("successful approval response");

        let rows = fetch_issued_rows(pool).await;
        assert!(rows.is_empty());
    }

    #[rocket::async_test]
    async fn test_handle_swap_calldata_allows_duplicate_payloads() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, _secret) = seed_api_key(&client).await;
        let pool = client.rocket().state::<DbPool>().expect("pool");
        let key = fetch_authenticated_key(pool, &key_id).await;

        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(ready_response()),
        };
        let request = calldata_request("100", "2.5");

        handle_swap_calldata(&ds, pool, &key, request.clone())
            .await
            .expect("first swap calldata");
        handle_swap_calldata(&ds, pool, &key, request)
            .await
            .expect("second swap calldata");

        let rows = fetch_issued_rows(pool).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].calldata_hash, rows[1].calldata_hash);
    }

    #[rocket::async_test]
    async fn test_issued_swap_calldata_survives_api_key_deletion() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, _secret) = seed_api_key(&client).await;
        let pool = client.rocket().state::<DbPool>().expect("pool");
        let key = fetch_authenticated_key(pool, &key_id).await;

        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(ready_response()),
        };

        handle_swap_calldata(&ds, pool, &key, calldata_request("100", "2.5"))
            .await
            .expect("successful swap calldata");

        sqlx::query("DELETE FROM api_keys WHERE key_id = ?")
            .bind(&key_id)
            .execute(pool)
            .await
            .expect("delete api key");

        let rows = fetch_issued_rows(pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key_id, key_id);
        assert_eq!(rows[0].label, "test-key");
        assert_eq!(rows[0].owner, "test-owner");
    }

    #[test]
    fn test_should_persist_issued_swap_calldata() {
        assert!(should_persist_issued_swap_calldata(&ready_response()));
        assert!(!should_persist_issued_swap_calldata(&approval_response()));
    }

    #[rocket::async_test]
    async fn test_handle_swap_calldata_returns_response_if_persistence_fails() {
        let client = TestClientBuilder::new().build().await;
        let pool = client.rocket().state::<DbPool>().expect("pool");

        sqlx::query("DROP TABLE issued_swap_calldata")
            .execute(pool)
            .await
            .expect("drop issued swap calldata table");

        let ds = MockSwapDataSource {
            orders: Ok(vec![]),
            candidates: vec![],
            calldata_result: Ok(ready_response()),
        };

        let key = AuthenticatedKey {
            id: i64::MAX,
            key_id: "missing".to_string(),
            label: "missing".to_string(),
            owner: "missing".to_string(),
            is_admin: false,
        };

        let result = handle_swap_calldata(&ds, pool, &key, calldata_request("100", "2.5"))
            .await
            .expect("swap calldata should still succeed");
        assert_eq!(result.to, ORDERBOOK);
        assert_eq!(result.data, ready_response().data);
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
    async fn test_unauthenticated_swap_calldata_creates_no_issued_row() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/swap/calldata")
            .header(ContentType::JSON)
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"100","maximumIoRatio":"2.5"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);

        let pool = client.rocket().state::<DbPool>().expect("pool");
        assert_eq!(issued_row_count(pool).await, 0);
    }

    #[rocket::async_test]
    async fn test_failed_authenticated_swap_calldata_creates_no_issued_row() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = Header::new("Authorization", basic_auth_header(&key_id, &secret));

        let response = client
            .post("/v1/swap/calldata")
            .header(ContentType::JSON)
            .header(header)
            .body(r#"{"taker":"0x1111111111111111111111111111111111111111","inputToken":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","outputToken":"0x4200000000000000000000000000000000000006","outputAmount":"not-a-number","maximumIoRatio":"2.5"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::BadRequest);

        let pool = client.rocket().state::<DbPool>().expect("pool");
        assert_eq!(issued_row_count(pool).await, 0);
    }
}
