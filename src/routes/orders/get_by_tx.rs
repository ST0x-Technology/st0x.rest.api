use super::{OrdersListDataSource, RaindexOrdersListDataSource};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::{TokenRef, ValidatedFixedBytes};
use crate::types::orders::{OrderByTxEntry, OrdersByTxResponse};
use alloy::primitives::B256;
use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};
use rocket::serde::json::Json;
use rocket::State;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/v1/orders/tx/{tx_hash}",
    tag = "Orders",
    security(("basicAuth" = [])),
    params(
        ("tx_hash" = String, Path, description = "Transaction hash"),
    ),
    responses(
        (status = 200, description = "Orders from transaction", body = OrdersByTxResponse),
        (status = 202, description = "Transaction not yet indexed", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 404, description = "Transaction not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/tx/<tx_hash>")]
pub async fn get_orders_by_tx(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    span: TracingSpan,
    tx_hash: ValidatedFixedBytes,
) -> Result<Json<OrdersByTxResponse>, ApiError> {
    async move {
        tracing::info!(tx_hash = ?tx_hash, "request received");
        let hash = tx_hash.0;
        let raindex = shared_raindex.read().await;
        let ds = RaindexOrdersListDataSource {
            client: raindex.client(),
        };
        let response = process_get_orders_by_tx(&ds, hash).await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

pub(crate) async fn process_get_orders_by_tx(
    ds: &dyn OrdersListDataSource,
    tx_hash: B256,
) -> Result<OrdersByTxResponse, ApiError> {
    let filters = GetOrdersFilters {
        tx_hashes: Some(vec![tx_hash]),
        ..Default::default()
    };
    let (orders, _total_count) = ds.get_orders_list(filters, None, None).await?;

    if orders.is_empty() {
        return Err(ApiError::NotFound(
            "no orders found for transaction".into(),
        ));
    }

    let (block_number, timestamp) = extract_tx_metadata(&orders[0]);

    let entries: Result<Vec<OrderByTxEntry>, ApiError> =
        orders.iter().map(build_order_by_tx_entry).collect();

    Ok(OrdersByTxResponse {
        tx_hash,
        block_number,
        timestamp,
        orders: entries?,
    })
}

fn extract_tx_metadata(order: &RaindexOrder) -> (u64, u64) {
    match order.transaction() {
        Some(tx) => {
            let block: u64 = tx.block_number().try_into().unwrap_or(0);
            let ts: u64 = tx.timestamp().try_into().unwrap_or(0);
            (block, ts)
        }
        None => {
            let ts: u64 = order.timestamp_added().try_into().unwrap_or(0);
            (0, ts)
        }
    }
}

fn build_order_by_tx_entry(order: &RaindexOrder) -> Result<OrderByTxEntry, ApiError> {
    let inputs = order.inputs_list().items();
    let outputs = order.outputs_list().items();

    let input = inputs.first().ok_or_else(|| {
        tracing::error!("order has no input vaults");
        ApiError::Internal("order has no input vaults".into())
    })?;
    let output = outputs.first().ok_or_else(|| {
        tracing::error!("order has no output vaults");
        ApiError::Internal("order has no output vaults".into())
    })?;

    let input_token_info = input.token();
    let output_token_info = output.token();

    Ok(OrderByTxEntry {
        order_hash: order.order_hash(),
        owner: order.owner(),
        orderbook_id: order.orderbook(),
        input_token: TokenRef {
            address: input_token_info.address(),
            symbol: input_token_info.symbol().unwrap_or_default(),
            decimals: input_token_info.decimals(),
        },
        output_token: TokenRef {
            address: output_token_info.address(),
            symbol: output_token_info.symbol().unwrap_or_default(),
            decimals: output_token_info.decimals(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::mock_order;
    use crate::routes::orders::test_fixtures::MockOrdersListDataSource;
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use alloy::primitives::Address;
    use rocket::http::{Header, Status};

    fn test_tx_hash() -> B256 {
        "0x0000000000000000000000000000000000000000000000000000000000000099"
            .parse()
            .unwrap()
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_tx_success() {
        let order = mock_order();
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![order]),
            total_count: 1,
            quotes: Ok(vec![]),
        };
        let result = process_get_orders_by_tx(&ds, test_tx_hash()).await.unwrap();

        assert_eq!(result.tx_hash, test_tx_hash());
        assert_eq!(result.block_number, 1);
        assert_eq!(result.timestamp, 1700000000);
        assert_eq!(result.orders.len(), 1);
        assert_eq!(
            result.orders[0].owner,
            "0x0000000000000000000000000000000000000001"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(result.orders[0].input_token.symbol, "USDC");
        assert_eq!(result.orders[0].output_token.symbol, "WETH");
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_tx_not_found() {
        let ds = MockOrdersListDataSource {
            orders: Ok(vec![]),
            total_count: 0,
            quotes: Ok(vec![]),
        };
        let result = process_get_orders_by_tx(&ds, test_tx_hash()).await;
        assert!(matches!(result, Err(ApiError::NotFound(_))));
    }

    #[rocket::async_test]
    async fn test_process_get_orders_by_tx_query_failure() {
        let ds = MockOrdersListDataSource {
            orders: Err(ApiError::Internal("failed".into())),
            total_count: 0,
            quotes: Ok(vec![]),
        };
        let result = process_get_orders_by_tx(&ds, test_tx_hash()).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_get_orders_by_tx_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/orders/tx/0x0000000000000000000000000000000000000000000000000000000000000099")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_orders_by_tx_invalid_hash_returns_422() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/orders/tx/not-a-hash")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::UnprocessableEntity);
    }
}
