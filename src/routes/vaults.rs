use crate::auth::AuthenticatedKey;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::vaults::{
    VaultOrderRef, VaultPositionResponse, VaultTokenResponse, VaultTotalResponse,
    VaultTotalTokenResponse, VaultTotalsResponse, VaultsPagination, VaultsQueryParams,
    VaultsResponse,
};
use alloy::primitives::{Address, FixedBytes, U256};
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::{
    types::ChainIds,
    vaults::{GetVaultsFilters, RaindexVault},
    RaindexClient,
};
use rocket::serde::json::Json;
use rocket::{Route, State};
use std::collections::HashMap;
use tracing::Instrument;

const DEFAULT_PAGE: u16 = 1;
const DEFAULT_PAGE_SIZE: u16 = 100;
const MAX_PAGE_SIZE: u16 = 100;
const TOTALS_PAGE_SIZE: u16 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VaultRecord {
    pub id: String,
    pub vault_id: String,
    pub owner: Address,
    pub token: VaultTokenResponse,
    pub balance: U256,
    pub orderbook: Address,
    pub orders_as_input: Vec<FixedBytes<32>>,
    pub orders_as_output: Vec<FixedBytes<32>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VaultsPage {
    pub vaults: Vec<VaultRecord>,
    pub page: u32,
    pub page_size: u32,
    pub total_items: u64,
    pub has_more: bool,
}

#[async_trait]
pub(crate) trait VaultsDataSource: Send + Sync {
    async fn get_vaults(
        &self,
        filters: GetVaultsFilters,
        page: u16,
        page_size: u16,
    ) -> Result<VaultsPage, ApiError>;
}

pub(crate) struct RaindexVaultsDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait]
impl VaultsDataSource for RaindexVaultsDataSource<'_> {
    async fn get_vaults(
        &self,
        filters: GetVaultsFilters,
        page: u16,
        page_size: u16,
    ) -> Result<VaultsPage, ApiError> {
        let response = self
            .client
            .get_vaults(
                Some(ChainIds(vec![crate::CHAIN_ID])),
                Some(filters),
                Some(page),
                Some(page_size),
            )
            .await
            .map_err(|error| {
                tracing::error!(
                    error = %error,
                    page,
                    page_size,
                    "failed to retrieve vaults from raindex"
                );
                ApiError::Internal("failed to retrieve vaults".into())
            })?;

        let vaults = response
            .items()
            .into_iter()
            .map(vault_record_from_sdk)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(VaultsPage {
            vaults,
            page: response.page().into(),
            page_size: response.page_size().into(),
            total_items: response.total_items().into(),
            has_more: response.has_more(),
        })
    }
}

fn vault_record_from_sdk(vault: RaindexVault) -> Result<VaultRecord, ApiError> {
    let token = vault.token();
    let decimals = token.decimals();
    let balance = vault
        .balance()
        .to_fixed_decimal_lossy(decimals)
        .map_err(|error| {
            tracing::error!(
                error = %error,
                vault_id = %vault.vault_id(),
                token = %token.address(),
                "failed to convert vault balance to raw token units"
            );
            ApiError::Internal("failed to convert vault balance".into())
        })?
        .0;

    Ok(VaultRecord {
        id: vault.id().to_string(),
        vault_id: vault.vault_id().to_string(),
        owner: vault.owner(),
        token: VaultTokenResponse {
            address: token.address(),
            name: token.name(),
            symbol: token.symbol(),
            decimals,
        },
        balance,
        orderbook: vault.raindex(),
        orders_as_input: vault
            .orders_as_inputs()
            .into_iter()
            .map(|order| order.order_hash)
            .collect(),
        orders_as_output: vault
            .orders_as_outputs()
            .into_iter()
            .map(|order| order.order_hash)
            .collect(),
    })
}

