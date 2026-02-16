use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::{TokenRef, ValidatedFixedBytes};
use crate::types::order::{
    CancelOrderRequest, CancelOrderResponse, DeployDcaOrderRequest, DeployOrderResponse,
    DeploySolverOrderRequest, OrderDetail, OrderDetailsInfo, OrderTradeEntry, OrderType,
};
use alloy::primitives::B256;
use async_trait::async_trait;
use rain_orderbook_common::parsed_meta::ParsedMeta;
use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};
use rain_orderbook_common::raindex_client::trades::RaindexTrade;
use rain_orderbook_common::raindex_client::RaindexClient;
use rocket::serde::json::Json;
use rocket::{Route, State};
use tracing::Instrument;

#[async_trait(?Send)]
trait OrderDataSource {
    async fn get_orders_by_hash(&self, hash: B256) -> Result<Vec<RaindexOrder>, ApiError>;
    async fn get_order_io_ratio(&self, order: &RaindexOrder) -> String;
    async fn get_order_trades(&self, order: &RaindexOrder) -> Vec<RaindexTrade>;
}

struct RaindexOrderDataSource<'a> {
    client: &'a RaindexClient,
}

#[async_trait(?Send)]
impl<'a> OrderDataSource for RaindexOrderDataSource<'a> {
    async fn get_orders_by_hash(&self, hash: B256) -> Result<Vec<RaindexOrder>, ApiError> {
        let filters = GetOrdersFilters {
            order_hash: Some(hash),
            ..Default::default()
        };
        self.client
            .get_orders(None, Some(filters), None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query orders");
                ApiError::Internal("failed to query orders".into())
            })
    }

    async fn get_order_io_ratio(&self, order: &RaindexOrder) -> String {
        match order.get_quotes(None, None).await {
            Ok(quotes) => quotes
                .first()
                .and_then(|q| q.data.as_ref())
                .map(|d| d.formatted_ratio.clone())
                .unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to fetch order quotes");
                String::new()
            }
        }
    }

    async fn get_order_trades(&self, order: &RaindexOrder) -> Vec<RaindexTrade> {
        match order.get_trades_list(None, None, None).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "failed to fetch order trades");
                vec![]
            }
        }
    }
}

async fn process_get_order(ds: &dyn OrderDataSource, hash: B256) -> Result<OrderDetail, ApiError> {
    let orders = ds.get_orders_by_hash(hash).await?;
    let order = orders
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::NotFound("order not found".into()))?;
    let io_ratio = ds.get_order_io_ratio(&order).await;
    let trades = ds.get_order_trades(&order).await;
    let order_type = determine_order_type(&order);
    build_order_detail(&order, order_type, &io_ratio, &trades)
}

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
        raindex
            .run_with_client(move |_client| async move { todo!() })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}

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
        raindex
            .run_with_client(move |_client| async move { todo!() })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/order/{order_hash}",
    tag = "Order",
    security(("basicAuth" = [])),
    params(
        ("order_hash" = String, Path, description = "The order hash"),
    ),
    responses(
        (status = 200, description = "Order details", body = OrderDetail),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 404, description = "Order not found", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<order_hash>")]
pub async fn get_order(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    raindex: &State<crate::raindex::RaindexProvider>,
    span: TracingSpan,
    order_hash: ValidatedFixedBytes,
) -> Result<Json<OrderDetail>, ApiError> {
    async move {
        tracing::info!(order_hash = ?order_hash, "request received");
        let hash = order_hash.0;
        let detail = raindex
            .run_with_client(move |client| async move {
                let ds = RaindexOrderDataSource { client: &client };
                process_get_order(&ds, hash).await
            })
            .await
            .map_err(ApiError::from)??;
        Ok(Json(detail))
    }
    .instrument(span.0)
    .await
}

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
        raindex
            .run_with_client(move |_client| async move { todo!() })
            .await
            .map_err(ApiError::from)?
    }
    .instrument(span.0)
    .await
}

