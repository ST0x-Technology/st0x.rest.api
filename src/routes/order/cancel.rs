use super::{OrderDataSource, RaindexOrderDataSource};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::order::{
    CancelOrderRequest, CancelOrderResponse, CancelSummary, CancelTransaction, TokenReturn,
};
use alloy::primitives::{B256, U256};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    post,
    path = "/v1/order/cancel",
    tag = "Order",
    security(("basicAuth" = [])),
    request_body = CancelOrderRequest,
    responses(
        (status = 200, description = "Cancel order result", body = CancelOrderResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 404, description = "Order not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/cancel", data = "<request>")]
pub async fn post_order_cancel(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    request: Json<CancelOrderRequest>,
) -> Result<Json<CancelOrderResponse>, ApiError> {
    let req = request.into_inner();
    async move {
        tracing::info!(body = ?req, "request received");
        let hash: B256 = req.order_hash;
        let response = raindex
            .run_with_client(move |client| async move {
                let ds = RaindexOrderDataSource { client: &client };
                process_cancel_order(&ds, hash).await
            })
            .await
            .map_err(ApiError::from)??;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

async fn process_cancel_order(
    ds: &dyn OrderDataSource,
    hash: B256,
) -> Result<CancelOrderResponse, ApiError> {
    let orders = ds.get_orders_by_hash(hash).await?;
    let order = orders
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::NotFound("order not found".into()))?;

    let calldata = ds.get_remove_calldata(&order).await?;

    let tx = CancelTransaction {
        to: order.orderbook(),
        data: calldata,
        value: U256::ZERO,
    };

    let inputs = order.inputs_list().items();
    let outputs = order.outputs_list().items();

    let mut vaults_to_withdraw: u32 = 0;
    let mut tokens_returned = Vec::new();

    for vault in inputs.iter().chain(outputs.iter()) {
        let balance_str = vault.formatted_balance();
        let balance: f64 = balance_str.parse().unwrap_or(0.0);
        if balance > 0.0 {
            vaults_to_withdraw += 1;
            let token_info = vault.token();
            tokens_returned.push(TokenReturn {
                token: token_info.address(),
                symbol: token_info.symbol().unwrap_or_default(),
                amount: balance_str,
            });
        }
    }

    let summary = CancelSummary {
        vaults_to_withdraw,
        tokens_returned,
    };

    Ok(CancelOrderResponse {
        transactions: vec![tx],
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::*;
    use crate::test_helpers::{
        basic_auth_header, mock_invalid_raindex_config, seed_api_key, TestClientBuilder,
    };
    use alloy::primitives::{Address, Bytes};
    use rocket::http::{ContentType, Header, Status};

    fn mock_calldata() -> Bytes {
        Bytes::from(vec![0xab, 0xcd, 0xef])
    }

    #[rocket::async_test]
    async fn test_cancel_order_success() {
        let ds = MockOrderDataSource {
            orders: Ok(vec![mock_order()]),
            trades: vec![],
            quotes: vec![],
            calldata: Ok(mock_calldata()),
        };
        let result = process_cancel_order(&ds, test_hash()).await.unwrap();

        assert_eq!(result.transactions.len(), 1);
        let tx = &result.transactions[0];
        assert_eq!(
            tx.to,
            "0xd2938e7c9fe3597f78832ce780feb61945c377d7"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(tx.data, mock_calldata());
        assert_eq!(tx.value, U256::ZERO);

        assert_eq!(result.summary.vaults_to_withdraw, 2);
        assert_eq!(result.summary.tokens_returned.len(), 2);

        assert_eq!(
            result.summary.tokens_returned[0].token,
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(result.summary.tokens_returned[0].symbol, "USDC");
        assert_eq!(result.summary.tokens_returned[0].amount, "1.000000");

        assert_eq!(
            result.summary.tokens_returned[1].token,
            "0x4200000000000000000000000000000000000006"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(result.summary.tokens_returned[1].symbol, "WETH");
        assert_eq!(
            result.summary.tokens_returned[1].amount,
            "0.500000000000000000"
        );
    }

    #[rocket::async_test]
    async fn test_cancel_order_not_found() {
        let ds = MockOrderDataSource {
            orders: Ok(vec![]),
            trades: vec![],
            quotes: vec![],
            calldata: Ok(mock_calldata()),
        };
        let result = process_cancel_order(&ds, test_hash()).await;
        assert!(matches!(result, Err(ApiError::NotFound(_))));
    }

    #[rocket::async_test]
    async fn test_cancel_order_calldata_error() {
        let ds = MockOrderDataSource {
            orders: Ok(vec![mock_order()]),
            trades: vec![],
            quotes: vec![],
            calldata: Err(ApiError::Internal("failed".into())),
        };
        let result = process_cancel_order(&ds, test_hash()).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_cancel_order_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .post("/v1/order/cancel")
            .header(ContentType::JSON)
            .body(r#"{"orderHash":"0x000000000000000000000000000000000000000000000000000000000000abcd"}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_cancel_order_500_when_client_init_fails() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .post("/v1/order/cancel")
            .header(Header::new("Authorization", header))
            .header(ContentType::JSON)
            .body(r#"{"orderHash":"0x000000000000000000000000000000000000000000000000000000000000abcd"}"#)
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
