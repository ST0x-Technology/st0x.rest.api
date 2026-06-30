use super::{
    api_error_message, matches_token_proof_address, post_graphql, registry_tokens,
    resolve_sft_subgraph_url, TimestampValue, SFT_PAGE_SIZE,
};
use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::raindex::SharedRaindexProvider;
use crate::types::common::ValidatedAddress;
use crate::wrap_ratio::is_st0x_token;
use alloy::primitives::{Address, U256};
use moka::future::Cache;
use rain_orderbook_app_settings::token::TokenCfg;
use rocket::form::FromForm;
use rocket::serde::json::Json;
use rocket::State;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::Instrument;

const DEFAULT_ACTIVITY_LIMIT: u32 = 5;
const MAX_ACTIVITY_LIMIT: u32 = 50;
const TOKEN_DETAILS_AGGREGATE_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const TOKEN_DETAILS_AGGREGATE_CACHE_CAPACITY: u64 = 512;
const TOKEN_DETAILS_LIST_CACHE_CAPACITY: u64 = 64;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetailsListResponse {
    data: Vec<TokenDetailsSummaryResponse>,
    errors: Vec<TokenDetailsErrorResponse>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetailsErrorResponse {
    #[schema(value_type = String, example = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2")]
    address: Address,
    #[schema(example = "SFT vault not found for token")]
    message: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetailsSummaryResponse {
    #[schema(value_type = String, example = "0xff05e1bd696900dc6a52ca35ca61bb1024eda8e2")]
    address: Address,
    #[schema(value_type = Option<String>, example = "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe")]
    receipt_contract_address: Option<Address>,
    name: String,
    symbol: String,
    decimals: u8,
    #[schema(example = "123456000000000000000")]
    total_supply: String,
    holder_count: u64,
    transfer_count: u64,
    #[schema(example = "123456000000000000000")]
    bridged_supply: String,
    #[schema(example = "200000000000000000000")]
    deposit_volume: String,
    #[schema(example = "76544000000000000000")]
    withdraw_volume: String,
    #[schema(example = "276544000000000000000")]
    activity_volume: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetailsResponse {
    #[serde(flatten)]
    summary: TokenDetailsSummaryResponse,
    #[schema(value_type = String, example = "0x013b782f402d61aa1004cca95b9f5bb402c9d5fe")]
    sft_vault_address: String,
    deploy_timestamp: u64,
    #[schema(value_type = String, example = "0x1c66d6708914c40239d54919320b4c48cae3d1a9")]
    deployer: Address,
    #[schema(value_type = String, example = "0x1c66d6708914c40239d54919320b4c48cae3d1a9")]
    admin: Address,
    activity: TokenDetailsActivityResponse,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetailsActivityResponse {
    deposits: Vec<TokenDetailsReceiptActivity>,
    withdraws: Vec<TokenDetailsReceiptActivity>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetailsReceiptActivity {
    id: String,
    #[schema(example = "0xabc123")]
    tx_hash: String,
    #[schema(value_type = String, example = "0x1c66d6708914c40239d54919320b4c48cae3d1a9")]
    caller: Address,
    #[schema(example = "1000000000000000000")]
    amount: String,
    timestamp: u64,
    receipt_id: String,
}

#[derive(Debug, Default, FromForm, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TokenDetailsQueryParams {
    #[param(rename = "activityLimit", example = 5, minimum = 1, maximum = 50)]
    #[field(name = "activityLimit")]
    activity_limit: Option<u32>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftTokenDetailsData {
    #[serde(default)]
    offchain_asset_receipt_vaults: Vec<SftTokenDetailsVault>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftTokenDetailsVault {
    id: String,
    address: String,
    receipt_contract_address: Option<Address>,
    total_shares: String,
    deploy_timestamp: TimestampValue,
    deployer: Address,
    admin: Address,
    name: String,
    symbol: String,
    #[serde(default)]
    deposits: Vec<SftReceiptActivity>,
    #[serde(default)]
    withdraws: Vec<SftReceiptActivity>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftTokenDetailsListData {
    #[serde(default)]
    offchain_asset_receipt_vaults: Vec<SftTokenDetailsSummaryVault>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftTokenDetailsSummaryVault {
    receipt_contract_address: Option<Address>,
    wrapped_token_contract_address: Option<Address>,
    total_shares: String,
    name: String,
    symbol: String,
    token_holder_count: String,
    share_transfer_count: String,
    deposit_volume: String,
    withdraw_volume: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftTokenHolderPageData {
    #[serde(default)]
    token_holders: Vec<SftCountRow>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftShareTransferPageData {
    #[serde(default)]
    shares_transfers: Vec<SftCountRow>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftDepositPageData {
    #[serde(default)]
    deposit_with_receipts: Vec<SftAmountRow>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftWithdrawPageData {
    #[serde(default)]
    withdraw_with_receipts: Vec<SftAmountRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct SftCountRow {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SftAmountRow {
    id: String,
    amount: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SftReceiptActivity {
    id: String,
    amount: String,
    timestamp: TimestampValue,
    transaction: Option<SftTransactionRef>,
    caller: Option<SftAddressRef>,
    receipt: Option<SftReceiptId>,
}

#[derive(Debug, Clone, Deserialize)]
struct SftTransactionRef {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SftAddressRef {
    address: Address,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SftReceiptId {
    receipt_id: String,
}

#[derive(Debug, Clone)]
struct TokenDetailsAggregate {
    holder_count: u64,
    transfer_count: u64,
    deposit_volume: U256,
    withdraw_volume: U256,
}

struct TokenDetailsBatchItem {
    token: TokenCfg,
    sft_subgraph_url: Result<String, ApiError>,
}
fn token_details_aggregate_cache() -> &'static Cache<String, TokenDetailsAggregate> {
    static CACHE: OnceLock<Cache<String, TokenDetailsAggregate>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Cache::builder()
            .max_capacity(TOKEN_DETAILS_AGGREGATE_CACHE_CAPACITY)
            .time_to_live(TOKEN_DETAILS_AGGREGATE_CACHE_TTL)
            .build()
    })
}

fn token_details_list_cache() -> &'static Cache<String, TokenDetailsListResponse> {
    static CACHE: OnceLock<Cache<String, TokenDetailsListResponse>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Cache::builder()
            .max_capacity(TOKEN_DETAILS_LIST_CACHE_CAPACITY)
            .time_to_live(TOKEN_DETAILS_AGGREGATE_CACHE_TTL)
            .build()
    })
}

#[cfg(test)]
pub(super) fn clear_token_details_aggregate_cache() {
    token_details_aggregate_cache().invalidate_all();
    token_details_list_cache().invalidate_all();
}

fn token_details_cache_key(sft_subgraph_url: &str, address: Address) -> String {
    format!("token-details:{sft_subgraph_url}:{address:#x}")
}

fn token_details_list_cache_key(items: &[TokenDetailsBatchItem]) -> String {
    let mut parts = items
        .iter()
        .map(|item| {
            let sft_key = match &item.sft_subgraph_url {
                Ok(url) => url.clone(),
                Err(error) => api_error_message(error),
            };
            format!(
                "{}:{:#x}:{}",
                item.token.network.key, item.token.address, sft_key
            )
        })
        .collect::<Vec<_>>();
    parts.sort_unstable();

    let mut key = String::from("token-details-list");
    for part in parts {
        key.push('|');
        key.push_str(&part);
    }
    key
}

fn parse_u256_decimal(value: &str, field: &str) -> Result<U256, ApiError> {
    U256::from_str(value).map_err(|error| {
        tracing::error!(field, value, error = %error, "invalid decimal integer from subgraph");
        ApiError::Internal("invalid amount from subgraph".into())
    })
}

fn parse_u64_decimal(value: &str, field: &str) -> Result<u64, ApiError> {
    value.parse::<u64>().map_err(|error| {
        tracing::error!(field, value, error = %error, "invalid count from subgraph");
        ApiError::Internal("invalid count from subgraph".into())
    })
}

fn add_amount(total: &mut U256, value: &str, field: &str) -> Result<(), ApiError> {
    let amount = parse_u256_decimal(value, field)?;
    *total = total.checked_add(amount).ok_or_else(|| {
        tracing::error!(field, "amount overflow while aggregating token details");
        ApiError::Internal("amount overflow while aggregating token details".into())
    })?;
    Ok(())
}

async fn count_token_holders(sft_subgraph_url: &str, sft_vault_id: &str) -> Result<u64, ApiError> {
    const QUERY: &str = r#"
query TokenHolderPage($vaultId: String!, $first: Int!, $lastId: String!) {
  tokenHolders(
    first: $first,
    orderBy: id,
    orderDirection: asc,
    where: { offchainAssetReceiptVault: $vaultId, balance_gt: "0", id_gt: $lastId }
  ) {
    id
  }
}
"#;

    let mut last_id = String::new();
    let mut total = 0u64;
    loop {
        let page = post_graphql::<SftTokenHolderPageData>(
            sft_subgraph_url,
            QUERY,
            json!({ "vaultId": sft_vault_id, "first": SFT_PAGE_SIZE, "lastId": last_id }),
        )
        .await?;
        let page_len = page.token_holders.len();
        let next_last_id = page.token_holders.last().map(|row| row.id.clone());
        total = total.checked_add(page_len as u64).ok_or_else(|| {
            tracing::error!("token holder count overflow");
            ApiError::Internal("token holder count overflow".into())
        })?;

        if page_len < SFT_PAGE_SIZE {
            return Ok(total);
        }
        last_id = next_last_id.ok_or_else(|| {
            tracing::error!("token holder page missing cursor row");
            ApiError::Internal("token holder page missing cursor row".into())
        })?;
    }
}

async fn count_share_transfers(
    sft_subgraph_url: &str,
    sft_vault_id: &str,
) -> Result<u64, ApiError> {
    const QUERY: &str = r#"
query ShareTransferPage($vaultId: String!, $first: Int!, $lastId: String!) {
  sharesTransfers(
    first: $first,
    orderBy: id,
    orderDirection: asc,
    where: { offchainAssetReceiptVault: $vaultId, id_gt: $lastId }
  ) {
    id
  }
}
"#;

    let mut last_id = String::new();
    let mut total = 0u64;
    loop {
        let page = post_graphql::<SftShareTransferPageData>(
            sft_subgraph_url,
            QUERY,
            json!({ "vaultId": sft_vault_id, "first": SFT_PAGE_SIZE, "lastId": last_id }),
        )
        .await?;
        let page_len = page.shares_transfers.len();
        let next_last_id = page.shares_transfers.last().map(|row| row.id.clone());
        total = total.checked_add(page_len as u64).ok_or_else(|| {
            tracing::error!("share transfer count overflow");
            ApiError::Internal("share transfer count overflow".into())
        })?;

        if page_len < SFT_PAGE_SIZE {
            return Ok(total);
        }
        last_id = next_last_id.ok_or_else(|| {
            tracing::error!("share transfer page missing cursor row");
            ApiError::Internal("share transfer page missing cursor row".into())
        })?;
    }
}

async fn sum_deposit_volume(sft_subgraph_url: &str, sft_vault_id: &str) -> Result<U256, ApiError> {
    const QUERY: &str = r#"
query DepositPage($vaultId: String!, $first: Int!, $lastId: String!) {
  depositWithReceipts(
    first: $first,
    orderBy: id,
    orderDirection: asc,
    where: { offchainAssetReceiptVault: $vaultId, id_gt: $lastId }
  ) {
    id
    amount
  }
}
"#;

    let mut last_id = String::new();
    let mut total = U256::ZERO;
    loop {
        let page = post_graphql::<SftDepositPageData>(
            sft_subgraph_url,
            QUERY,
            json!({ "vaultId": sft_vault_id, "first": SFT_PAGE_SIZE, "lastId": last_id }),
        )
        .await?;
        let page_len = page.deposit_with_receipts.len();
        let next_last_id = page.deposit_with_receipts.last().map(|row| row.id.clone());
        for row in &page.deposit_with_receipts {
            add_amount(&mut total, &row.amount, "deposit.amount")?;
        }

        if page_len < SFT_PAGE_SIZE {
            return Ok(total);
        }
        last_id = next_last_id.ok_or_else(|| {
            tracing::error!("deposit page missing cursor row");
            ApiError::Internal("deposit page missing cursor row".into())
        })?;
    }
}

async fn sum_withdraw_volume(sft_subgraph_url: &str, sft_vault_id: &str) -> Result<U256, ApiError> {
    const QUERY: &str = r#"
query WithdrawPage($vaultId: String!, $first: Int!, $lastId: String!) {
  withdrawWithReceipts(
    first: $first,
    orderBy: id,
    orderDirection: asc,
    where: { offchainAssetReceiptVault: $vaultId, id_gt: $lastId }
  ) {
    id
    amount
  }
}
"#;

    let mut last_id = String::new();
    let mut total = U256::ZERO;
    loop {
        let page = post_graphql::<SftWithdrawPageData>(
            sft_subgraph_url,
            QUERY,
            json!({ "vaultId": sft_vault_id, "first": SFT_PAGE_SIZE, "lastId": last_id }),
        )
        .await?;
        let page_len = page.withdraw_with_receipts.len();
        let next_last_id = page.withdraw_with_receipts.last().map(|row| row.id.clone());
        for row in &page.withdraw_with_receipts {
            add_amount(&mut total, &row.amount, "withdraw.amount")?;
        }

        if page_len < SFT_PAGE_SIZE {
            return Ok(total);
        }
        last_id = next_last_id.ok_or_else(|| {
            tracing::error!("withdraw page missing cursor row");
            ApiError::Internal("withdraw page missing cursor row".into())
        })?;
    }
}

async fn read_token_details_aggregate_uncached(
    sft_subgraph_url: &str,
    sft_vault_id: &str,
) -> Result<TokenDetailsAggregate, ApiError> {
    let (holder_count, transfer_count, deposit_volume, withdraw_volume) = tokio::try_join!(
        count_token_holders(sft_subgraph_url, sft_vault_id),
        count_share_transfers(sft_subgraph_url, sft_vault_id),
        sum_deposit_volume(sft_subgraph_url, sft_vault_id),
        sum_withdraw_volume(sft_subgraph_url, sft_vault_id),
    )?;

    Ok(TokenDetailsAggregate {
        holder_count,
        transfer_count,
        deposit_volume,
        withdraw_volume,
    })
}

async fn read_token_details_aggregate(
    sft_subgraph_url: &str,
    wrapped_address: Address,
    sft_vault_id: &str,
) -> Result<TokenDetailsAggregate, ApiError> {
    let cache_key = token_details_cache_key(sft_subgraph_url, wrapped_address);
    token_details_aggregate_cache()
        .try_get_with(cache_key, async move {
            read_token_details_aggregate_uncached(sft_subgraph_url, sft_vault_id).await
        })
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "failed to read token details aggregate");
            error.as_ref().clone()
        })
}

fn checked_sub_u256(lhs: U256, rhs: U256, field: &str) -> Result<U256, ApiError> {
    lhs.checked_sub(rhs).ok_or_else(|| {
        tracing::error!(field, "amount underflow while building token details");
        ApiError::Internal("amount underflow while building token details".into())
    })
}

fn checked_add_u256(lhs: U256, rhs: U256, field: &str) -> Result<U256, ApiError> {
    lhs.checked_add(rhs).ok_or_else(|| {
        tracing::error!(field, "amount overflow while building token details");
        ApiError::Internal("amount overflow while building token details".into())
    })
}

fn token_name(token: &TokenCfg, fallback: &str) -> String {
    token
        .label
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn token_symbol(token: &TokenCfg, fallback: &str) -> String {
    token
        .symbol
        .clone()
        .filter(|symbol| !symbol.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn build_token_details_summary(
    token: &TokenCfg,
    vault: &SftTokenDetailsVault,
    aggregate: &TokenDetailsAggregate,
) -> Result<TokenDetailsSummaryResponse, ApiError> {
    build_token_details_summary_fields(
        token,
        vault.receipt_contract_address,
        &vault.name,
        &vault.symbol,
        &vault.total_shares,
        aggregate,
    )
}

fn build_token_details_summary_fields(
    token: &TokenCfg,
    receipt_contract_address: Option<Address>,
    vault_name: &str,
    vault_symbol: &str,
    total_shares: &str,
    aggregate: &TokenDetailsAggregate,
) -> Result<TokenDetailsSummaryResponse, ApiError> {
    let bridged_supply = checked_sub_u256(
        aggregate.deposit_volume,
        aggregate.withdraw_volume,
        "bridgedSupply",
    )?;
    let activity_volume = checked_add_u256(
        aggregate.deposit_volume,
        aggregate.withdraw_volume,
        "activityVolume",
    )?;

    Ok(TokenDetailsSummaryResponse {
        address: token.address,
        receipt_contract_address,
        name: token_name(token, vault_name),
        symbol: token_symbol(token, vault_symbol),
        decimals: token.decimals.unwrap_or(18),
        total_supply: total_shares.to_string(),
        holder_count: aggregate.holder_count,
        transfer_count: aggregate.transfer_count,
        bridged_supply: bridged_supply.to_string(),
        deposit_volume: aggregate.deposit_volume.to_string(),
        withdraw_volume: aggregate.withdraw_volume.to_string(),
        activity_volume: activity_volume.to_string(),
    })
}

fn aggregate_from_summary_vault(
    vault: &SftTokenDetailsSummaryVault,
) -> Result<TokenDetailsAggregate, ApiError> {
    Ok(TokenDetailsAggregate {
        holder_count: parse_u64_decimal(&vault.token_holder_count, "tokenHolderCount")?,
        transfer_count: parse_u64_decimal(&vault.share_transfer_count, "shareTransferCount")?,
        deposit_volume: parse_u256_decimal(&vault.deposit_volume, "depositVolume")?,
        withdraw_volume: parse_u256_decimal(&vault.withdraw_volume, "withdrawVolume")?,
    })
}

fn build_token_details_list_summary_response(
    token: &TokenCfg,
    vault: &SftTokenDetailsSummaryVault,
) -> Result<TokenDetailsSummaryResponse, ApiError> {
    let aggregate = aggregate_from_summary_vault(vault)?;
    build_token_details_summary_fields(
        token,
        vault.receipt_contract_address,
        &vault.name,
        &vault.symbol,
        &vault.total_shares,
        &aggregate,
    )
}

fn build_receipt_activity(
    event: &SftReceiptActivity,
) -> Result<TokenDetailsReceiptActivity, ApiError> {
    let transaction = event.transaction.as_ref().ok_or_else(|| {
        tracing::error!(activity_id = %event.id, "receipt activity missing transaction");
        ApiError::Internal("receipt activity missing transaction".into())
    })?;
    let caller = event.caller.as_ref().ok_or_else(|| {
        tracing::error!(activity_id = %event.id, "receipt activity missing caller");
        ApiError::Internal("receipt activity missing caller".into())
    })?;
    let receipt = event.receipt.as_ref().ok_or_else(|| {
        tracing::error!(activity_id = %event.id, "receipt activity missing receipt");
        ApiError::Internal("receipt activity missing receipt".into())
    })?;

    Ok(TokenDetailsReceiptActivity {
        id: event.id.clone(),
        tx_hash: transaction.id.clone(),
        caller: caller.address,
        amount: event.amount.clone(),
        timestamp: event.timestamp.parse()?,
        receipt_id: receipt.receipt_id.clone(),
    })
}

fn build_token_details_response(
    token: &TokenCfg,
    vault: SftTokenDetailsVault,
    aggregate: TokenDetailsAggregate,
) -> Result<TokenDetailsResponse, ApiError> {
    let summary = build_token_details_summary(token, &vault, &aggregate)?;
    let deposits = vault
        .deposits
        .iter()
        .map(build_receipt_activity)
        .collect::<Result<Vec<_>, ApiError>>()?;
    let withdraws = vault
        .withdraws
        .iter()
        .map(build_receipt_activity)
        .collect::<Result<Vec<_>, ApiError>>()?;

    Ok(TokenDetailsResponse {
        summary,
        sft_vault_address: vault.address,
        deploy_timestamp: vault.deploy_timestamp.parse()?,
        deployer: vault.deployer,
        admin: vault.admin,
        activity: TokenDetailsActivityResponse {
            deposits,
            withdraws,
        },
    })
}

async fn read_sft_token_details_vault(
    address: Address,
    sft_subgraph_url: &str,
    activity_limit: u32,
) -> Result<SftTokenDetailsVault, ApiError> {
    const QUERY: &str = r#"
query TokenDetails($address: String!, $activityLimit: Int!) {
  offchainAssetReceiptVaults(where: { wrappedTokenContractAddress: $address }) {
    id
    address
    receiptContractAddress
    totalShares
    deployTimestamp
    deployer
    admin
    name
    symbol
    deposits(first: $activityLimit, orderBy: timestamp, orderDirection: desc) {
      id
      amount
      timestamp
      transaction { id }
      caller { address }
      receipt { receiptId }
    }
    withdraws(first: $activityLimit, orderBy: timestamp, orderDirection: desc) {
      id
      amount
      timestamp
      transaction { id }
      caller { address }
      receipt { receiptId }
    }
  }
}
"#;

    let address_lower = format!("{address:#x}");
    let data = post_graphql::<SftTokenDetailsData>(
        sft_subgraph_url,
        QUERY,
        json!({
            "address": address_lower,
            "activityLimit": activity_limit,
        }),
    )
    .await?;

    data.offchain_asset_receipt_vaults
        .into_iter()
        .next()
        .ok_or_else(|| {
            tracing::warn!(address = %address, "SFT vault not found for token details");
            ApiError::NotFound("SFT vault not found for token".into())
        })
}

async fn read_token_details_response(
    token: &TokenCfg,
    sft_subgraph_url: &str,
    activity_limit: u32,
) -> Result<TokenDetailsResponse, ApiError> {
    let vault =
        read_sft_token_details_vault(token.address, sft_subgraph_url, activity_limit).await?;
    let aggregate =
        read_token_details_aggregate(sft_subgraph_url, token.address, &vault.id).await?;
    build_token_details_response(token, vault, aggregate)
}

async fn read_token_details_list_vaults(
    sft_subgraph_url: &str,
    addresses: &[Address],
) -> Result<HashMap<Address, SftTokenDetailsSummaryVault>, ApiError> {
    const QUERY: &str = r#"
query TokenDetailsList($addresses: [Bytes!]!) {
  offchainAssetReceiptVaults(
    first: 1000
    where: { wrappedTokenContractAddress_in: $addresses }
  ) {
    receiptContractAddress
    wrappedTokenContractAddress
    totalShares
    name
    symbol
    tokenHolderCount
    shareTransferCount
    depositVolume
    withdrawVolume
  }
}
"#;

    let mut vaults = HashMap::new();
    for chunk in addresses.chunks(SFT_PAGE_SIZE) {
        let address_values = chunk
            .iter()
            .map(|address| format!("{address:#x}"))
            .collect::<Vec<_>>();
        let data = post_graphql::<SftTokenDetailsListData>(
            sft_subgraph_url,
            QUERY,
            json!({
                "addresses": address_values,
            }),
        )
        .await?;

        for vault in data.offchain_asset_receipt_vaults {
            let Some(wrapped_address) = vault.wrapped_token_contract_address else {
                tracing::error!("token details list vault missing wrapped token address");
                continue;
            };
            vaults.insert(wrapped_address, vault);
        }
    }
    Ok(vaults)
}

fn activity_limit(params: &TokenDetailsQueryParams) -> u32 {
    params
        .activity_limit
        .unwrap_or(DEFAULT_ACTIVITY_LIMIT)
        .clamp(1, MAX_ACTIVITY_LIMIT)
}

#[utoipa::path(
    get,
    path = "/v1/tokens/details",
    tag = "Tokens",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "ST0x token detail summaries with per-token errors", body = TokenDetailsListResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/details")]
pub async fn get_token_details(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
) -> Result<Json<TokenDetailsListResponse>, ApiError> {
    async move {
        tracing::info!("request received");

        let tokens = registry_tokens(shared_raindex).await?;
        let st0x_tokens: Vec<TokenCfg> = tokens.into_iter().filter(is_st0x_token).collect();
        tracing::info!(count = st0x_tokens.len(), "reading ST0x token details");

        let batch_items = {
            let raindex = shared_raindex.read().await;
            st0x_tokens
                .into_iter()
                .map(|token| {
                    let sft_subgraph_url =
                        resolve_sft_subgraph_url(raindex.raindex_yaml(), &token.network.key);
                    TokenDetailsBatchItem {
                        token,
                        sft_subgraph_url,
                    }
                })
                .collect::<Vec<_>>()
        };
        let cache_key = token_details_list_cache_key(&batch_items);
        if let Some(response) = token_details_list_cache().get(&cache_key).await {
            tracing::info!(
                data_count = response.data.len(),
                error_count = response.errors.len(),
                "returning cached token details"
            );
            return Ok(Json(response));
        }

        tracing::info!("token details list cache miss");

        let mut data = Vec::new();
        let mut errors = Vec::new();
        let mut vaults_by_subgraph = HashMap::new();
        let mut tokens_by_subgraph: HashMap<String, Vec<&TokenCfg>> = HashMap::new();

        for item in &batch_items {
            match &item.sft_subgraph_url {
                Ok(url) => tokens_by_subgraph
                    .entry(url.clone())
                    .or_default()
                    .push(&item.token),
                Err(error) => {
                    tracing::error!(
                        address = %item.token.address,
                        error = %error,
                        "failed to resolve token details subgraph"
                    );
                    errors.push(TokenDetailsErrorResponse {
                        address: item.token.address,
                        message: api_error_message(error),
                    });
                }
            }
        }

        for (url, tokens) in &tokens_by_subgraph {
            let addresses = tokens.iter().map(|token| token.address).collect::<Vec<_>>();
            match read_token_details_list_vaults(url, &addresses).await {
                Ok(vaults) => {
                    vaults_by_subgraph.insert(url.clone(), vaults);
                }
                Err(error) => {
                    tracing::error!(
                        url,
                        error = %error,
                        "failed to read batched token details"
                    );
                    for token in tokens {
                        errors.push(TokenDetailsErrorResponse {
                            address: token.address,
                            message: api_error_message(&error),
                        });
                    }
                }
            }
        }

        for item in &batch_items {
            let Ok(url) = &item.sft_subgraph_url else {
                continue;
            };
            let Some(vaults) = vaults_by_subgraph.get(url) else {
                continue;
            };
            let row = vaults
                .get(&item.token.address)
                .ok_or_else(|| {
                    tracing::warn!(
                        address = %item.token.address,
                        "SFT vault not found for token details"
                    );
                    ApiError::NotFound("SFT vault not found for token".into())
                })
                .and_then(|vault| build_token_details_list_summary_response(&item.token, vault));

            match row {
                Ok(row) => data.push(row),
                Err(error) => {
                    tracing::error!(
                        address = %item.token.address,
                        error = %error,
                        "failed to read token details"
                    );
                    errors.push(TokenDetailsErrorResponse {
                        address: item.token.address,
                        message: api_error_message(&error),
                    });
                }
            }
        }

        tracing::info!(
            data_count = data.len(),
            error_count = errors.len(),
            "returning token details"
        );
        let response = TokenDetailsListResponse { data, errors };
        if response.errors.is_empty() {
            token_details_list_cache()
                .insert(cache_key, response.clone())
                .await;
        } else {
            tracing::warn!(
                error_count = response.errors.len(),
                "skipping token details list cache because response is partial"
            );
        }
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/tokens/{address}/details",
    tag = "Tokens",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Wrapped, unwrapped, or legacy ST0x token address"),
        TokenDetailsQueryParams,
    ),
    responses(
        (status = 200, description = "ST0x token details and recent deposit/withdraw activity", body = TokenDetailsResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "Wrapped ST0x token or SFT vault not found", body = ApiErrorResponse),
        (status = 422, description = "Invalid token address", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<address>/details?<params..>", rank = 10)]
pub async fn get_token_details_by_address(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    shared_raindex: &State<SharedRaindexProvider>,
    address: ValidatedAddress,
    params: TokenDetailsQueryParams,
) -> Result<Json<TokenDetailsResponse>, ApiError> {
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

        let sft_subgraph_url = {
            let raindex = shared_raindex.read().await;
            resolve_sft_subgraph_url(raindex.raindex_yaml(), &token.network.key)?
        };
        let activity_limit = activity_limit(&params);

        tracing::info!(
            requested_address = %address.0,
            wrapped_address = %token.address,
            network_key = %token.network.key,
            activity_limit,
            "querying token details"
        );

        let response =
            read_token_details_response(token, &sft_subgraph_url, activity_limit).await?;

        tracing::info!(
            wrapped_address = %token.address,
            holder_count = response.summary.holder_count,
            transfer_count = response.summary.transfer_count,
            deposit_count = response.activity.deposits.len(),
            withdraw_count = response.activity.withdraws.len(),
            "returning token details"
        );
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}
