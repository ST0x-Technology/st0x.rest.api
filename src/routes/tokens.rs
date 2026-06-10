use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::SharedRaindexProvider;
use crate::types::common::ValidatedAddress;
use crate::wrap_ratio::{
    build_wrap_ratio_response, find_wrap_ratio_item, is_st0x_token, read_wrap_ratios_batch,
    unwrapped_address, WrapRatioBatchInput, WrapRatioMetadata, WrapRatioResponse,
};
use alloy::primitives::Address;
use rain_orderbook_app_settings::token::TokenCfg;
use rain_orderbook_common::raindex_client::RaindexError;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tracing::Instrument;

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    #[serde(flatten)]
    token: TokenCfg,
    name: Option<String>,
    isin: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WrapRatioErrorResponse {
    #[schema(value_type = String, example = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2")]
    share_address: Address,
    #[schema(example = "failed to read ERC4626 ratio")]
    message: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WrapRatioBatchResponse {
    data: Vec<WrapRatioResponse>,
    errors: Vec<WrapRatioErrorResponse>,
}

impl From<TokenCfg> for TokenResponse {
    fn from(mut token: TokenCfg) -> Self {
        let name = token.label.clone();
        let isin = token
            .extensions
            .as_ref()
            .and_then(|extensions| extract_extension_string(extensions, "isin"))
            .or_else(|| {
                token
                    .extensions
                    .as_ref()
                    .and_then(|extensions| extract_extension_string(extensions, "ISIN"))
            });

        token.network = Arc::new(sanitize_network(token.network.as_ref()));

        Self { token, name, isin }
    }
}

fn sanitize_network(
    network: &rain_orderbook_app_settings::network::NetworkCfg,
) -> rain_orderbook_app_settings::network::NetworkCfg {
    let mut network = network.clone();
    network.rpcs.clear();
    network
}

fn extract_extension_string(
    extensions: &std::collections::HashMap<String, Value>,
    key: &str,
) -> Option<String> {
    match extensions.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Null) | None => None,
        Some(value) => Some(value.to_string()),
    }
}

fn token_lookup_error(error: RaindexError) -> ApiError {
    tracing::error!(error = %error, "failed to get tokens from raindex");
    ApiError::Internal("failed to retrieve token list".into())
}

async fn registry_tokens(
    shared_raindex: &State<SharedRaindexProvider>,
) -> Result<Vec<TokenCfg>, ApiError> {
    let raindex = shared_raindex.read().await;
    let tokens = raindex
        .client()
        .get_all_tokens()
        .map_err(token_lookup_error)?
        .into_values()
        .collect();
    Ok(tokens)
}

#[utoipa::path(
    get,
    path = "/v1/tokens",
    tag = "Tokens",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "List of supported tokens", body = Vec<serde_json::Value>),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/")]