fn parse_address(value: &str, field: &str) -> Result<Address, ApiError> {
    value.parse::<Address>().map_err(|error| {
        tracing::warn!(field, value, error = %error, "invalid address query parameter");
        ApiError::BadRequest(format!("{field} must be a valid address"))
    })
}

fn pagination(params: &VaultsQueryParams) -> Result<(u16, u16), ApiError> {
    let page = params.page.unwrap_or(DEFAULT_PAGE);
    if page == 0 {
        return Err(ApiError::BadRequest(
            "page must be greater than zero".into(),
        ));
    }

    let page_size = params.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    if page_size == 0 {
        return Err(ApiError::BadRequest(
            "pageSize must be greater than zero".into(),
        ));
    }

    Ok((page, page_size.min(MAX_PAGE_SIZE)))
}

fn order_refs(order_hashes: Vec<FixedBytes<32>>) -> Vec<VaultOrderRef> {
    order_hashes
        .into_iter()
        .map(|order_hash| VaultOrderRef { order_hash })
        .collect()
}

fn position_response(vault: VaultRecord) -> VaultPositionResponse {
    VaultPositionResponse {
        id: vault.id,
        vault_id: vault.vault_id,
        owner: vault.owner,
        token: vault.token,
        balance: vault.balance.to_string(),
        orderbook: vault.orderbook,
        orders_as_input: order_refs(vault.orders_as_input),
        orders_as_output: order_refs(vault.orders_as_output),
    }
}

pub(crate) async fn process_get_vaults(
    ds: &dyn VaultsDataSource,
    params: VaultsQueryParams,
) -> Result<VaultsResponse, ApiError> {
    let owner = params
        .owner
        .as_deref()
        .ok_or_else(|| ApiError::BadRequest("owner is required".into()))
        .and_then(|owner| parse_address(owner, "owner"))?;
    let token = params
        .token
        .as_deref()
        .map(|token| parse_address(token, "token"))
        .transpose()?;
    let (page, page_size) = pagination(&params)?;

    let filters = GetVaultsFilters {
        owners: vec![owner],
        hide_zero_balance: params.hide_zero_balance.unwrap_or(false),
        tokens: token.map(|token| vec![token]),
        ..Default::default()
    };

    let page = ds.get_vaults(filters, page, page_size).await?;

    Ok(VaultsResponse {
        vaults: page.vaults.into_iter().map(position_response).collect(),
        pagination: VaultsPagination {
            page: page.page,
            page_size: page.page_size,
            total_items: page.total_items,
            has_more: page.has_more,
        },
    })
}

#[derive(Debug, Clone)]
struct VaultTotalAccumulator {
    token: VaultTokenResponse,
    total_balance: U256,
    vault_count: u64,
}

pub(crate) async fn process_get_vault_totals(
    ds: &dyn VaultsDataSource,
) -> Result<VaultTotalsResponse, ApiError> {
    let filters = GetVaultsFilters {
        owners: Vec::new(),
        hide_zero_balance: true,
        ..Default::default()
    };
    let mut totals: HashMap<Address, VaultTotalAccumulator> = HashMap::new();
    let mut page = DEFAULT_PAGE;

    loop {
        let response = ds
            .get_vaults(filters.clone(), page, TOTALS_PAGE_SIZE)
            .await?;

        for vault in response
            .vaults
            .into_iter()
            .filter(|vault| vault.balance > U256::ZERO)
        {
            let entry =
                totals
                    .entry(vault.token.address)
                    .or_insert_with(|| VaultTotalAccumulator {
                        token: vault.token.clone(),
                        total_balance: U256::ZERO,
                        vault_count: 0,
                    });
            entry.total_balance += vault.balance;
            entry.vault_count += 1;
        }

        if !response.has_more {
            break;
        }
        page = page.checked_add(1).ok_or_else(|| {
            tracing::error!("vault totals pagination exhausted u16 page range");
            ApiError::Internal("vault totals pagination exceeded maximum".into())
        })?;
    }

    let mut totals: Vec<VaultTotalResponse> = totals
        .into_values()
        .map(|total| VaultTotalResponse {
            token: VaultTotalTokenResponse {
                address: total.token.address,
                symbol: total.token.symbol,
                decimals: total.token.decimals,
            },
            total_balance: total.total_balance.to_string(),
            vault_count: total.vault_count,
        })
        .collect();
    totals.sort_by(|a, b| a.token.address.cmp(&b.token.address));

    Ok(VaultTotalsResponse { totals })
}

