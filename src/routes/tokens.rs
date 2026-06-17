use crate::auth::AuthenticatedKey;
use crate::db::wrapped_exchange_rate_history::{
    count_wrapped_exchange_rate_snapshots_for_share,
    list_wrapped_exchange_rate_snapshots_for_share, WrappedExchangeRateSnapshot,
};
use crate::db::DbPool;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::SharedRaindexProvider;
use crate::types::common::ValidatedAddress;
use crate::wrap_ratio::{
    build_wrap_ratio_response, find_wrap_ratio_item, is_st0x_token,
    persist_wrap_ratio_snapshots_best_effort, read_wrap_ratios_batch, unwrapped_address,
    WrapRatioBatchInput, WrapRatioMetadata, WrapRatioResponse,
};
use alloy::primitives::Address;
use rain_orderbook_app_settings::token::TokenCfg;
use rain_orderbook_common::raindex_client::RaindexError;
use rocket::form::FromForm;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::Instrument;
use utoipa::IntoParams;

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

#[derive(Debug, Clone, FromForm, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct WrapRatioHistoryParams {
    #[field(name = "page")]
    #[param(example = 1)]
    page: Option<u32>,
    #[field(name = "pageSize")]
    #[param(example = 20)]
    page_size: Option<u32>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WrapRatioHistoryResponse {
    #[schema(value_type = String, example = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2")]
    share_address: Address,
    #[schema(value_type = String, example = "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe")]
    asset_address: Address,
    events: Vec<WrapRatioHistorySnapshotEvent>,
    pagination: WrapRatioHistoryPagination,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WrapRatioHistorySnapshotEvent {
    #[serde(rename = "type")]
    #[schema(example = "snapshot")]
    event_type: String,
    #[schema(example = 123)]
    block_number: u64,
    #[schema(nullable = true, example = 456)]
    block_timestamp: Option<u64>,
    #[schema(example = "1.0027")]
    assets_per_share: String,
    #[schema(example = "1781506371")]
    captured_at: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WrapRatioHistoryPagination {
    #[schema(example = 1)]
    page: u32,
    #[schema(example = 20)]
    page_size: u32,
    #[schema(example = 42)]
    total_events: u64,
    #[schema(example = 3)]
    total_pages: u64,
    #[schema(example = true)]
    has_more: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenProofsResponse {
    #[schema(value_type = String, example = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2")]
    address: Address,
    metadata: Vec<TokenProofMetadata>,
    schemas: Vec<TokenProofSchema>,
    receipts: Vec<TokenProofReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenProofMetadata {
    id: String,
    meta: String,
    sender: String,
    subject: String,
    #[schema(example = "0x1234")]
    meta_hash: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct TokenProofSchema {
    id: String,
    information: String,
    timestamp: u64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct TokenProofReceipt {
    id: String,
    #[serde(rename = "receiptId")]
    receipt_id: String,
    #[serde(rename = "txHash")]
    tx_hash: String,
    #[serde(rename = "type")]
    receipt_type: TokenProofReceiptType,
    information: String,
    timestamp: u64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TokenProofReceiptType {
    Deposit,
    Withdraw,
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

fn normalize_address(address: Address) -> String {
    format!("{address:#x}").to_ascii_lowercase()
}

fn wrap_ratio_history_pagination_params(
    params: WrapRatioHistoryParams,
) -> Result<(u32, u32, u32), ApiError> {
    let page = params.page.unwrap_or(1);
    if page == 0 {
        return Err(ApiError::BadRequest("page must be greater than 0".into()));
    }

    let page_size = params.page_size.unwrap_or(20).min(100);
    if page_size == 0 {
        return Err(ApiError::BadRequest(
            "pageSize must be greater than 0".into(),
        ));
    }

    let offset = page
        .checked_sub(1)
        .and_then(|page_index| page_index.checked_mul(page_size))
        .ok_or_else(|| ApiError::BadRequest("pagination offset value too large".into()))?;

    Ok((page, page_size, offset))
}

fn build_wrap_ratio_history_pagination(
    total_events: u64,
    page: u32,
    page_size: u32,
) -> WrapRatioHistoryPagination {
    let total_pages = total_events.div_ceil(page_size as u64);
    WrapRatioHistoryPagination {
        page,
        page_size,
        total_events,
        total_pages,
        has_more: (page as u64) < total_pages,
    }
}

fn wrap_ratio_history_event_from_snapshot(
    snapshot: WrappedExchangeRateSnapshot,
) -> Result<WrapRatioHistorySnapshotEvent, ApiError> {
    Ok(WrapRatioHistorySnapshotEvent {
        event_type: "snapshot".to_string(),
        block_number: snapshot
            .block_number
            .try_into()
            .map_err(|_| ApiError::Internal("block number overflow".into()))?,
        block_timestamp: snapshot
            .block_timestamp
            .map(u64::try_from)
            .transpose()
            .map_err(|_| ApiError::Internal("block timestamp overflow".into()))?,
        assets_per_share: snapshot.assets_per_share,
        captured_at: snapshot.captured_at,
    })
}

fn extension_address(token: &TokenCfg, key: &str) -> Option<Address> {
    token
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get(key))
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Address>().ok())
}

fn matches_token_proof_address(token: &TokenCfg, address: Address) -> bool {
    token.address == address
        || extension_address(token, "unwrappedAddress") == Some(address)
        || extension_address(token, "legacyAddress") == Some(address)
}

fn resolve_proof_subgraph_urls(
    yaml: &rain_orderbook_app_settings::yaml::raindex::RaindexYaml,
    network_key: &str,
) -> Result<(String, String), ApiError> {
    let sft_subgraph = resolve_sft_subgraph_url(yaml, network_key)?;
    let metaboard = yaml.get_metaboard(network_key).map_err(|error| {
        tracing::error!(
            network_key,
            error = %error,
            "failed to resolve metadata subgraph"
        );
        ApiError::Internal("metadata subgraph is not configured".into())
    })?;
    tracing::info!(
        network_key,
        metaboard_key = network_key,
        "resolved metadata subgraph"
    );

    Ok((sft_subgraph, metaboard.url.to_string()))
}

fn resolve_sft_subgraph_url(
    yaml: &rain_orderbook_app_settings::yaml::raindex::RaindexYaml,
    network_key: &str,
) -> Result<String, ApiError> {
    // Convention: the SFT subgraph is configured separately from the raindex
    // subgraph as `subgraphs.sft-{network}` (for example, `sft-base`).
    let key = format!("sft-{network_key}");
    if let Ok(subgraph) = yaml.get_subgraph(&key) {
        tracing::info!(network_key, sft_subgraph_key = %key, "resolved SFT subgraph");
        return Ok(subgraph.url.to_string());
    }

    tracing::error!(network_key, sft_subgraph_key = %key, "failed to resolve SFT subgraph");
    Err(ApiError::Internal("SFT subgraph is not configured".into()))
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TimestampValue {
    Number(u64),
    String(String),
}

impl TimestampValue {
    fn parse(&self) -> Result<u64, ApiError> {
        match self {
            TimestampValue::Number(value) => Ok(*value),
            TimestampValue::String(value) => value.parse::<u64>().map_err(|error| {
                tracing::error!(value, error = %error, "invalid timestamp from subgraph");
                ApiError::Internal("invalid timestamp from subgraph".into())
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    errors: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftProofsData {
    #[serde(default)]
    offchain_asset_receipt_vaults: Vec<SftVault>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftVault {
    #[serde(default)]
    receipt_vault_informations: Vec<SftReceiptVaultInformation>,
    #[serde(default)]
    deposits: Vec<SftReceiptEvent>,
    #[serde(default)]
    withdraws: Vec<SftReceiptEvent>,
}

#[derive(Debug, Deserialize)]
struct SftReceiptVaultInformation {
    id: String,
    information: String,
    timestamp: TimestampValue,
}

#[derive(Debug, Deserialize)]
struct SftReceiptEvent {
    timestamp: TimestampValue,
    transaction: Option<SftTransaction>,
    receipt: Option<SftReceipt>,
}

#[derive(Debug, Deserialize)]
struct SftTransaction {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftReceipt {
    receipt_id: String,
    #[serde(default)]
    receipt_informations: Vec<SftReceiptInformation>,
}

#[derive(Debug, Deserialize)]
struct SftReceiptInformation {
    id: String,
    information: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataProofsData {
    #[serde(default)]
    meta_v1_s: Vec<TokenProofMetadata>,
}

async fn post_graphql<T: for<'de> Deserialize<'de>>(
    url: &str,
    query: &str,
    variables: Value,
) -> Result<T, ApiError> {
    let response = reqwest::Client::new()
        .post(url)
        .json(&json!({
            "query": query,
            "variables": variables,
        }))
        .send()
        .await
        .map_err(|error| {
            tracing::error!(url, error = %error, "failed to query subgraph");
            ApiError::Internal("failed to query subgraph".into())
        })?;

    let status = response.status();
    if !status.is_success() {
        tracing::error!(
            url,
            status = status.as_u16(),
            "subgraph returned error status"
        );
        return Err(ApiError::Internal("subgraph returned error status".into()));
    }

    let body = response
        .json::<GraphqlResponse<T>>()
        .await
        .map_err(|error| {
            tracing::error!(url, error = %error, "failed to decode subgraph response");
            ApiError::Internal("failed to decode subgraph response".into())
        })?;

    if let Some(errors) = body.errors {
        tracing::error!(url, errors = %errors, "subgraph returned GraphQL errors");
        return Err(ApiError::Internal(
            "subgraph returned GraphQL errors".into(),
        ));
    }

    body.data.ok_or_else(|| {
        tracing::error!(url, "subgraph response missing data");
        ApiError::Internal("subgraph response missing data".into())
    })
}

fn build_token_proofs_response(
    address: Address,
    sft: SftProofsData,
    metadata: MetadataProofsData,
) -> Result<TokenProofsResponse, ApiError> {
    let Some(vault) = sft.offchain_asset_receipt_vaults.first() else {
        tracing::warn!(address = %address, "SFT vault not found for token");
        return Err(ApiError::NotFound("SFT vault not found for token".into()));
    };

    let schemas = vault
        .receipt_vault_informations
        .iter()
        .map(|schema| {
            Ok(TokenProofSchema {
                id: schema.id.clone(),
                information: schema.information.clone(),
                timestamp: schema.timestamp.parse()?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    let mut receipts = flatten_receipt_events(&vault.deposits, TokenProofReceiptType::Deposit)?;
    receipts.extend(flatten_receipt_events(
        &vault.withdraws,
        TokenProofReceiptType::Withdraw,
    )?);

    Ok(TokenProofsResponse {
        address,
        metadata: metadata.meta_v1_s,
        schemas,
        receipts,
    })
}

fn flatten_receipt_events(
    events: &[SftReceiptEvent],
    receipt_type: TokenProofReceiptType,
) -> Result<Vec<TokenProofReceipt>, ApiError> {
    let mut rows = Vec::new();

    for event in events {
        let timestamp = event.timestamp.parse()?;
        let Some(transaction) = event.transaction.as_ref() else {
            tracing::error!("receipt event missing transaction");
            return Err(ApiError::Internal(
                "receipt event missing transaction".into(),
            ));
        };
        let Some(receipt) = event.receipt.as_ref() else {
            tracing::error!("receipt event missing receipt");
            return Err(ApiError::Internal("receipt event missing receipt".into()));
        };

        for information in &receipt.receipt_informations {
            rows.push(TokenProofReceipt {
                id: information.id.clone(),
                receipt_id: receipt.receipt_id.clone(),
                tx_hash: transaction.id.clone(),
                receipt_type: receipt_type.clone(),
                information: information.information.clone(),
                timestamp,
            });
        }
    }

    Ok(rows)
}

async fn read_token_proofs(
    address: Address,
    sft_subgraph_url: &str,
    metadata_subgraph_url: &str,
) -> Result<TokenProofsResponse, ApiError> {
    const SFT_QUERY: &str = r#"
query TokenProofs($address: String!) {
  offchainAssetReceiptVaults(where: { wrappedTokenContractAddress: $address }) {
    id
    address
    receiptVaultInformations(orderBy: timestamp, orderDirection: desc) {
      id
      information
      timestamp
    }
    deposits {
      id
      timestamp
      transaction { id }
      receipt {
        id
        receiptId
        receiptInformations { information id }
      }
    }
    withdraws {
      id
      timestamp
      transaction { id }
      receipt {
        id
        receiptId
        receiptInformations { information id }
      }
    }
  }
}
"#;
    const METADATA_QUERY: &str = r#"
query TokenMetadata($subject: String!) {
  metaV1S(
    where: { subject: $subject },
    orderBy: transaction__timestamp,
    orderDirection: desc
  ) {
    id
    meta
    sender
    subject
    metaHash
  }
}
"#;

    let address_lower = format!("{address:#x}");
    let subject = format!(
        "0x000000000000000000000000{}",
        address_lower.trim_start_matches("0x")
    );

    let (sft, metadata) = tokio::try_join!(
        post_graphql::<SftProofsData>(
            sft_subgraph_url,
            SFT_QUERY,
            json!({ "address": address_lower }),
        ),
        post_graphql::<MetadataProofsData>(
            metadata_subgraph_url,
            METADATA_QUERY,
            json!({ "subject": subject }),
        ),
    )?;

    build_token_proofs_response(address, sft, metadata)
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
    pool: &State<DbPool>,
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

        persist_wrap_ratio_snapshots_best_effort(pool.inner(), &data).await;

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
    pool: &State<DbPool>,
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

        persist_wrap_ratio_snapshots_best_effort(pool.inner(), std::slice::from_ref(&response))
            .await;

        tracing::info!(share_address = %token.address, "returning wrapped token ratio");
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/tokens/wrap-ratio/{address}/history",
    tag = "Tokens",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Wrapped token / ERC4626 vault address"),
        WrapRatioHistoryParams
    ),
    responses(
        (status = 200, description = "Wrapped token ratio snapshot history", body = WrapRatioHistoryResponse),
        (status = 400, description = "Invalid pagination parameters", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "Wrapped ST0x token not found", body = ApiErrorResponse),
        (status = 422, description = "Invalid wrapped token address", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/wrap-ratio/<address>/history?<params..>")]
pub async fn get_wrap_ratio_history_by_address(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
    pool: &State<DbPool>,
    address: ValidatedAddress,
    params: WrapRatioHistoryParams,
) -> Result<Json<WrapRatioHistoryResponse>, ApiError> {
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

        let asset_address = unwrapped_address(token).map_err(|error| {
            tracing::error!(
                share_address = %token.address,
                error = %error,
                "failed to read wrapped token ratio history"
            );
            error.into_api_error()
        })?;

        let (page, page_size, offset) = wrap_ratio_history_pagination_params(params)?;
        let share_token_address = normalize_address(token.address);
        let total_events =
            count_wrapped_exchange_rate_snapshots_for_share(pool.inner(), &share_token_address)
                .await
                .map_err(|error| {
                    tracing::error!(
                        share_address = %token.address,
                        error = %error,
                        "failed to count wrapped token ratio history"
                    );
                    ApiError::Internal("failed to query wrapped token ratio history".into())
                })?;
        let snapshots = list_wrapped_exchange_rate_snapshots_for_share(
            pool.inner(),
            &share_token_address,
            page_size,
            offset,
        )
        .await
        .map_err(|error| {
            tracing::error!(
                share_address = %token.address,
                error = %error,
                "failed to list wrapped token ratio history"
            );
            ApiError::Internal("failed to query wrapped token ratio history".into())
        })?;

        let events = snapshots
            .into_iter()
            .map(wrap_ratio_history_event_from_snapshot)
            .collect::<Result<Vec<_>, ApiError>>()?;
        let pagination = build_wrap_ratio_history_pagination(total_events, page, page_size);

        tracing::info!(
            share_address = %token.address,
            event_count = events.len(),
            total_events,
            page,
            page_size,
            "returning wrapped token ratio history"
        );
        Ok(Json(WrapRatioHistoryResponse {
            share_address: token.address,
            asset_address,
            events,
            pagination,
        }))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/tokens/{address}/proofs",
    tag = "Tokens",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Wrapped, unwrapped, or legacy ST0x token address")
    ),
    responses(
        (status = 200, description = "Raw ST0x proof metadata, schemas, and receipts", body = TokenProofsResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "Wrapped ST0x token or SFT vault not found", body = ApiErrorResponse),
        (status = 422, description = "Invalid token address", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<address>/proofs", rank = 10)]
pub async fn get_token_proofs(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
    address: ValidatedAddress,
) -> Result<Json<TokenProofsResponse>, ApiError> {
    async move {
        tracing::info!(address = %address.0, "request received");

        let tokens = registry_tokens(shared_raindex).await?;
        let Some(token) = tokens
            .iter()
            .find(|token| is_st0x_token(token) && matches_token_proof_address(token, address.0))
        else {
            tracing::warn!(address = %address.0, "wrapped ST0x token not found");
            return Err(ApiError::NotFound("wrapped ST0x token not found".into()));
        };

        let (sft_subgraph_url, metadata_subgraph_url) = {
            let raindex = shared_raindex.read().await;
            resolve_proof_subgraph_urls(raindex.raindex_yaml(), &token.network.key)?
        };

        tracing::info!(
            requested_address = %address.0,
            wrapped_address = %token.address,
            network_key = %token.network.key,
            "querying token proofs"
        );

        let response =
            read_token_proofs(token.address, &sft_subgraph_url, &metadata_subgraph_url).await?;

        tracing::info!(
            wrapped_address = %token.address,
            metadata_count = response.metadata.len(),
            schema_count = response.schemas.len(),
            receipt_count = response.receipts.len(),
            "returning token proofs"
        );
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_tokens,
        get_wrap_ratios,
        get_wrap_ratio_by_address,
        get_wrap_ratio_history_by_address,
        get_token_proofs
    ]
}

#[cfg(test)]
mod tests {
    use crate::db::wrapped_exchange_rate_history::{
        insert_wrapped_exchange_rate_snapshots, NewWrappedExchangeRateSnapshot,
    };
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
    use std::sync::{Arc as StdArc, Mutex};

    const WT_MSTR: Address = address!("ff05e1bd696900dc6a52ca35ca61bb1024eda8e2");
    const T_MSTR: Address = address!("013b782f402d61aa1004cca95b9f5bb402c9d5fe");
    const WT_SECOND: Address = address!("3333333333333333333333333333333333333333");
    const T_SECOND: Address = address!("4444444444444444444444444444444444444444");
    const WT_BAD: Address = address!("1111111111111111111111111111111111111111");
    const T_BAD: Address = address!("2222222222222222222222222222222222222222");
    const WT_LEGACY: Address = address!("5555555555555555555555555555555555555555");

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

    fn history_snapshot(
        share_token_address: Address,
        asset_token_address: Address,
        assets_per_share: &str,
        block_number: i64,
        block_timestamp: Option<i64>,
        captured_at: &str,
    ) -> NewWrappedExchangeRateSnapshot {
        NewWrappedExchangeRateSnapshot {
            share_token_address: format!("{share_token_address:#x}"),
            asset_token_address: format!("{asset_token_address:#x}"),
            assets_per_share: assets_per_share.to_string(),
            block_number,
            block_timestamp,
            captured_at: captured_at.to_string(),
        }
    }

    async fn seed_history_snapshots(
        client: &rocket::local::asynchronous::Client,
        snapshots: &[NewWrappedExchangeRateSnapshot],
    ) {
        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        insert_wrapped_exchange_rate_snapshots(pool, snapshots)
            .await
            .expect("insert history snapshots");
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

    async fn mock_proofs_subgraphs(
        sft_body: serde_json::Value,
        metadata_body: serde_json::Value,
    ) -> (String, String) {
        let (sft_url, metadata_url, _) =
            mock_proofs_subgraphs_with_requests(sft_body, metadata_body).await;
        (sft_url, metadata_url)
    }

    async fn mock_proofs_subgraphs_with_requests(
        sft_body: serde_json::Value,
        metadata_body: serde_json::Value,
    ) -> (String, String, StdArc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock proofs subgraph");
        let addr = listener.local_addr().expect("mock proofs subgraph address");
        let requests = StdArc::new(Mutex::new(Vec::new()));
        let recorded_requests = requests.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };

                let sft_body = sft_body.clone();
                let metadata_body = metadata_body.clone();
                let requests = recorded_requests.clone();

                tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    let n = tokio::io::AsyncReadExt::read(&mut socket, &mut buf)
                        .await
                        .unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    requests
                        .lock()
                        .expect("mock proofs request lock")
                        .push(request.clone());
                    let body = if request.contains("/metadata") {
                        metadata_body
                    } else {
                        sft_body
                    }
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

        (
            format!("http://{addr}/sft"),
            format!("http://{addr}/metadata"),
            requests,
        )
    }

    async fn proofs_client(
        sft_url: &str,
        metadata_url: &str,
    ) -> rocket::local::asynchronous::Client {
        let settings = format!(
            r#"version: 6
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://example.com/raindex-subgraph
  sft-base: {sft_url}
metaboards:
  base: {metadata_url}
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
  "name": "ST0x Proof Token List",
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
        "unwrappedAddress": "{T_MSTR:#x}",
        "legacyAddress": "{WT_LEGACY:#x}"
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

    async fn authorized_get<'a>(
        client: &'a rocket::local::asynchronous::Client,
        path: String,
    ) -> rocket::local::asynchronous::LocalResponse<'a> {
        let (key_id, secret) = seed_api_key(client).await;
        let header = basic_auth_header(&key_id, &secret);
        client
            .get(path)
            .header(Header::new("Authorization", header))
            .dispatch()
            .await
    }

    fn proofs_sft_body() -> serde_json::Value {
        json!({
            "data": {
                "offchainAssetReceiptVaults": [{
                    "id": "vault-1",
                    "address": "0xvault",
                    "receiptVaultInformations": [{
                        "id": "schema-1",
                        "information": "0xRAWSCHEMA",
                        "timestamp": "101"
                    }],
                    "deposits": [{
                        "id": "deposit-1",
                        "timestamp": "201",
                        "transaction": { "id": "0xdeposit" },
                        "receipt": {
                            "id": "receipt-1",
                            "receiptId": "7",
                            "receiptInformations": [
                                { "id": "deposit-info-1", "information": "0xRAWDEPOSIT1" },
                                { "id": "deposit-info-2", "information": "0xRAWDEPOSIT2" }
                            ]
                        }
                    }],
                    "withdraws": [{
                        "id": "withdraw-1",
                        "timestamp": 202,
                        "transaction": { "id": "0xwithdraw" },
                        "receipt": {
                            "id": "receipt-2",
                            "receiptId": "8",
                            "receiptInformations": [
                                { "id": "withdraw-info-1", "information": "0xRAWWITHDRAW" }
                            ]
                        }
                    }]
                }]
            }
        })
    }

    fn proofs_metadata_body() -> serde_json::Value {
        json!({
            "data": {
                "metaV1S": [{
                    "id": "meta-1",
                    "meta": "0xRAWMETA",
                    "sender": "0xsender",
                    "subject": format!(
                        "0x000000000000000000000000{}",
                        format!("{WT_MSTR:#x}").trim_start_matches("0x")
                    ),
                    "metaHash": "0xmetahash"
                }]
            }
        })
    }

    fn graphql_request_for_path(requests: &[String], path: &str) -> serde_json::Value {
        let request = requests
            .iter()
            .find(|request| request.contains(path))
            .expect("mock GraphQL request for path");
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .expect("mock GraphQL request body");
        serde_json::from_str(body).expect("mock GraphQL request body is json")
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
    async fn test_get_token_proofs_rejects_invalid_address() {
        let (sft_url, metadata_url) =
            mock_proofs_subgraphs(proofs_sft_body(), proofs_metadata_body()).await;
        let client = proofs_client(&sft_url, &metadata_url).await;

        let response =
            authorized_get(&client, "/v1/tokens/not-an-address/proofs".to_string()).await;

        assert_eq!(response.status(), Status::UnprocessableEntity);
    }

    #[rocket::async_test]
    async fn test_get_token_proofs_returns_not_found_for_unsupported_token() {
        let (sft_url, metadata_url) =
            mock_proofs_subgraphs(proofs_sft_body(), proofs_metadata_body()).await;
        let client = proofs_client(&sft_url, &metadata_url).await;

        let response = authorized_get(
            &client,
            "/v1/tokens/0x4200000000000000000000000000000000000006/proofs".to_string(),
        )
        .await;

        assert_eq!(response.status(), Status::NotFound);
    }

    #[rocket::async_test]
    async fn test_get_token_proofs_resolves_wrapped_unwrapped_and_legacy_addresses() {
        let (sft_url, metadata_url) =
            mock_proofs_subgraphs(proofs_sft_body(), proofs_metadata_body()).await;
        let client = proofs_client(&sft_url, &metadata_url).await;

        for address in [WT_MSTR, T_MSTR, WT_LEGACY] {
            let response = authorized_get(&client, format!("/v1/tokens/{address:#x}/proofs")).await;
            assert_eq!(response.status(), Status::Ok);

            let body: serde_json::Value =
                serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
            assert_eq!(body["address"], format!("{WT_MSTR:#x}"));
        }
    }

    #[rocket::async_test]
    async fn test_get_token_proofs_queries_subgraphs_with_normalized_wrapped_address() {
        let (sft_url, metadata_url, requests) =
            mock_proofs_subgraphs_with_requests(proofs_sft_body(), proofs_metadata_body()).await;
        let client = proofs_client(&sft_url, &metadata_url).await;

        let response = authorized_get(&client, format!("/v1/tokens/{T_MSTR:#x}/proofs")).await;

        assert_eq!(response.status(), Status::Ok);
        let recorded = requests.lock().expect("mock requests").clone();
        let sft_request = graphql_request_for_path(&recorded, "/sft");
        let metadata_request = graphql_request_for_path(&recorded, "/metadata");
        let wrapped_address = format!("{WT_MSTR:#x}");
        let metadata_subject = format!(
            "0x000000000000000000000000{}",
            wrapped_address.trim_start_matches("0x")
        );

        assert_eq!(sft_request["variables"]["address"], wrapped_address);
        assert_eq!(metadata_request["variables"]["subject"], metadata_subject);
        assert!(sft_request["query"]
            .as_str()
            .expect("SFT query string")
            .contains("wrappedTokenContractAddress: $address"));
        assert!(metadata_request["query"]
            .as_str()
            .expect("metadata query string")
            .contains("subject: $subject"));
    }

    #[rocket::async_test]
    async fn test_get_token_proofs_returns_raw_metadata_schemas_and_flattened_receipts() {
        let (sft_url, metadata_url) =
            mock_proofs_subgraphs(proofs_sft_body(), proofs_metadata_body()).await;
        let client = proofs_client(&sft_url, &metadata_url).await;

        let response = authorized_get(&client, format!("/v1/tokens/{WT_MSTR:#x}/proofs")).await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["address"], format!("{WT_MSTR:#x}"));
        assert_eq!(body["metadata"][0]["meta"], "0xRAWMETA");
        assert_eq!(body["metadata"][0]["metaHash"], "0xmetahash");
        assert_eq!(body["schemas"][0]["id"], "schema-1");
        assert_eq!(body["schemas"][0]["information"], "0xRAWSCHEMA");
        assert_eq!(body["schemas"][0]["timestamp"], 101);

        let receipts = body["receipts"].as_array().expect("receipts is an array");
        assert_eq!(receipts.len(), 3);
        assert!(receipts.iter().any(|row| row["id"] == "deposit-info-1"
            && row["receiptId"] == "7"
            && row["txHash"] == "0xdeposit"
            && row["type"] == "deposit"
            && row["information"] == "0xRAWDEPOSIT1"
            && row["timestamp"] == 201));
        assert!(receipts.iter().any(|row| row["id"] == "deposit-info-2"
            && row["type"] == "deposit"
            && row["information"] == "0xRAWDEPOSIT2"));
        assert!(receipts.iter().any(|row| row["id"] == "withdraw-info-1"
            && row["receiptId"] == "8"
            && row["txHash"] == "0xwithdraw"
            && row["type"] == "withdraw"
            && row["information"] == "0xRAWWITHDRAW"
            && row["timestamp"] == 202));
    }

    #[rocket::async_test]
    async fn test_get_token_proofs_returns_empty_collections_when_vault_has_no_rows() {
        let sft_body = json!({
            "data": {
                "offchainAssetReceiptVaults": [{
                    "id": "vault-1",
                    "address": "0xvault",
                    "receiptVaultInformations": [],
                    "deposits": [],
                    "withdraws": []
                }]
            }
        });
        let metadata_body = json!({ "data": { "metaV1S": [] } });
        let (sft_url, metadata_url) = mock_proofs_subgraphs(sft_body, metadata_body).await;
        let client = proofs_client(&sft_url, &metadata_url).await;

        let response = authorized_get(&client, format!("/v1/tokens/{WT_MSTR:#x}/proofs")).await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["metadata"].as_array().unwrap().len(), 0);
        assert_eq!(body["schemas"].as_array().unwrap().len(), 0);
        assert_eq!(body["receipts"].as_array().unwrap().len(), 0);
    }

    #[rocket::async_test]
    async fn test_get_token_proofs_returns_not_found_when_sft_vault_is_missing() {
        let sft_body = json!({ "data": { "offchainAssetReceiptVaults": [] } });
        let (sft_url, metadata_url) = mock_proofs_subgraphs(sft_body, proofs_metadata_body()).await;
        let client = proofs_client(&sft_url, &metadata_url).await;

        let response = authorized_get(&client, format!("/v1/tokens/{WT_MSTR:#x}/proofs")).await;

        assert_eq!(response.status(), Status::NotFound);
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

        let pool = client
            .rocket()
            .state::<crate::db::DbPool>()
            .expect("pool in state");
        let persisted = sqlx::query_as::<_, (String, String, String, i64, Option<i64>)>(
            "SELECT share_token_address, asset_token_address, assets_per_share, block_number, block_timestamp \
             FROM wrapped_exchange_rate_snapshots \
             WHERE share_token_address = ?",
        )
        .bind(format!("{WT_MSTR:#x}"))
        .fetch_one(pool)
        .await
        .expect("read persisted snapshot");
        assert_eq!(persisted.0, format!("{WT_MSTR:#x}"));
        assert_eq!(persisted.1, format!("{T_MSTR:#x}"));
        assert_eq!(persisted.2, "1.0");
        assert_eq!(persisted.3, 123);
        assert_eq!(persisted.4, Some(456));

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
    async fn test_get_wrap_ratio_history_by_address_returns_snapshot_events() {
        let rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let client = wrap_ratio_client(&rpc_url).await;
        seed_history_snapshots(
            &client,
            &[
                history_snapshot(WT_MSTR, T_MSTR, "1.0", 100, Some(1100), "1781506300"),
                history_snapshot(WT_MSTR, T_MSTR, "1.1", 101, Some(1101), "1781506301"),
                history_snapshot(WT_MSTR, T_MSTR, "1.2", 102, Some(1102), "1781506302"),
                history_snapshot(WT_SECOND, T_SECOND, "9.9", 103, Some(1103), "1781506303"),
            ],
        )
        .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get(format!(
                "/v1/tokens/wrap-ratio/{WT_MSTR:#x}/history?page=1&pageSize=2"
            ))
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["shareAddress"], format!("{WT_MSTR:#x}"));
        assert_eq!(body["assetAddress"], format!("{T_MSTR:#x}"));

        let events = body["events"].as_array().expect("events is an array");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "snapshot");
        assert_eq!(events[0]["blockNumber"], 102);
        assert_eq!(events[0]["blockTimestamp"], 1102);
        assert_eq!(events[0]["assetsPerShare"], "1.2");
        assert_eq!(events[0]["capturedAt"], "1781506302");
        assert_eq!(events[1]["type"], "snapshot");
        assert_eq!(events[1]["blockNumber"], 101);

        assert_eq!(body["pagination"]["page"], 1);
        assert_eq!(body["pagination"]["pageSize"], 2);
        assert_eq!(body["pagination"]["totalEvents"], 3);
        assert_eq!(body["pagination"]["totalPages"], 2);
        assert_eq!(body["pagination"]["hasMore"], true);
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratio_history_by_address_returns_empty_history_for_valid_token() {
        let rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let client = wrap_ratio_client(&rpc_url).await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get(format!("/v1/tokens/wrap-ratio/{WT_MSTR:#x}/history"))
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        assert_eq!(body["shareAddress"], format!("{WT_MSTR:#x}"));
        assert_eq!(body["assetAddress"], format!("{T_MSTR:#x}"));
        assert!(body["events"]
            .as_array()
            .expect("events is an array")
            .is_empty());
        assert_eq!(body["pagination"]["page"], 1);
        assert_eq!(body["pagination"]["pageSize"], 20);
        assert_eq!(body["pagination"]["totalEvents"], 0);
        assert_eq!(body["pagination"]["totalPages"], 0);
        assert_eq!(body["pagination"]["hasMore"], false);
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratio_history_by_address_paginates_and_caps_page_size() {
        let rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let client = wrap_ratio_client(&rpc_url).await;
        seed_history_snapshots(
            &client,
            &[
                history_snapshot(WT_MSTR, T_MSTR, "1.0", 100, Some(1100), "1781506300"),
                history_snapshot(WT_MSTR, T_MSTR, "1.1", 101, Some(1101), "1781506301"),
                history_snapshot(WT_MSTR, T_MSTR, "1.2", 102, Some(1102), "1781506302"),
            ],
        )
        .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get(format!(
                "/v1/tokens/wrap-ratio/{WT_MSTR:#x}/history?page=2&pageSize=2"
            ))
            .header(Header::new("Authorization", header.clone()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value =
            serde_json::from_str(&response.into_string().await.unwrap()).unwrap();
        let events = body["events"].as_array().expect("events is an array");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["blockNumber"], 100);
        assert_eq!(body["pagination"]["page"], 2);
        assert_eq!(body["pagination"]["pageSize"], 2);
        assert_eq!(body["pagination"]["totalEvents"], 3);
        assert_eq!(body["pagination"]["totalPages"], 2);
        assert_eq!(body["pagination"]["hasMore"], false);

        let capped_response = client
            .get(format!(
                "/v1/tokens/wrap-ratio/{WT_MSTR:#x}/history?pageSize=500"
            ))
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(capped_response.status(), Status::Ok);
        let capped_body: serde_json::Value =
            serde_json::from_str(&capped_response.into_string().await.unwrap()).unwrap();
        assert_eq!(capped_body["pagination"]["pageSize"], 100);
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratio_history_by_address_rejects_symbol_lookup() {
        let rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let client = wrap_ratio_client(&rpc_url).await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/v1/tokens/wrap-ratio/wtMSTR/history")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::UnprocessableEntity);
    }

    #[rocket::async_test]
    async fn test_get_wrap_ratio_history_by_address_returns_not_found_for_unsupported_address() {
        let rpc_url =
            mock_erc4626_batch_rpc(WT_MSTR, T_MSTR, 18, 18, U256::from(10).pow(U256::from(18)))
                .await;
        let client = wrap_ratio_client(&rpc_url).await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        for path in [
            "/v1/tokens/wrap-ratio/0x4200000000000000000000000000000000000006/history".to_string(),
            "/v1/tokens/wrap-ratio/0x9999999999999999999999999999999999999999/history".to_string(),
        ] {
            let response = client
                .get(path)
                .header(Header::new("Authorization", header.clone()))
                .dispatch()
                .await;

            assert_eq!(response.status(), Status::NotFound);
        }
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
