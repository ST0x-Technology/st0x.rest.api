use crate::error::ApiError;
use crate::raindex::RaindexProvider;
use crate::test_helpers::mock_raindex_config;
use rain_orderbook_app_settings::yaml::{
    raindex::{RaindexYaml, RaindexYamlValidation},
    YamlParsable,
};
use rocket::http::Status;
use rocket::local::asynchronous::Client;
use rocket::State;

fn raindex_yaml(networks: &str) -> RaindexYaml {
    RaindexYaml::new(
        vec![format!("version: 6\nnetworks:\n{networks}")],
        RaindexYamlValidation::default(),
    )
    .expect("valid network fixture")
}

fn single_network_yaml() -> RaindexYaml {
    raindex_yaml(
        "  base:\n    rpcs:\n      - https://mainnet.base.org\n    chain-id: 8453\n    currency: ETH\n",
    )
}

fn multiple_networks_yaml() -> RaindexYaml {
    raindex_yaml(
        "  optimism:\n    rpcs:\n      - https://mainnet.optimism.io\n    chain-id: 10\n    currency: ETH\n  base:\n    rpcs:\n      - https://mainnet.base.org\n    chain-id: 8453\n    currency: ETH\n  base-alias:\n    rpcs:\n      - https://mainnet.base.org\n    chain-id: 8453\n    currency: ETH\n",
    )
}

fn standalone_network_with_one_raindex_yaml() -> String {
    r#"version: 6
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
  ethereum:
    rpcs:
      - https://ethereum-rpc.publicnode.com
    chain-id: 1
  hyperevm:
    rpcs:
      - https://rpc.hyperliquid.xyz/evm
    chain-id: 999
subgraphs:
  base: https://example.com/base
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
tokens:
  base-token:
    address: 0x1111111111111111111111111111111111111111
    network: base
  ethereum-token:
    address: 0x1111111111111111111111111111111111111111
    network: ethereum
  hyperevm-token:
    address: 0x1111111111111111111111111111111111111111
    network: hyperevm
"#
    .to_string()
}

#[get("/shared-client")]
async fn shared_client_contract(
    provider: &State<RaindexProvider>,
) -> Result<&'static str, ApiError> {
    let orderbook_address =
        alloy::primitives::address!("0xd2938e7c9fe3597f78832ce780feb61945c377d7");

    provider
        .client()
        .get_raindex_subgraph_client(orderbook_address)
        .map(|_| "ok")
        .map_err(|e| ApiError::Internal(format!("{e}")))
}

#[rocket::async_test]
async fn test_shared_client_succeeds_with_valid_registry() {
    let raindex_config = mock_raindex_config().await;
    let rocket = rocket::build()
        .manage(raindex_config)
        .mount("/__test", rocket::routes![shared_client_contract]);
    let client = Client::tracked(rocket).await.expect("valid test client");

    let response = client.get("/__test/shared-client").dispatch().await;

    assert_eq!(response.status(), Status::Ok);
    let body = response.into_string().await.expect("response body");
    assert_eq!(body, "ok");
}

#[test]
fn configured_chain_ids_handles_zero_single_multiple_and_duplicate_networks() {
    let empty = raindex_yaml("  {}\n");
    assert!(super::configured_chain_ids(&empty)
        .expect("read empty networks")
        .is_empty());
    assert_eq!(
        super::configured_chain_ids(&single_network_yaml()).expect("read single network"),
        vec![8453]
    );
    assert_eq!(
        super::configured_chain_ids(&multiple_networks_yaml()).expect("read multiple networks"),
        vec![10, 8453]
    );
}

#[test]
fn required_chain_selection_defaults_and_rejects_ambiguity() {
    let empty = raindex_yaml("  {}\n");
    assert!(matches!(
        super::resolve_required_chain_id(&empty, None),
        Err(ApiError::Internal(message)) if message == "no configured networks"
    ));

    let single = single_network_yaml();
    assert_eq!(
        super::resolve_required_chain_id(&single, None).expect("default single network"),
        8453
    );
    assert_eq!(
        super::resolve_required_chain_id(&single, Some(8453)).expect("select supported network"),
        8453
    );
    assert!(matches!(
        super::resolve_required_chain_id(&single, Some(1)),
        Err(ApiError::BadRequest(message)) if message == "unsupported chainId"
    ));

    let multiple = multiple_networks_yaml();
    assert!(matches!(
        super::resolve_required_chain_id(&multiple, None),
        Err(ApiError::BadRequest(message))
            if message == "chainId is required when multiple networks are configured"
    ));
}

#[test]
fn optional_chain_filter_supports_all_chains_and_validates_requested_chain() {
    let multiple = multiple_networks_yaml();
    assert_eq!(
        super::optional_chain_ids_filter(&multiple, None).expect("all-chain filter"),
        None
    );
    assert_eq!(
        super::optional_chain_ids_filter(&multiple, Some(10)).expect("supported optional filter"),
        Some(vec![10])
    );
    assert!(matches!(
        super::optional_chain_ids_filter(&multiple, Some(1)),
        Err(ApiError::BadRequest(message)) if message == "unsupported chainId"
    ));
}

#[rocket::async_test]
async fn raindex_chain_selection_excludes_standalone_networks() {
    let client = rain_orderbook_common::raindex_client::RaindexClient::new(
        vec![standalone_network_with_one_raindex_yaml()],
        None,
        None,
    )
    .await
    .expect("valid raindex client");

    assert_eq!(
        super::configured_raindex_chain_ids(&client).expect("read configured raindexes"),
        vec![8453]
    );
    assert_eq!(
        super::required_raindex_chain_ids(&client).expect("select all raindex chains"),
        vec![8453]
    );
    let tokens = super::raindex_backed_tokens(&client).expect("select Raindex-backed tokens");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].network.chain_id, 8453);
    assert!(matches!(
        super::validate_raindex_chain_id(&client, 1),
        Err(ApiError::BadRequest(message)) if message == "unsupported chainId"
    ));
    assert!(matches!(
        super::validate_raindex_chain_id(&client, 999),
        Err(ApiError::BadRequest(message)) if message == "unsupported chainId"
    ));
}
