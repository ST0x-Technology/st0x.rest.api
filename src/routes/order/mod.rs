mod cancel;
mod deploy_dca;
mod deploy_solver;
mod get_order;

use crate::error::ApiError;
use alloy::primitives::{Bytes, B256};
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::order_quotes::RaindexOrderQuote;
use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};
use rain_orderbook_common::raindex_client::trades::RaindexTrade;
use rain_orderbook_common::raindex_client::RaindexClient;
use rocket::Route;

#[async_trait]
pub(crate) trait OrderDataSource: Send + Sync {
    async fn get_orders_by_hash(&self, hash: B256) -> Result<Vec<RaindexOrder>, ApiError>;
    async fn get_order_quotes(
        &self,
        order: &RaindexOrder,
    ) -> Result<Vec<RaindexOrderQuote>, ApiError>;
    async fn get_order_trades(&self, order: &RaindexOrder) -> Result<Vec<RaindexTrade>, ApiError>;
    async fn get_remove_calldata(&self, order: &RaindexOrder) -> Result<Bytes, ApiError>;
}

pub(crate) struct RaindexOrderDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait]
impl<'a> OrderDataSource for RaindexOrderDataSource<'a> {
    async fn get_orders_by_hash(&self, hash: B256) -> Result<Vec<RaindexOrder>, ApiError> {
        let filters = GetOrdersFilters {
            order_hash: Some(hash),
            ..Default::default()
        };
        let result = self
            .client
            .get_orders(None, Some(filters), None, None)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query orders");
                ApiError::Internal("failed to query orders".into())
            })?;
        Ok(result.orders().to_vec())
    }

    async fn get_order_quotes(
        &self,
        order: &RaindexOrder,
    ) -> Result<Vec<RaindexOrderQuote>, ApiError> {
        order.get_quotes(None, None).await.map_err(|e| {
            tracing::error!(error = %e, "failed to query order quotes");
            ApiError::Internal("failed to query order quotes".into())
        })
    }

    async fn get_order_trades(&self, order: &RaindexOrder) -> Result<Vec<RaindexTrade>, ApiError> {
        order
            .get_trades_list(None, None, None)
            .await
            .map(|r| r.trades().to_vec())
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query order trades");
                ApiError::Internal("failed to query order trades".into())
            })
    }

    async fn get_remove_calldata(&self, order: &RaindexOrder) -> Result<Bytes, ApiError> {
        order.get_remove_calldata().map_err(|e| {
            tracing::error!(error = %e, "failed to get remove calldata");
            ApiError::Internal("failed to get remove calldata".into())
        })
    }
}

pub use cancel::*;
pub use deploy_dca::*;
pub use deploy_solver::*;
pub use get_order::*;