fn determine_order_type(order: &RaindexOrder) -> OrderType {
    for meta in order.parsed_meta() {
        if let ParsedMeta::DotrainGuiStateV1(gui_state) = meta {
            if gui_state
                .selected_deployment
                .to_lowercase()
                .contains("dca")
            {
                return OrderType::Dca;
            }
        }
    }
    OrderType::Solver
}

fn build_order_detail(
    order: &RaindexOrder,
    order_type: OrderType,
    io_ratio: &str,
    trades: &[RaindexTrade],
) -> Result<OrderDetail, ApiError> {
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

    let trade_entries: Vec<OrderTradeEntry> = trades.iter().map(map_trade).collect();

    let created_at: u64 = order
        .timestamp_added()
        .try_into()
        .unwrap_or(0);

    Ok(OrderDetail {
        order_hash: order.order_hash(),
        owner: order.owner(),
        order_details: OrderDetailsInfo {
            type_: order_type,
            io_ratio: io_ratio.to_string(),
        },
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
        input_vault_id: input.vault_id(),
        output_vault_id: output.vault_id(),
        input_vault_balance: input.formatted_balance(),
        output_vault_balance: output.formatted_balance(),
        io_ratio: io_ratio.to_string(),
        created_at,
        orderbook_id: order.orderbook(),
        trades: trade_entries,
    })
}