#[utoipa::path(
    get,
    path = "/v1/vaults",
    tag = "Vaults",
    security(("basicAuth" = [])),
    params(VaultsQueryParams),
    responses(
        (status = 200, description = "Paginated list of deposit vault positions", body = VaultsResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/?<params..>")]
pub async fn get_vaults(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    span: TracingSpan,
    params: VaultsQueryParams,
) -> Result<Json<VaultsResponse>, ApiError> {
    async move {
        tracing::info!(params = ?params, "request received");
        let raindex = shared_raindex.read().await;
        let ds = RaindexVaultsDataSource {
            client: raindex.client(),
        };
        let response = process_get_vaults(&ds, params.clone())
            .await
            .map_err(|error| {
                tracing::warn!(params = ?params, error = %error, "get_vaults failed");
                error
            })?;
        tracing::info!(
            vault_count = response.vaults.len(),
            total_items = response.pagination.total_items,
            "returning vault positions"
        );
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/v1/vaults/totals",
    tag = "Vaults",
    security(("basicAuth" = [])),
    responses(
        (status = 200, description = "Aggregated non-zero vault balances by token", body = VaultTotalsResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/totals")]
pub async fn get_vault_totals(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    span: TracingSpan,
) -> Result<Json<VaultTotalsResponse>, ApiError> {
    async move {
        tracing::info!("request received");
        let raindex = shared_raindex.read().await;
        let ds = RaindexVaultsDataSource {
            client: raindex.client(),
        };
        let response = process_get_vault_totals(&ds).await.map_err(|error| {
            tracing::warn!(error = %error, "get_vault_totals failed");
            error
        })?;
        tracing::info!(
            token_count = response.totals.len(),
            "returning vault totals"
        );
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_vault_totals, get_vaults]
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, fixed_bytes};
    use std::sync::{Arc, Mutex};

    const OWNER: Address = address!("1111111111111111111111111111111111111111");
    const OTHER_OWNER: Address = address!("2222222222222222222222222222222222222222");
    const TOKEN_A: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const TOKEN_B: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    const ORDERBOOK: Address = address!("d2938e7c9fe3597f78832ce780feb61945c377d7");

    #[derive(Clone, Default)]
    struct MockVaultsDataSource {
        vaults: Vec<VaultRecord>,
        error: Option<ApiError>,
        calls: Arc<Mutex<Vec<(GetVaultsFilters, u16, u16)>>>,
    }

    #[async_trait]
    impl VaultsDataSource for MockVaultsDataSource {
        async fn get_vaults(
            &self,
            filters: GetVaultsFilters,
            page: u16,
            page_size: u16,
        ) -> Result<VaultsPage, ApiError> {
            self.calls
                .lock()
                .unwrap()
                .push((filters.clone(), page, page_size));
            if let Some(error) = &self.error {
                return Err(error.clone());
            }

            let mut vaults: Vec<VaultRecord> = self
                .vaults
                .iter()
                .filter(|vault| {
                    (filters.owners.is_empty() || filters.owners.contains(&vault.owner))
                        && filters
                            .tokens
                            .as_ref()
                            .is_none_or(|tokens| tokens.contains(&vault.token.address))
                        && (!filters.hide_zero_balance || vault.balance > U256::ZERO)
                })
                .cloned()
                .collect();
            vaults.sort_by(|a, b| a.id.cmp(&b.id));

            let total_items = vaults.len() as u64;
            let start = ((page as usize) - 1) * page_size as usize;
            let end = start.saturating_add(page_size as usize).min(vaults.len());
            let page_vaults = if start < vaults.len() {
                vaults[start..end].to_vec()
            } else {
                Vec::new()
            };

            Ok(VaultsPage {
                vaults: page_vaults,
                page: page.into(),
                page_size: page_size.into(),
                total_items,
                has_more: end < vaults.len(),
            })
        }
    }

    fn token(address: Address, symbol: &str, decimals: u8) -> VaultTokenResponse {
        VaultTokenResponse {
            address,
            name: Some(format!("{symbol} Token")),
            symbol: Some(symbol.to_string()),
            decimals,
        }
    }

    fn vault(
        id: &str,
        owner: Address,
        token: VaultTokenResponse,
        balance: u64,
        order_hash_seed: u8,
    ) -> VaultRecord {
        VaultRecord {
            id: id.to_string(),
            vault_id: balance.to_string(),
            owner,
            token,
            balance: U256::from(balance),
            orderbook: ORDERBOOK,
            orders_as_input: vec![FixedBytes::from([order_hash_seed; 32])],
            orders_as_output: vec![FixedBytes::from([order_hash_seed + 1; 32])],
        }
    }

    fn params(owner: &str) -> VaultsQueryParams {
        VaultsQueryParams {
            owner: Some(owner.to_string()),
            token: None,
            hide_zero_balance: None,
            page: None,
            page_size: None,
        }
    }

    #[rocket::async_test]
    async fn get_vaults_happy_path() {
        let ds = MockVaultsDataSource {
            vaults: vec![vault("1", OWNER, token(TOKEN_A, "USDC", 6), 1_000_000, 1)],
            ..Default::default()
        };

        let response = process_get_vaults(&ds, params(&OWNER.to_string()))
            .await
            .unwrap();

        assert_eq!(response.vaults.len(), 1);
        let vault = &response.vaults[0];
        assert_eq!(vault.owner, OWNER);
        assert_eq!(vault.token.address, TOKEN_A);
        assert_eq!(vault.balance, "1000000");
        assert_eq!(vault.orderbook, ORDERBOOK);
        assert_eq!(
            vault.orders_as_input[0].order_hash,
            fixed_bytes!("0101010101010101010101010101010101010101010101010101010101010101")
        );
        assert_eq!(
            vault.orders_as_output[0].order_hash,
            fixed_bytes!("0202020202020202020202020202020202020202020202020202020202020202")
        );
        assert_eq!(response.pagination.total_items, 1);
        assert!(!response.pagination.has_more);
    }

    #[rocket::async_test]
    async fn get_vaults_applies_token_filter() {
        let ds = MockVaultsDataSource {
            vaults: vec![
                vault("1", OWNER, token(TOKEN_A, "USDC", 6), 1, 1),
                vault("2", OWNER, token(TOKEN_B, "WETH", 18), 2, 3),
            ],
            ..Default::default()
        };
        let mut params = params(&OWNER.to_string());
        params.token = Some(TOKEN_B.to_string());

        let response = process_get_vaults(&ds, params).await.unwrap();

        assert_eq!(response.vaults.len(), 1);
        assert_eq!(response.vaults[0].token.address, TOKEN_B);
    }

    #[rocket::async_test]
    async fn get_vaults_hide_zero_balance() {
        let ds = MockVaultsDataSource {
            vaults: vec![
                vault("1", OWNER, token(TOKEN_A, "USDC", 6), 0, 1),
                vault("2", OWNER, token(TOKEN_A, "USDC", 6), 5, 3),
            ],
            ..Default::default()
        };
        let mut params = params(&OWNER.to_string());
        params.hide_zero_balance = Some(true);

        let response = process_get_vaults(&ds, params).await.unwrap();

        assert_eq!(response.vaults.len(), 1);
        assert_eq!(response.vaults[0].balance, "5");
    }

    #[rocket::async_test]
    async fn get_vaults_paginates_and_sets_has_more() {
        let ds = MockVaultsDataSource {
            vaults: (0..3)
                .map(|i| {
                    vault(
                        &i.to_string(),
                        OWNER,
                        token(TOKEN_A, "USDC", 6),
                        i + 1,
                        i as u8,
                    )
                })
                .collect(),
            ..Default::default()
        };
        let mut params = params(&OWNER.to_string());
        params.page = Some(2);
        params.page_size = Some(1);

        let response = process_get_vaults(&ds, params).await.unwrap();

        assert_eq!(response.vaults.len(), 1);
        assert_eq!(response.vaults[0].id, "1");
        assert_eq!(response.pagination.page, 2);
        assert_eq!(response.pagination.page_size, 1);
        assert_eq!(response.pagination.total_items, 3);
        assert!(response.pagination.has_more);
    }

    #[rocket::async_test]
    async fn get_vaults_rejects_invalid_owner_and_token() {
        let ds = MockVaultsDataSource::default();

        let err = process_get_vaults(&ds, params("not-an-address"))
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));

        let mut params = params(&OWNER.to_string());
        params.token = Some("not-a-token".to_string());
        let err = process_get_vaults(&ds, params).await.unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[rocket::async_test]
    async fn get_vaults_requires_owner() {
        let ds = MockVaultsDataSource::default();
        let err = process_get_vaults(&ds, VaultsQueryParams::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[rocket::async_test]
    async fn get_vaults_maps_data_source_error() {
        let ds = MockVaultsDataSource {
            error: Some(ApiError::Internal("subgraph error".into())),
            ..Default::default()
        };
        let err = process_get_vaults(&ds, params(&OWNER.to_string()))
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Internal(_)));
    }

    #[rocket::async_test]
    async fn get_vault_totals_aggregates_non_zero_by_token() {
        let ds = MockVaultsDataSource {
            vaults: vec![
                vault("1", OWNER, token(TOKEN_A, "USDC", 6), 7, 1),
                vault("2", OTHER_OWNER, token(TOKEN_A, "USDC", 6), 5, 3),
                vault("3", OWNER, token(TOKEN_B, "WETH", 18), 11, 5),
            ],
            ..Default::default()
        };

        let response = process_get_vault_totals(&ds).await.unwrap();

        assert_eq!(response.totals.len(), 2);
        assert_eq!(response.totals[0].token.address, TOKEN_A);
        assert_eq!(response.totals[0].total_balance, "12");
        assert_eq!(response.totals[0].vault_count, 2);
        assert_eq!(response.totals[1].token.address, TOKEN_B);
        assert_eq!(response.totals[1].total_balance, "11");
        assert_eq!(response.totals[1].vault_count, 1);
    }

    #[rocket::async_test]
    async fn get_vault_totals_skips_zero_balances() {
        let ds = MockVaultsDataSource {
            vaults: vec![
                vault("1", OWNER, token(TOKEN_A, "USDC", 6), 0, 1),
                vault("2", OWNER, token(TOKEN_A, "USDC", 6), 9, 3),
            ],
            ..Default::default()
        };

        let response = process_get_vault_totals(&ds).await.unwrap();

        assert_eq!(response.totals.len(), 1);
        assert_eq!(response.totals[0].total_balance, "9");
        assert_eq!(response.totals[0].vault_count, 1);
    }

    #[rocket::async_test]
    async fn totals_uses_empty_owner_filter_and_hide_zero_balance() {
        let ds = MockVaultsDataSource {
            vaults: vec![vault("1", OWNER, token(TOKEN_A, "USDC", 6), 1, 1)],
            ..Default::default()
        };

        process_get_vault_totals(&ds).await.unwrap();

        let calls = ds.calls.lock().unwrap();
        assert_eq!(calls[0].0.owners, Vec::<Address>::new());
        assert!(calls[0].0.hide_zero_balance);
    }
}
