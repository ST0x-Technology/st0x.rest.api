use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::{TokenRef, ValidatedFixedBytes};
use crate::types::orders::{OrderByTxEntry, OrdersByTxResponse};
use rain_orderbook_common::raindex_client::orders::RaindexOrder;
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
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
        let orders = query_add_orders_across_chains(raindex.client(), hash).await?;

        if orders.is_empty() {
            return Err(ApiError::NotFound("no orders found for transaction".into()));
        }

        let (block_number, timestamp) = extract_tx_metadata(&orders[0]);

        let entries: Result<Vec<OrderByTxEntry>, ApiError> =
            orders.iter().map(build_order_by_tx_entry).collect();

        Ok(Json(OrdersByTxResponse {
            tx_hash: hash,
            block_number,
            timestamp,
            orders: entries?,
        }))
    }
    .instrument(span.0)
    .await
}

async fn query_add_orders_across_chains(
    client: &RaindexClient,
    tx_hash: alloy::primitives::B256,
) -> Result<Vec<RaindexOrder>, ApiError> {
    let orderbooks = client.get_all_orderbooks().map_err(|e| {
        tracing::error!(error = %e, "failed to get orderbooks");
        ApiError::Internal("failed to get orderbooks".into())
    })?;

    let chain_ids: std::collections::HashSet<u32> =
        orderbooks.values().map(|ob| ob.network.chain_id).collect();

    let mut all_orders = Vec::new();
    let mut had_timeout = false;
    let mut had_hard_failure = false;

    for chain_id in &chain_ids {
        match client
            .get_add_orders_for_transaction(*chain_id, tx_hash, None, None)
            .await
        {
            Ok(orders) => all_orders.extend(orders),
            Err(RaindexError::TransactionIndexingTimeout { .. }) => {
                tracing::info!(chain_id, "transaction not yet indexed");
                had_timeout = true;
            }
            Err(e) => {
                tracing::warn!(chain_id, error = %e, "failed to query chain");
                had_hard_failure = true;
            }
        }
    }

    if !all_orders.is_empty() {
        return Ok(all_orders);
    }

    if had_hard_failure {
        return Err(ApiError::Internal(
            "one or more chains failed to query".into(),
        ));
    }

    if had_timeout {
        return Err(ApiError::Accepted(
            "transaction not yet indexed, try again later".into(),
        ));
    }

    Ok(all_orders)
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
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use rocket::http::{Header, Status};

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