fn map_trade(trade: &RaindexTrade) -> OrderTradeEntry {
    let timestamp: u64 = trade.timestamp().try_into().unwrap_or(0);
    let tx = trade.transaction();
    OrderTradeEntry {
        id: trade.id().to_string(),
        tx_hash: tx.id(),
        input_amount: trade.input_vault_balance_change().formatted_amount(),
        output_amount: trade.output_vault_balance_change().formatted_amount(),
        timestamp,
        sender: tx.from(),
    }
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        post_order_dca,
        post_order_solver,
        get_order,
        post_order_cancel
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{
        basic_auth_header, mock_invalid_raindex_config, seed_api_key, TestClientBuilder,
    };
    use alloy::primitives::Address;
    use rocket::http::{Header, Status};
    use serde_json::json;

    fn stub_raindex_client() -> serde_json::Value {
        json!({
            "orderbook_yaml": {
                "documents": ["version: 4\nnetworks:\n  base:\n    rpcs:\n      - https://mainnet.base.org\n    chain-id: 8453\n    currency: ETH\nsubgraphs:\n  base: https://example.com/sg\norderbooks:\n  base:\n    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7\n    network: base\n    subgraph: base\n    deployment-block: 0\ndeployers:\n  base:\n    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D\n    network: base\n"],
                "profile": "strict"
            }
        })
    }

    fn order_json() -> serde_json::Value {
        let rc = stub_raindex_client();
        json!({
            "raindexClient": rc,
            "chainId": 8453,
            "id": "0x0000000000000000000000000000000000000000000000000000000000000001",
            "orderBytes": "0x01",
            "orderHash": "0x000000000000000000000000000000000000000000000000000000000000abcd",
            "owner": "0x0000000000000000000000000000000000000001",
            "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
            "active": true,
            "timestampAdded": "0x000000000000000000000000000000000000000000000000000000006553f100",
            "meta": null,
            "parsedMeta": [],
            "rainlang": null,
            "transaction": {
                "id": "0x0000000000000000000000000000000000000000000000000000000000000099",
                "from": "0x0000000000000000000000000000000000000001",
                "blockNumber": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "timestamp": "0x000000000000000000000000000000000000000000000000000000006553f100"
            },
            "tradesCount": 0,
            "inputs": [{
                "raindexClient": rc,
                "chainId": 8453,
                "vaultType": "input",
                "id": "0x01",
                "owner": "0x0000000000000000000000000000000000000001",
                "vaultId": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "balance": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "formattedBalance": "1.000000",
                "token": {
                    "chainId": 8453,
                    "id": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                    "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                    "name": "USD Coin",
                    "symbol": "USDC",
                    "decimals": 6
                },
                "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                "ordersAsInputs": [],
                "ordersAsOutputs": []
            }],
            "outputs": [{
                "raindexClient": rc,
                "chainId": 8453,
                "vaultType": "output",
                "id": "0x02",
                "owner": "0x0000000000000000000000000000000000000001",
                "vaultId": "0x0000000000000000000000000000000000000000000000000000000000000002",
                "balance": "0xffffffff00000000000000000000000000000000000000000000000000000005",
                "formattedBalance": "0.500000000000000000",
                "token": {
                    "chainId": 8453,
                    "id": "0x4200000000000000000000000000000000000006",
                    "address": "0x4200000000000000000000000000000000000006",
                    "name": "Wrapped Ether",
                    "symbol": "WETH",
                    "decimals": 18
                },
                "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                "ordersAsInputs": [],
                "ordersAsOutputs": []
            }]
        })
    }

    fn trade_json() -> serde_json::Value {
        json!({
            "id": "0x0000000000000000000000000000000000000000000000000000000000000042",
            "orderHash": "0x000000000000000000000000000000000000000000000000000000000000abcd",
            "transaction": {
                "id": "0x0000000000000000000000000000000000000000000000000000000000000088",
                "from": "0x0000000000000000000000000000000000000002",
                "blockNumber": "0x0000000000000000000000000000000000000000000000000000000000000064",
                "timestamp": "0x000000000000000000000000000000000000000000000000000000006553f4e8"
            },
            "inputVaultBalanceChange": {
                "type": "takeOrder",
                "vaultId": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "token": {
                    "chainId": 8453,
                    "id": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                    "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                    "name": "USD Coin",
                    "symbol": "USDC",
                    "decimals": 6
                },
                "amount": "0xffffffff00000000000000000000000000000000000000000000000000000005",
                "formattedAmount": "0.500000",
                "newBalance": "0xffffffff0000000000000000000000000000000000000000000000000000000f",
                "formattedNewBalance": "1.500000",
                "oldBalance": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "formattedOldBalance": "1.000000",
                "timestamp": "0x000000000000000000000000000000000000000000000000000000006553f4e8",
                "transaction": {
                    "id": "0x0000000000000000000000000000000000000000000000000000000000000088",
                    "from": "0x0000000000000000000000000000000000000002",
                    "blockNumber": "0x0000000000000000000000000000000000000000000000000000000000000064",
                    "timestamp": "0x000000000000000000000000000000000000000000000000000000006553f4e8"
                },
                "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7"
            },
            "outputVaultBalanceChange": {
                "type": "takeOrder",
                "vaultId": "0x0000000000000000000000000000000000000000000000000000000000000002",
                "token": {
                    "chainId": 8453,
                    "id": "0x4200000000000000000000000000000000000006",
                    "address": "0x4200000000000000000000000000000000000006",
                    "name": "Wrapped Ether",
                    "symbol": "WETH",
                    "decimals": 18
                },
                "amount": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "formattedAmount": "-0.250000000000000000",
                "newBalance": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "formattedNewBalance": "0.250000000000000000",
                "oldBalance": "0xffffffff00000000000000000000000000000000000000000000000000000005",
                "formattedOldBalance": "0.500000000000000000",
                "timestamp": "0x000000000000000000000000000000000000000000000000000000006553f4e8",
                "transaction": {
                    "id": "0x0000000000000000000000000000000000000000000000000000000000000088",
                    "from": "0x0000000000000000000000000000000000000002",
                    "blockNumber": "0x0000000000000000000000000000000000000000000000000000000000000064",
                    "timestamp": "0x000000000000000000000000000000000000000000000000000000006553f4e8"
                },
                "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7"
            },
            "timestamp": "0x000000000000000000000000000000000000000000000000000000006553f4e8",
            "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7"
        })
    }

    fn mock_order() -> RaindexOrder {
        serde_json::from_value(order_json()).expect("deserialize mock RaindexOrder")
    }

    fn mock_trade() -> RaindexTrade {
        serde_json::from_value(trade_json()).expect("deserialize mock RaindexTrade")
    }

    struct MockOrderDataSource {
        orders: Result<Vec<RaindexOrder>, ApiError>,
        trades: Vec<RaindexTrade>,
        io_ratio: String,
    }

    #[async_trait(?Send)]
    impl OrderDataSource for MockOrderDataSource {
        async fn get_orders_by_hash(&self, _hash: B256) -> Result<Vec<RaindexOrder>, ApiError> {
            match &self.orders {
                Ok(orders) => Ok(orders.clone()),
                Err(_) => Err(ApiError::Internal("failed to query orders".into())),
            }
        }
        async fn get_order_io_ratio(&self, _order: &RaindexOrder) -> String {
            self.io_ratio.clone()
        }
        async fn get_order_trades(&self, _order: &RaindexOrder) -> Vec<RaindexTrade> {
            self.trades.clone()
        }
    }

    fn test_hash() -> B256 {
        "0x000000000000000000000000000000000000000000000000000000000000abcd"
            .parse()
            .unwrap()
    }

    #[rocket::async_test]
    async fn test_process_get_order_success() {
        let ds = MockOrderDataSource {
            orders: Ok(vec![mock_order()]),
            trades: vec![mock_trade()],
            io_ratio: "1.5".into(),
        };
        let detail = process_get_order(&ds, test_hash()).await.unwrap();

        assert_eq!(detail.order_hash, test_hash());
        assert_eq!(
            detail.owner,
            "0x0000000000000000000000000000000000000001"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(detail.input_token.symbol, "USDC");
        assert_eq!(detail.output_token.symbol, "WETH");
        assert_eq!(detail.input_vault_balance, "1.000000");
        assert_eq!(detail.output_vault_balance, "0.500000000000000000");
        assert_eq!(detail.io_ratio, "1.5");
        assert_eq!(detail.order_details.type_, OrderType::Solver);
        assert_eq!(detail.order_details.io_ratio, "1.5");
        assert_eq!(detail.created_at, 1700000000);
        assert_eq!(detail.trades.len(), 1);
        assert_eq!(detail.trades[0].input_amount, "0.500000");
        assert_eq!(detail.trades[0].output_amount, "-0.250000000000000000");
        assert_eq!(detail.trades[0].timestamp, 1700001000);
    }

    #[rocket::async_test]
    async fn test_process_get_order_not_found() {
        let ds = MockOrderDataSource {
            orders: Ok(vec![]),
            trades: vec![],
            io_ratio: String::new(),
        };
        let result = process_get_order(&ds, test_hash()).await;
        assert!(matches!(result, Err(ApiError::NotFound(_))));
    }

    #[rocket::async_test]
    async fn test_process_get_order_empty_trades() {
        let ds = MockOrderDataSource {
            orders: Ok(vec![mock_order()]),
            trades: vec![],
            io_ratio: "2.0".into(),
        };
        let detail = process_get_order(&ds, test_hash()).await.unwrap();
        assert!(detail.trades.is_empty());
        assert_eq!(detail.io_ratio, "2.0");
    }

    #[rocket::async_test]
    async fn test_process_get_order_query_failure() {
        let ds = MockOrderDataSource {
            orders: Err(ApiError::Internal("failed to query orders".into())),
            trades: vec![],
            io_ratio: String::new(),
        };
        let result = process_get_order(&ds, test_hash()).await;
        assert!(matches!(result, Err(ApiError::Internal(_))));
    }

    #[rocket::async_test]
    async fn test_determine_order_type_solver_default() {
        let order = mock_order();
        assert_eq!(determine_order_type(&order), OrderType::Solver);
    }

    #[rocket::async_test]
    async fn test_get_order_401_without_auth() {
        let client = TestClientBuilder::new().build().await;
        let response = client
            .get("/v1/order/0x000000000000000000000000000000000000000000000000000000000000abcd")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
    }

    #[rocket::async_test]
    async fn test_get_order_500_when_client_init_fails() {
        let config = mock_invalid_raindex_config().await;
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/order/0x000000000000000000000000000000000000000000000000000000000000abcd")
            .header(Header::new("Authorization", header))
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