pub async fn get_tokens(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
) -> Result<Json<Vec<TokenResponse>>, ApiError> {
    async move {
        tracing::info!("request received");

        let raindex = shared_raindex.read().await;
        let tokens = raindex
            .client()
            .get_all_tokens()
            .map_err(|e: RaindexError| {
                tracing::error!(error = %e, "failed to get tokens from raindex");
                ApiError::Internal("failed to retrieve token list".into())
            })?;

        let result: Vec<TokenResponse> = tokens.into_values().map(TokenResponse::from).collect();
        tracing::info!(count = result.len(), "returning tokens");
        Ok(Json(result))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/tokens/wrap-ratio",
    tag = "Tokens",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "Wrapped ST0x token ratios", body = WrapRatioBatchResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/wrap-ratio")]
pub async fn get_wrap_ratios(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
) -> Result<Json<WrapRatioBatchResponse>, ApiError> {
    async move {
        tracing::info!("request received");

        let tokens = registry_tokens(shared_raindex).await?;
        let st0x_tokens: Vec<&TokenCfg> =
            tokens.iter().filter(|token| is_st0x_token(token)).collect();
        tracing::info!(count = st0x_tokens.len(), "reading wrapped token ratios");

        let mut data = Vec::new();
        let mut errors = Vec::new();
        let mut inputs = Vec::new();

        for token in st0x_tokens {
            match unwrapped_address(token) {
                Ok(expected_asset_address) => inputs.push(WrapRatioBatchInput {
                    token,
                    expected_asset_address,
                }),
                Err(error) => {
                    tracing::error!(
                        share_address = %token.address,
                        error = %error,
                        "failed to read wrapped token ratio"
                    );
                    errors.push(WrapRatioErrorResponse {
                        share_address: token.address,
                        message: error.batch_message(),
                    });
                }
            }
        }

        for group in read_wrap_ratios_batch(&inputs).await? {
            let metadata = WrapRatioMetadata::from_batch_response(&group.response);

            if group.response.items.len() != group.input_indices.len() {
                tracing::error!(
                    expected = group.input_indices.len(),
                    actual = group.response.items.len(),
                    "ERC4626 batch response item count mismatch"
                );
            }

            for index in group.input_indices {
                let Some(input) = inputs.get(index) else {
                    continue;
                };

                let row = find_wrap_ratio_item(&group.response.items, input.token.address)
                    .and_then(|item| {
                        build_wrap_ratio_response(item, input.expected_asset_address, &metadata)
                    });

                match row {
                    Ok(row) => data.push(row),
                    Err(error) => {
                        tracing::error!(
                            share_address = %input.token.address,
                            error = %error,
                            "failed to read wrapped token ratio"
                        );
                        errors.push(WrapRatioErrorResponse {
                            share_address: input.token.address,
                            message: error.batch_message(),
                        });
                    }
                }
            }
        }

        tracing::info!(
            data_count = data.len(),
            error_count = errors.len(),
            "returning wrapped token ratios"
        );
        Ok(Json(WrapRatioBatchResponse { data, errors }))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/tokens/wrap-ratio/{address}",
    tag = "Tokens",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Wrapped token / ERC4626 vault address")
    ),
    responses(
        (status = 200, description = "Wrapped token ratio", body = WrapRatioResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "Wrapped ST0x token not found", body = ApiErrorResponse),
        (status = 422, description = "Invalid wrapped token address", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/wrap-ratio/<address>")]
pub async fn get_wrap_ratio_by_address(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
    address: ValidatedAddress,
) -> Result<Json<WrapRatioResponse>, ApiError> {
    async move {
        tracing::info!(share_address = %address.0, "request received");

        let tokens = registry_tokens(shared_raindex).await?;
        let Some(token) = tokens
            .iter()
            .find(|token| token.address == address.0 && is_st0x_token(token))
        else {
            tracing::warn!(share_address = %address.0, "wrapped ST0x token not found");
            return Err(ApiError::NotFound("wrapped ST0x token not found".into()));
        };

        let expected_asset_address = unwrapped_address(token).map_err(|error| {
            tracing::error!(
                share_address = %token.address,
                error = %error,
                "failed to read wrapped token ratio"
            );
            error.into_api_error()
        })?;

        let inputs = vec![WrapRatioBatchInput {
            token,
            expected_asset_address,
        }];
        let mut groups = read_wrap_ratios_batch(&inputs).await?;
        let group = groups.pop().ok_or_else(|| {
            tracing::error!(
                share_address = %token.address,
                "ERC4626 batch response did not include requested token"
            );
            ApiError::Internal("failed to read ERC4626 ratio".into())
        })?;

        if group.response.items.len() != 1 {
            tracing::error!(
                expected = 1,
                actual = group.response.items.len(),
                "ERC4626 batch response item count mismatch"
            );
            return Err(ApiError::Internal("failed to read ERC4626 ratio".into()));
        }

        let metadata = WrapRatioMetadata::from_batch_response(&group.response);
        let item = find_wrap_ratio_item(&group.response.items, token.address).map_err(|error| {
            tracing::error!(
                share_address = %token.address,
                error = %error,
                "failed to read wrapped token ratio"
            );
            error.into_api_error()
        })?;

        let response = build_wrap_ratio_response(item, expected_asset_address, &metadata).map_err(
            |error| {
                tracing::error!(
                    share_address = %token.address,
                    error = %error,
                    "failed to read wrapped token ratio"
                );
                error.into_api_error()
            },
        )?;

        tracing::info!(share_address = %token.address, "returning wrapped token ratio");
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_tokens, get_wrap_ratios, get_wrap_ratio_by_address]
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{
        basic_auth_header, mock_raindex_registry_url_with_settings_and_tokens, seed_api_key,
        TestClientBuilder,
    };
    use alloy::primitives::{address, Address, U256};
    use alloy::providers::bindings::IMulticall3::{
        aggregate3Call, aggregateCall, aggregateReturn, getCurrentBlockTimestampCall,
        Result as Multicall3Result,
    };
    use alloy::{hex::encode_prefixed, sol_types::SolCall};
    use rain_erc::erc4626::{
        IERC20Metadata::decimalsCall as erc20DecimalsCall,
        IERC4626::{assetCall, convertToAssetsCall, decimalsCall as erc4626DecimalsCall},
    };
    use rocket::http::{Header, Status};
    use serde_json::json;

    const WT_MSTR: Address = address!("ff05e1bd696900dc6a52ca35ca61bb1024eda8e2");
    const T_MSTR: Address = address!("013b782f402d61aa1004cca95b9f5bb402c9d5fe");
    const WT_SECOND: Address = address!("3333333333333333333333333333333333333333");
    const T_SECOND: Address = address!("4444444444444444444444444444444444444444");
    const WT_BAD: Address = address!("1111111111111111111111111111111111111111");
    const T_BAD: Address = address!("2222222222222222222222222222222222222222");

    fn success_result<C: SolCall>(value: &C::Return) -> Multicall3Result {
        Multicall3Result {
            success: true,
            returnData: C::abi_encode_returns(value).into(),
        }
    }

    fn failed_result() -> Multicall3Result {
        Multicall3Result {
            success: false,
            returnData: Vec::<u8>::new().into(),
        }
    }

    fn timestamp_success(block_number: u64, timestamp: u64) -> String {
        let ret = aggregateReturn {
            blockNumber: U256::from(block_number),
            returnData: vec![
                getCurrentBlockTimestampCall::abi_encode_returns(&U256::from(timestamp)).into(),
            ],
        };
        encode_prefixed(aggregateCall::abi_encode_returns(&ret))
    }

    async fn mock_erc4626_batch_rpc(
        vault: Address,
        asset: Address,
        share_decimals: u8,
        asset_decimals: u8,
        converted_assets: U256,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock rpc");
        let addr = listener.local_addr().expect("mock rpc address");

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };

                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = tokio::io::AsyncReadExt::read(&mut socket, &mut buf)
                        .await
                        .unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);

                    let request_lower = request.to_ascii_lowercase();
                    let result = if request.contains("eth_blockNumber") {
                        json!("0x7b")
                    } else if request.contains(
                        encode_prefixed(getCurrentBlockTimestampCall::SELECTOR)
                            .trim_start_matches("0x"),
                    ) {
                        json!(timestamp_success(123, 456))
                    } else if request.contains(
                        encode_prefixed(erc4626DecimalsCall::SELECTOR).trim_start_matches("0x"),
                    ) && (request_lower.contains(
                        format!("{vault:#x}")
                            .trim_start_matches("0x")
                            .to_ascii_lowercase()
                            .as_str(),
                    ) || request_lower.contains(
                        format!("{WT_BAD:#x}")
                            .trim_start_matches("0x")
                            .to_ascii_lowercase()
                            .as_str(),
                    )) {
                        let vault_hex = format!("{vault:#x}")
                            .trim_start_matches("0x")
                            .to_ascii_lowercase();
                        let bad_hex = format!("{WT_BAD:#x}")
                            .trim_start_matches("0x")
                            .to_ascii_lowercase();
                        let mut targets = Vec::new();
                        if let Some(index) = request_lower.find(&vault_hex) {
                            targets.push(index);
                        }
                        if let Some(index) = request_lower.find(&bad_hex) {
                            targets.push(index);
                        }
                        targets.sort();

                        let results = targets
                            .into_iter()
                            .map(|_| success_result::<erc4626DecimalsCall>(&share_decimals))
                            .collect::<Vec<_>>();

                        json!(encode_prefixed(aggregate3Call::abi_encode_returns(
                            &results
                        )))
                    } else if request
                        .contains(encode_prefixed(assetCall::SELECTOR).trim_start_matches("0x"))
                    {
                        let vault_hex = format!("{vault:#x}")
                            .trim_start_matches("0x")
                            .to_ascii_lowercase();
                        let bad_hex = format!("{WT_BAD:#x}")
                            .trim_start_matches("0x")
                            .to_ascii_lowercase();
                        let mut targets = Vec::new();
                        if let Some(index) = request_lower.find(&vault_hex) {
                            targets.push((index, false));
                        }
                        if let Some(index) = request_lower.find(&bad_hex) {
                            targets.push((index, true));
                        }
                        targets.sort_by_key(|(index, _)| *index);

                        let mut results = Vec::with_capacity(targets.len());
                        for (_, is_bad) in targets {
                            if is_bad {
                                results.push(failed_result());
                            } else {
                                results.push(success_result::<assetCall>(&asset));
                            }
                        }

                        json!(encode_prefixed(aggregate3Call::abi_encode_returns(
                            &results
                        )))
                    } else if request.contains(
                        encode_prefixed(erc20DecimalsCall::SELECTOR).trim_start_matches("0x"),
                    ) && request_lower.contains(
                        format!("{asset:#x}")
                            .trim_start_matches("0x")
                            .to_ascii_lowercase()
                            .as_str(),
                    ) {
                        let results = vec![success_result::<erc20DecimalsCall>(&asset_decimals)];
                        json!(encode_prefixed(aggregate3Call::abi_encode_returns(
                            &results
                        )))
                    } else if request.contains(
                        encode_prefixed(convertToAssetsCall::SELECTOR).trim_start_matches("0x"),
                    ) {
                        let results =
                            vec![success_result::<convertToAssetsCall>(&converted_assets)];
                        json!(encode_prefixed(aggregate3Call::abi_encode_returns(
                            &results
                        )))
                    } else {
                        json!("0x")
                    };

                    let body = json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": result,
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ =
                        tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
                });
            }
        });

        format!("http://{addr}/rpc")
    }

    async fn wrap_ratio_client(rpc_url: &str) -> rocket::local::asynchronous::Client {
        let settings = format!(
            r#"version: 6
networks:
  base:
    rpcs:
      - {rpc_url}
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
using-tokens-from:
  - __TOKENS_URL__
"#
        );
        let remote_tokens = format!(
            r#"{{
  "name": "ST0x Base Token List",
  "timestamp": "2026-06-02T00:00:00.000Z",
  "version": {{
    "major": 1,
    "minor": 0,
    "patch": 0
  }},
  "tokens": [
    {{
      "chainId": 8453,
      "address": "{WT_MSTR:#x}",
      "decimals": 18,
      "name": "Wrapped MicroStrategy ST0x",
      "symbol": "wtMSTR",
      "extensions": {{
        "category": "ST0x",
        "unwrappedAddress": "{T_MSTR:#x}"
      }}
    }},
    {{
      "chainId": 8453,
      "address": "{T_MSTR:#x}",
      "decimals": 18,
      "name": "MicroStrategy ST0x",
      "symbol": "tMSTR"
    }},
    {{
      "chainId": 8453,
      "address": "{WT_BAD:#x}",
      "decimals": 18,
      "name": "Bad Wrapped ST0x",
      "symbol": "wtBAD",
      "extensions": {{
        "category": "ST0x",
        "unwrappedAddress": "{T_BAD:#x}"
      }}
    }},
    {{
      "chainId": 8453,
      "address": "0x4200000000000000000000000000000000000006",
      "decimals": 18,
      "name": "Wrapped Ether",
      "symbol": "WETH"
    }}
  ]
}}"#
        );
        let registry_url =
            mock_raindex_registry_url_with_settings_and_tokens(&settings, &remote_tokens).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load raindex config");
        TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await
    }

    async fn wrap_ratio_multi_network_client(
        base_rpc_url: &str,
        polygon_rpc_url: &str,
    ) -> rocket::local::asynchronous::Client {
        let settings = format!(
            r#"version: 6
networks:
  base:
    rpcs:
      - {base_rpc_url}
    chain-id: 8453
    currency: ETH
  polygon:
    rpcs:
      - {polygon_rpc_url}
    chain-id: 137
    currency: POL
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
using-tokens-from:
  - __TOKENS_URL__
"#
        );
        let remote_tokens = format!(
            r#"{{
  "name": "ST0x Multi Network Token List",
  "timestamp": "2026-06-02T00:00:00.000Z",
  "version": {{
    "major": 1,
    "minor": 0,
    "patch": 0
  }},
  "tokens": [
    {{
      "chainId": 8453,
      "address": "{WT_MSTR:#x}",
      "decimals": 18,
      "name": "Wrapped MicroStrategy ST0x",
      "symbol": "wtMSTR",
      "extensions": {{
        "category": "ST0x",
        "unwrappedAddress": "{T_MSTR:#x}"
      }}
    }},
    {{
      "chainId": 137,
      "address": "{WT_SECOND:#x}",
      "decimals": 18,
      "name": "Wrapped Second ST0x",
      "symbol": "wtSECOND",
      "extensions": {{
        "category": "ST0x",
        "unwrappedAddress": "{T_SECOND:#x}"
      }}
    }}
  ]
}}"#
        );
        let registry_url =
            mock_raindex_registry_url_with_settings_and_tokens(&settings, &remote_tokens).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load raindex config");
        TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await
    }

    #[rocket::async_test]
    async fn test_get_tokens_returns_token_list() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let tokens = body.as_array().expect("tokens is an array");
        assert_eq!(tokens.len(), 1);
        let first = &tokens[0];
        assert_eq!(
            first["address"],
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
        );
    }

    #[rocket::async_test]
    async fn test_get_tokens_returns_multiple_tokens() {
        let settings = r#"version: 6
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
tokens:
  usdc:
    address: 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
    network: base
    decimals: 6
    label: USD Coin
    symbol: USDC
  weth:
    address: 0x4200000000000000000000000000000000000006
    network: base
    decimals: 18
    label: Wrapped Ether
    symbol: WETH
"#;
        let registry_url =
            crate::test_helpers::mock_raindex_registry_url_with_settings(settings).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load raindex config");
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let tokens = body.as_array().expect("tokens is an array");
        assert_eq!(tokens.len(), 2);
    }

    #[rocket::async_test]
    async fn test_get_tokens_clears_network_rpcs() {
        let private_rpc = "https://private-rpc.example.com/secret-token";
        let settings = format!(
            r#"version: 6
networks:
  base:
    rpcs:
      - {private_rpc}
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
tokens:
  usdc:
    address: 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
    network: base
    decimals: 6
    label: USD Coin
    symbol: USDC
"#
        );
        let registry_url =
            crate::test_helpers::mock_raindex_registry_url_with_settings(&settings).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load raindex config");
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let response_body = response.into_string().await.expect("response body");
        assert!(!response_body.contains(private_rpc));

        let body: serde_json::Value =
            serde_json::from_str(&response_body).expect("response is json");
        let tokens = body.as_array().expect("tokens is an array");
        let rpcs = tokens[0]["network"]["rpcs"]
            .as_array()
            .expect("network rpcs is an array");
        assert!(rpcs.is_empty());
    }

    #[rocket::async_test]
    async fn test_get_tokens_adds_name_and_isin_from_remote_tokens() {
        let settings = r#"version: 6
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
using-tokens-from:
  - __TOKENS_URL__
"#;
        let remote_tokens = r#"{
  "name": "ST0x Base Token List",
  "timestamp": "2026-03-20T00:00:00.000Z",
  "version": {
    "major": 1,
    "minor": 0,
    "patch": 0
  },
  "tokens": [
    {
      "chainId": 8453,
      "address": "0x8AFba81DEc38DE0A18E2Df5E1967a7493651eebf",
      "decimals": 18,
      "name": "Wrapped Circle Internet Group Inc ST0x",
      "symbol": "wtCRCL",
      "logoURI": "https://tokens.st0x.com/images/CRCL.png",
      "extensions": {
        "category": "ST0x",
        "isin": "US1725731079"
      }
    }
  ]
}"#;
        let registry_url =
            mock_raindex_registry_url_with_settings_and_tokens(settings, remote_tokens).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load raindex config");
        let client = TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);
        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let tokens = body.as_array().expect("tokens is an array");
        assert_eq!(tokens.len(), 1);

        let first = &tokens[0];
        assert_eq!(
            first["label"],
            serde_json::Value::String("Wrapped Circle Internet Group Inc ST0x".to_string())
        );
        assert_eq!(
            first["name"],
            serde_json::Value::String("Wrapped Circle Internet Group Inc ST0x".to_string())
        );
        assert_eq!(
            first["isin"],
            serde_json::Value::String("US1725731079".to_string())
        );
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratios_returns_data_and_per_token_errors() {
        let rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let client = wrap_ratio_client(&rpc_url).await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/v1/tokens/wrap-ratio")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let data = body["data"].as_array().expect("data is an array");
        let errors = body["errors"].as_array().expect("errors is an array");
        assert_eq!(data.len(), 1);
        assert_eq!(errors.len(), 1);

        assert_eq!(data[0]["shareAddress"], format!("{WT_MSTR:#x}"));
        assert_eq!(data[0]["assetAddress"], format!("{T_MSTR:#x}"));
        assert_eq!(data[0]["assetsPerShare"], "1.0");
        assert_eq!(data[0]["blockNumber"], 123);
        assert_eq!(data[0]["blockTimestamp"], 456);
        assert!(data[0]["capturedAt"].as_str().is_some());

        assert_eq!(errors[0]["shareAddress"], format!("{WT_BAD:#x}"));
        assert_eq!(errors[0]["message"], "failed to read ERC4626 ratio");
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratios_batches_tokens_by_network() {
        let base_rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let polygon_rpc_url = mock_erc4626_batch_rpc(
            WT_SECOND,
            T_SECOND,
            18,
            18,
            U256::from(3_000_000_000_000_000_000u128),
        )
        .await;
        let client = wrap_ratio_multi_network_client(&base_rpc_url, &polygon_rpc_url).await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/v1/tokens/wrap-ratio")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let data = body["data"].as_array().expect("data is an array");
        let errors = body["errors"].as_array().expect("errors is an array");
        assert_eq!(data.len(), 2);
        assert!(errors.is_empty());

        assert!(data
            .iter()
            .any(|row| row["shareAddress"] == format!("{WT_MSTR:#x}")
                && row["assetAddress"] == format!("{T_MSTR:#x}")
                && row["assetsPerShare"] == "1.0"));
        assert!(data
            .iter()
            .any(|row| row["shareAddress"] == format!("{WT_SECOND:#x}")
                && row["assetAddress"] == format!("{T_SECOND:#x}")
                && row["assetsPerShare"] == "3"));
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratio_by_address_returns_row() {
        let rpc_url = mock_erc4626_batch_rpc(
            WT_MSTR,
            T_MSTR,
            18,
            18,
            U256::from(2_000_000_000_000_000_000u128),
        )
        .await;
        let client = wrap_ratio_client(&rpc_url).await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get(format!("/v1/tokens/wrap-ratio/{WT_MSTR:#x}"))
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["shareAddress"], format!("{WT_MSTR:#x}"));
        assert_eq!(body["assetAddress"], format!("{T_MSTR:#x}"));
        assert_eq!(body["assetsPerShare"], "2");
        assert_eq!(body["blockNumber"], 123);
        assert_eq!(body["blockTimestamp"], 456);
        assert!(body["capturedAt"].as_str().is_some());
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratio_by_address_rejects_symbol_lookup() {
        let rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let client = wrap_ratio_client(&rpc_url).await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/v1/tokens/wrap-ratio/wtMSTR")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::UnprocessableEntity);
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratio_by_address_returns_not_found_for_non_st0x_token() {
        let rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let client = wrap_ratio_client(&rpc_url).await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/v1/tokens/wrap-ratio/0x4200000000000000000000000000000000000006")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::NotFound);
    }
}
