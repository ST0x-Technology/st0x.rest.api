use super::*;
use crate::attribution::{
    address_to_b256, u64_to_b256, AttributionSigner, AttributionState, ATTRIBUTION_SCHEMA_VERSION,
};
use crate::cache::RouteResponseCaches;
use crate::types::swap::SwapCalldataMode;
use alloy::hex::encode_prefixed;
use alloy::network::TransactionBuilder;
use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use alloy::rpc::types::TransactionRequest;
use alloy::serde::WithOtherFields;
use alloy::signers::{local::PrivateKeySigner, Signer};
use alloy::sol_types::{SolCall, SolValue};
use rain_orderbook_bindings::IRaindexV6::{takeOrders4Call, OrderV4};
use rain_orderbook_common::add_order::AddOrderArgs;
use rain_orderbook_common::dotrain_order::DotrainOrder;
use rain_orderbook_common::raindex_client::RaindexClient;
use raindex_test_fixtures::LocalEvm;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

struct MockSwapServices {
    base_url: String,
    subgraph_order: Arc<RwLock<Option<Value>>>,
    oracle_available: Arc<AtomicBool>,
    oracle_bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl MockSwapServices {
    async fn start(oracle_response: Value) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let subgraph_order = Arc::new(RwLock::new(None));
        let oracle_available = Arc::new(AtomicBool::new(true));
        let oracle_bodies = Arc::new(Mutex::new(Vec::new()));

        let orders = Arc::clone(&subgraph_order);
        let available = Arc::clone(&oracle_available);
        let bodies = Arc::clone(&oracle_bodies);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let orders = Arc::clone(&orders);
                let available = Arc::clone(&available);
                let bodies = Arc::clone(&bodies);
                let oracle_response = oracle_response.clone();
                tokio::spawn(async move {
                    let Some((path, body)) = read_request(&mut socket).await else {
                        return;
                    };
                    let (status, response) = match path.as_str() {
                        "/oracle" if available.load(Ordering::SeqCst) => {
                            bodies.lock().unwrap().push(body);
                            ("200 OK", oracle_response.to_string())
                        }
                        "/oracle" => {
                            bodies.lock().unwrap().push(body);
                            (
                                "503 Service Unavailable",
                                json!({"error": "offline"}).to_string(),
                            )
                        }
                        "/sg" => {
                            let orders = orders.read().await;
                            let values = orders.iter().cloned().collect::<Vec<_>>();
                            ("200 OK", json!({"data": {"orders": values}}).to_string())
                        }
                        _ => ("404 Not Found", json!({"error": "not found"}).to_string()),
                    };
                    write_response(&mut socket, status, &response).await;
                });
            }
        });

        Self {
            base_url: format!("http://{address}"),
            subgraph_order,
            oracle_available,
            oracle_bodies,
        }
    }

    fn oracle_url(&self) -> String {
        format!("{}/oracle", self.base_url)
    }

    fn subgraph_url(&self) -> String {
        format!("{}/sg", self.base_url)
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 8192];
    let (header_end, content_length) = loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header_end = header_end + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or_default();
            break (header_end, content_length);
        }
    };
    while request.len() < header_end + content_length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..read]);
    }

    let request_line = String::from_utf8_lossy(&request)
        .lines()
        .next()?
        .to_string();
    let path = request_line.split_whitespace().nth(1)?.to_string();
    Some((
        path,
        request[header_end..header_end + content_length].to_vec(),
    ))
}

async fn write_response(socket: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await.unwrap();
}

fn oracle_order_dotrain(
    rpc_url: &str,
    oracle_url: &str,
    rainlang: Address,
    input_token: Address,
    output_token: Address,
) -> String {
    format!(
        r#"
version: 6
networks:
    test-network:
        rpcs:
            - {rpc_url}
        chain-id: 8453
        network-id: 8453
        currency: ETH
rainlangs:
    test-rainlang:
        network: test-network
        address: {rainlang}
tokens:
    input:
        network: test-network
        address: {input_token}
        decimals: 18
        label: Input
        symbol: INPUT
    output:
        network: test-network
        address: {output_token}
        decimals: 18
        label: Output
        symbol: OUTPUT
raindex:
    test-raindex:
        address: 0x0000000000000000000000000000000000000001
orders:
    test-order:
        oracle-url: {oracle_url}
        inputs:
            - token: input
        outputs:
            - token: output
              vault-id: 1
scenarios:
    test-scenario:
        rainlang: test-rainlang
        bindings:
            max-amount: 1000
deployments:
    test-deployment:
        scenario: test-scenario
        order: test-order
---
#max-amount !Max output amount
#calculate-io
amount price: 100 2;
#handle-add-order
:;
#handle-io
:;
"#
    )
}