pub fn routes() -> Vec<Route> {
    rocket::routes![
        deploy_dca::post_order_dca,
        deploy_solver::post_order_solver,
        get_order::get_order,
        cancel::post_order_cancel
    ]
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::OrderDataSource;
    use crate::error::ApiError;
    use alloy::primitives::{Bytes, B256};
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::order_quotes::RaindexOrderQuote;
    use rain_orderbook_common::raindex_client::orders::RaindexOrder;
    use rain_orderbook_common::raindex_client::trades::{RaindexTrade, RaindexTradesListResult};
    use serde_json::json;

    pub fn stub_raindex_client() -> serde_json::Value {
        json!({
            "orderbook_yaml": {
                "documents": ["version: 4\nnetworks:\n  base:\n    rpcs:\n      - https://mainnet.base.org\n    chain-id: 8453\n    currency: ETH\nsubgraphs:\n  base: https://example.com/sg\norderbooks:\n  base:\n    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7\n    network: base\n    subgraph: base\n    deployment-block: 0\ndeployers:\n  base:\n    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D\n    network: base\n"],
                "profile": "strict"
            }
        })
    }

    pub fn order_json() -> serde_json::Value {
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

    pub fn trade_json() -> serde_json::Value {
        json!({
            "id": "0x0000000000000000000000000000000000000000000000000000000000000042",
            "chainId": 8453,
            "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
            "orderHash": "0x000000000000000000000000000000000000000000000000000000000000abcd",
            "owner": "0x0000000000000000000000000000000000000001",
            "ioRatio": "0xffffffff00000000000000000000000000000000000000000000000000000005",
            "formattedIoRatio": "2.0",
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

    pub fn quote_json(formatted_ratio: &str) -> serde_json::Value {
        json!({
            "pair": { "pairName": "USDC/WETH", "inputIndex": 0, "outputIndex": 0 },
            "blockNumber": 1,
            "data": {
                "maxOutput": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "formattedMaxOutput": "1",
                "maxInput": "0x0000000000000000000000000000000000000000000000000000000000000002",
                "formattedMaxInput": "2",
                "ratio": "0x0000000000000000000000000000000000000000000000000000000000000002",
                "formattedRatio": formatted_ratio,
                "inverseRatio": "0xffffffff00000000000000000000000000000000000000000000000000000005",
                "formattedInverseRatio": "0.5"
            },
            "success": true,
            "error": null
        })
    }

    pub fn mock_order() -> RaindexOrder {
        serde_json::from_value(order_json()).expect("deserialize mock RaindexOrder")
    }

    pub fn order_with_shared_vaults_json() -> serde_json::Value {
        let rc = stub_raindex_client();
        let shared_vault = |id: &str,
                            token_address: &str,
                            token_name: &str,
                            token_symbol: &str,
                            decimals: u8,
                            balance: &str,
                            formatted_balance: &str| {
            json!({
                "raindexClient": rc,
                "chainId": 8453,
                "vaultType": "input",
                "id": id,
                "owner": "0x0000000000000000000000000000000000000001",
                "vaultId": "0x0000000000000000000000000000000000000000000000000000000000000fab",
                "balance": balance,
                "formattedBalance": formatted_balance,
                "token": {
                    "chainId": 8453,
                    "id": token_address,
                    "address": token_address,
                    "name": token_name,
                    "symbol": token_symbol,
                    "decimals": decimals
                },
                "orderbook": "0xd2938e7c9fe3597f78832ce780feb61945c377d7",
                "ordersAsInputs": [],
                "ordersAsOutputs": []
            })
        };
        let vault_a = shared_vault(
            "0x03",
            "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2",
            "Wrapped MicroStrategy",
            "wtMSTR",
            18,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "0",
        );
        let vault_b = shared_vault(
            "0x04",
            "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599",
            "Wrapped BTC",
            "WBTC",
            8,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "0",
        );
        json!({
            "raindexClient": rc,
            "chainId": 8453,
            "id": "0x0000000000000000000000000000000000000000000000000000000000000002",
            "orderBytes": "0x01",
            "orderHash": "0x000000000000000000000000000000000000000000000000000000000000beef",
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
            "inputs": [vault_a.clone(), vault_b.clone()],
            "outputs": [vault_a, vault_b]
        })
    }

    pub fn mock_order_with_shared_vaults() -> RaindexOrder {
        serde_json::from_value(order_with_shared_vaults_json())
            .expect("deserialize mock RaindexOrder with shared vaults")
    }

    pub fn mock_trade() -> RaindexTrade {
        serde_json::from_value(trade_json()).expect("deserialize mock RaindexTrade")
    }

    pub fn mock_trades_list_result() -> RaindexTradesListResult {
        serde_json::from_value(serde_json::json!({
            "trades": [trade_json()],
            "totalCount": 1,
            "summary": [{
                "chainId": 8453,
                "inputToken": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                "outputToken": "0x4200000000000000000000000000000000000006",
                "totalInput": "0xffffffff00000000000000000000000000000000000000000000000000000005",
                "formattedTotalInput": "0.500000",
                "totalOutput": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "formattedTotalOutput": "-0.250000000000000000",
                "averageIoRatio": "0xffffffff00000000000000000000000000000000000000000000000000000005",
                "formattedAverageIoRatio": "2.0",
                "tradeCount": 1
            }]
        }))
        .expect("deserialize mock RaindexTradesListResult")
    }

    pub fn mock_empty_trades_list_result() -> RaindexTradesListResult {
        serde_json::from_value(serde_json::json!({
            "trades": [],
            "totalCount": 0,
            "summary": null
        }))
        .expect("deserialize mock empty RaindexTradesListResult")
    }

    pub fn mock_quote(formatted_ratio: &str) -> RaindexOrderQuote {
        serde_json::from_value(quote_json(formatted_ratio)).expect("deserialize mock quote")
    }

    pub fn mock_failed_quote() -> RaindexOrderQuote {
        serde_json::from_value(json!({
            "pair": { "pairName": "USDC/WETH", "inputIndex": 0, "outputIndex": 0 },
            "blockNumber": 1,
            "data": null,
            "success": false,
            "error": "quote failed"
        }))
        .expect("deserialize mock failed quote")
    }

    pub fn test_hash() -> B256 {
        "0x000000000000000000000000000000000000000000000000000000000000abcd"
            .parse()
            .unwrap()
    }

    pub struct MockOrderDataSource {
        pub orders: Result<Vec<RaindexOrder>, ApiError>,
        pub trades: Result<Vec<RaindexTrade>, ApiError>,
        pub quotes: Result<Vec<RaindexOrderQuote>, ApiError>,
        pub calldata: Result<Bytes, ApiError>,
    }

    #[async_trait]
    impl OrderDataSource for MockOrderDataSource {
        async fn get_orders_by_hash(&self, _hash: B256) -> Result<Vec<RaindexOrder>, ApiError> {
            match &self.orders {
                Ok(orders) => Ok(orders.clone()),
                Err(_) => Err(ApiError::Internal("failed to query orders".into())),
            }
        }
        async fn get_order_quotes(
            &self,
            _order: &RaindexOrder,
        ) -> Result<Vec<RaindexOrderQuote>, ApiError> {
            match &self.quotes {
                Ok(quotes) => Ok(quotes.clone()),
                Err(_) => Err(ApiError::Internal("failed to query order quotes".into())),
            }
        }
        async fn get_order_trades(
            &self,
            _order: &RaindexOrder,
        ) -> Result<Vec<RaindexTrade>, ApiError> {
            match &self.trades {
                Ok(trades) => Ok(trades.clone()),
                Err(_) => Err(ApiError::Internal("failed to query order trades".into())),
            }
        }
        async fn get_remove_calldata(&self, _order: &RaindexOrder) -> Result<Bytes, ApiError> {
            match &self.calldata {
                Ok(bytes) => Ok(bytes.clone()),
                Err(_) => Err(ApiError::Internal("failed to get remove calldata".into())),
            }
        }
    }
}