fn client_yaml(
    rpc_url: &str,
    subgraph_url: &str,
    raindex: Address,
    input_token: Address,
    output_token: Address,
) -> String {
    format!(
        r#"
version: 6
networks:
    test-network:
        rpcs:
            - {rpc_url}
        chain-id: 8453
        network-id: 8453
        currency: ETH
subgraphs:
    test-subgraph: {subgraph_url}
raindexes:
    test-raindex:
        address: {raindex}
        network: test-network
        subgraph: test-subgraph
        deployment-block: 0
tokens:
    input:
        network: test-network
        address: {input_token}
        decimals: 18
        label: Input
        symbol: INPUT
    output:
        network: test-network
        address: {output_token}
        decimals: 18
        label: Output
        symbol: OUTPUT
"#
    )
}

fn vault_json(
    id: &str,
    owner: Address,
    vault_id: B256,
    balance: U256,
    token: Address,
    symbol: &str,
    raindex: Address,
) -> Value {
    json!({
        "id": id,
        "owner": owner,
        "vaultId": vault_id,
        "balance": B256::from(balance),
        "token": {
            "id": token,
            "address": token,
            "name": symbol,
            "symbol": symbol,
            "decimals": "18",
        },
        "raindex": {"id": raindex},
        "ordersAsOutput": [],
        "ordersAsInput": [],
        "balanceChanges": [],
    })
}

fn subgraph_order_json(order: &OrderV4, order_hash: B256, meta: &[u8], raindex: Address) -> Value {
    let owner = order.owner;
    let input = &order.validInputs[0];
    let output = &order.validOutputs[0];
    json!({
        "id": order_hash,
        "orderBytes": encode_prefixed(order.abi_encode()),
        "orderHash": order_hash,
        "owner": owner,
        "outputs": [vault_json(
            "0x02",
            owner,
            output.vaultId,
            U256::from(10).pow(U256::from(20)),
            output.token,
            "OUTPUT",
            raindex
        )],
        "inputs": [vault_json(
            "0x01",
            owner,
            input.vaultId,
            U256::ZERO,
            input.token,
            "INPUT",
            raindex
        )],
        "raindex": {"id": raindex},
        "active": true,
        "timestampAdded": "1739448802",
        "meta": encode_prefixed(meta),
        "addEvents": [{
            "transaction": {
                "id": B256::from(U256::from(1)),
                "from": owner,
                "blockNumber": "1",
                "timestamp": "1739448802",
            }
        }],
        "trades": [],
        "removeEvents": [],
    })
}

#[rocket::async_test]
async fn test_v1_and_v2_calldata_preserve_oracle_and_embed_api_key_attribution() {
    let mut local_evm = LocalEvm::new().await;
    let oracle_signer = PrivateKeySigner::from(local_evm.anvil.keys()[0].clone());
    let oracle_context = B256::from(U256::from(42));
    let oracle_signature = oracle_signer
        .sign_message(keccak256(oracle_context).as_slice())
        .await
        .unwrap();
    let oracle_signature = Bytes::from(oracle_signature.as_bytes().to_vec());
    let oracle_signer_address = oracle_signer.address();
    let services = MockSwapServices::start(json!([{
        "signer": oracle_signer_address,
        "context": [oracle_context],
        "signature": oracle_signature,
    }]))
    .await;
    let owner = local_evm.signer_wallets[0].default_signer().address();
    let input = local_evm
        .deploy_new_token("Input", "INPUT", 18, U256::MAX, owner)
        .await;
    let output = local_evm
        .deploy_new_token("Output", "OUTPUT", 18, U256::MAX, owner)
        .await;
    let input_address = *input.address();
    let output_address = *output.address();
    let raindex = *local_evm.raindex.address();

    let dotrain = oracle_order_dotrain(
        &local_evm.url(),
        &services.oracle_url(),
        local_evm.rainlang,
        input_address,
        output_address,
    );
    let dotrain_order = DotrainOrder::create(dotrain.clone(), None).await.unwrap();
    let deployment = dotrain_order
        .dotrain_yaml()
        .get_deployment("test-deployment")
        .unwrap();
    let add_order_args = AddOrderArgs::new_from_deployment(dotrain, deployment, None)
        .await
        .unwrap();
    let add_order_call = add_order_args
        .try_into_call(vec![local_evm.url()])
        .await
        .unwrap();
    let meta = add_order_call.config.meta.clone();
    let (event, _) = local_evm
        .add_order(&add_order_call.abi_encode(), owner)
        .await;
    let order = OrderV4::abi_decode(&event.order.abi_encode()).unwrap();
    local_evm
        .deposit(
            owner,
            output_address,
            U256::from(10).pow(U256::from(20)),
            18,
            order.validOutputs[0].vaultId,
        )
        .await;
    input
        .approve(raindex, U256::MAX)
        .from(owner)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    *services.subgraph_order.write().await =
        Some(subgraph_order_json(&order, event.orderHash, &meta, raindex));
    let client = RaindexClient::new(
        vec![client_yaml(
            &local_evm.url(),
            &services.subgraph_url(),
            raindex,
            input_address,
            output_address,
        )],
        None,
        None,
    )
    .await
    .unwrap();
    let database_url = format!(
        "sqlite:file:{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    let pool = crate::db::init(&database_url, 1).await.unwrap();
    let caches = RouteResponseCaches::new(0, Duration::ZERO);
    let attribution_state = AttributionState::new(
        AttributionSigner::from_hex_key(
            "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .unwrap(),
    );
    assert_eq!(oracle_signer_address, attribution_state.signer.address());
    let attribution = attribution_state.for_api_key("customer-key", owner);
    let data_source = RaindexSwapDataSource {
        client: &client,
        caches: &caches,
        pool: &pool,
    };
    let request = SwapCalldataRequest {
        chain_id: Some(8453),
        taker: owner,
        input_token: input_address,
        output_token: output_address,
        output_amount: "10".to_string(),
        maximum_io_ratio: "3".to_string(),
        denomination: crate::types::swap::SwapDenomination::Wrapped,
    };

    let mut response = process_swap_calldata(&data_source, request.clone())
        .await
        .unwrap();
    embed_and_validate_attribution(&mut response, &attribution_state.signer, &attribution)
        .await
        .unwrap();
    let decoded = takeOrders4Call::abi_decode(&response.data).unwrap();
    assert_eq!(decoded.config.orders[0].signedContext.len(), 2);
    let signed_context = &decoded.config.orders[0].signedContext[0];
    assert_eq!(signed_context.signer, oracle_signer_address);
    assert_eq!(signed_context.context[0], oracle_context);
    assert_eq!(signed_context.signature, oracle_signature);

    let attributed_context = &decoded.config.orders[0].signedContext[1];
    assert_eq!(
        attributed_context.signer,
        attribution_state.signer.address()
    );
    assert_eq!(attributed_context.context.len(), 4);
    assert_eq!(
        attributed_context.context[0],
        u64_to_b256(ATTRIBUTION_SCHEMA_VERSION)
    );
    assert_eq!(attributed_context.context[1], attribution.api_key_hash);
    assert_eq!(attributed_context.context[2], address_to_b256(owner));
    assert_eq!(
        attributed_context.context[3],
        keccak256(decoded.config.orders[0].order.abi_encode())
    );

    local_evm
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default()
                .with_input(response.data.clone())
                .with_to(response.to)
                .with_from(owner),
        ))
        .await
        .unwrap();

    let v2_attribution = attribution_state.for_api_key("customer-key", owner);
    let v2_request = SwapCalldataV2Request {
        chain_id: Some(8453),
        taker: owner,
        input_token: input_address,
        output_token: output_address,
        mode: SwapCalldataMode::BuyUpTo,
        amount: "10".to_string(),
        price_cap: None,
        slippage_bps: Some(50),
        reference_io_ratio: Some("2".to_string()),
        denomination: crate::types::swap::SwapDenomination::Wrapped,
    };
    let mut v2_response = process_swap_calldata_v2(&data_source, v2_request)
        .await
        .unwrap();
    assert_eq!(v2_response.resolved_price_cap, "2.01");
    embed_and_validate_attribution(
        &mut v2_response.calldata,
        &attribution_state.signer,
        &v2_attribution,
    )
    .await
    .unwrap();
    let v2_decoded = takeOrders4Call::abi_decode(&v2_response.calldata.data).unwrap();
    assert_eq!(v2_decoded.config.orders[0].signedContext.len(), 2);
    let v2_oracle_context = &v2_decoded.config.orders[0].signedContext[0];
    assert_eq!(v2_oracle_context.signer, oracle_signer_address);
    assert_eq!(v2_oracle_context.context[0], oracle_context);
    assert_eq!(v2_oracle_context.signature, oracle_signature);
    let v2_context = &v2_decoded.config.orders[0].signedContext[1];
    assert_eq!(v2_context.context[1], v2_attribution.api_key_hash);

    local_evm
        .send_transaction(WithOtherFields::new(
            TransactionRequest::default()
                .with_input(v2_response.calldata.data.clone())
                .with_to(v2_response.calldata.to)
                .with_from(owner),
        ))
        .await
        .unwrap();

    let oracle_body = services.oracle_bodies.lock().unwrap()[0].clone();
    let oracle_request = <(OrderV4, U256, U256, Address)>::abi_decode(&oracle_body).unwrap();
    assert_eq!(oracle_request.0, order);
    assert_eq!(oracle_request.1, U256::ZERO);
    assert_eq!(oracle_request.2, U256::ZERO);
    assert_eq!(oracle_request.3, owner);

    services.oracle_available.store(false, Ordering::SeqCst);
    let error = process_swap_calldata(&data_source, request)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ApiError::Coded {
            code: ApiErrorCode::SwapNoLiquidity,
            ..
        }
    ));
}
