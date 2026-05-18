pub(crate) mod get_by_address;
pub(crate) mod get_by_order_hashes;
pub(crate) mod get_by_taker;
pub(crate) mod get_by_token;
pub(crate) mod get_by_tx;

use crate::error::ApiError;
use crate::types::common::TokenRef;
use crate::types::trades::{
    TradeByAddress, TradesByAddressResponse, TradesPagination, TradesPaginationParams,
};
use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use rain_orderbook_common::raindex_client::trades::{
    GetTradesByOrderHashesFilters, GetTradesFilters, GetTradesTokenFilter, OrderHashes,
    RaindexTrade, RaindexTradesByOrderHashResult, RaindexTradesListResult,
};
use rain_orderbook_common::raindex_client::types::{PaginationParams, TimeFilter};
use rain_orderbook_common::raindex_client::{RaindexClient, RaindexError};
use rocket::serde::json::Json;
use rocket::Route;

#[async_trait]
pub(crate) trait TradesDataSource: Send + Sync {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<RaindexTradesListResult, ApiError>;

    async fn get_trades_for_owner(
        &self,
        owner: Address,
        pagination: PaginationParams,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError>;

    async fn get_trades_for_token(
        &self,
        token: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError>;

    async fn get_trades_for_taker(
        &self,
        taker: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError>;

    async fn get_trades_by_order_hashes(
        &self,
        order_hashes: Vec<B256>,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesByOrderHashResult, ApiError>;
}

pub(crate) struct RaindexTradesDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait]
impl TradesDataSource for RaindexTradesDataSource<'_> {
    async fn get_trades_by_tx(&self, tx_hash: B256) -> Result<RaindexTradesListResult, ApiError> {
        self.client
            .get_trades_for_transaction(None, None, tx_hash)
            .await
            .map_err(|e| match e {
                RaindexError::TransactionIndexingTimeout { tx_hash, attempts } => {
                    ApiError::NotYetIndexed(format!(
                        "transaction {tx_hash:#x} not yet indexed after {attempts} attempts"
                    ))
                }
                other => {
                    tracing::error!(error = %other, "failed to query trades for transaction");
                    ApiError::Internal("failed to query trades".into())
                }
            })
    }

    async fn get_trades_for_owner(
        &self,
        owner: Address,
        pagination: PaginationParams,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError> {
        let filters = GetTradesFilters {
            owners: vec![owner],
            time_filter: Some(time_filter),
            ..Default::default()
        };

        self.client
            .get_trades(None, Some(filters), pagination.page, pagination.page_size)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades for owner");
                ApiError::Internal("failed to query trades".into())
            })
    }

    async fn get_trades_for_token(
        &self,
        token: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError> {
        let filters = GetTradesFilters {
            tokens: Some(GetTradesTokenFilter {
                inputs: Some(vec![token]),
                outputs: Some(vec![token]),
            }),
            time_filter: Some(time_filter),
            ..Default::default()
        };

        self.client
            .get_trades(None, Some(filters), Some(page), Some(page_size))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades for token");
                ApiError::Internal("failed to query trades".into())
            })
    }

    async fn get_trades_for_taker(
        &self,
        taker: Address,
        page: u16,
        page_size: u16,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesListResult, ApiError> {
        let filters = GetTradesFilters {
            takers: vec![taker],
            time_filter: Some(time_filter),
            ..Default::default()
        };

        self.client
            .get_trades(None, Some(filters), Some(page), Some(page_size))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades for taker");
                ApiError::Internal("failed to query trades".into())
            })
    }

    async fn get_trades_by_order_hashes(
        &self,
        order_hashes: Vec<B256>,
        time_filter: TimeFilter,
    ) -> Result<RaindexTradesByOrderHashResult, ApiError> {
        let filters = GetTradesByOrderHashesFilters {
            time_filter: Some(time_filter),
            ..Default::default()
        };

        self.client
            .get_trades_by_order_hashes(None, OrderHashes(order_hashes), Some(filters))
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query trades by order hashes");
                ApiError::Internal("failed to query trades".into())
            })
    }
}

pub(super) fn map_trade_for_list(trade: &RaindexTrade) -> Result<TradeByAddress, ApiError> {
    let tx_hash = trade.transaction().id();
    let input_vc = trade.input_vault_balance_change();
    let output_vc = trade.output_vault_balance_change();

    let input_token_data = input_vc.token();
    let output_token_data = output_vc.token();

    let timestamp: u64 = trade.timestamp().try_into().map_err(|_| {
        tracing::error!("timestamp does not fit in u64");
        ApiError::Internal("timestamp overflow".into())
    })?;
    let block_number: u64 = trade.transaction().block_number().try_into().map_err(|_| {
        tracing::error!("block number does not fit in u64");
        ApiError::Internal("block number overflow".into())
    })?;

    Ok(TradeByAddress {
        tx_hash,
        input_amount: input_vc.formatted_amount(),
        output_amount: output_vc.formatted_amount(),
        input_token: TokenRef {
            address: input_token_data.address(),
            symbol: input_token_data.symbol().unwrap_or_default(),
            decimals: input_token_data.decimals(),
        },
        output_token: TokenRef {
            address: output_token_data.address(),
            symbol: output_token_data.symbol().unwrap_or_default(),
            decimals: output_token_data.decimals(),
        },
        order_hash: Some(trade.order_hash()),
        timestamp,
        block_number,
    })
}

pub(super) fn build_trades_list_response(
    result: RaindexTradesListResult,
    page: u32,
    page_size: u32,
) -> Result<Json<TradesByAddressResponse>, ApiError> {
    let trades = result
        .trades()
        .iter()
        .map(map_trade_for_list)
        .collect::<Result<Vec<_>, ApiError>>()?;

    let total_trades = result.total_count();
    let total_pages = if page_size > 0 {
        total_trades.div_ceil(u64::from(page_size))
    } else {
        0
    };
    let has_more = u64::from(page) < total_pages;

    Ok(Json(TradesByAddressResponse {
        trades,
        pagination: TradesPagination {
            page,
            page_size,
            total_trades,
            total_pages,
            has_more,
        },
    }))
}

pub(super) fn trades_pagination_params(
    params: TradesPaginationParams,
) -> Result<(u32, u32, u16, u16, TimeFilter), ApiError> {
    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20);

    let sdk_page = page
        .try_into()
        .map_err(|_| ApiError::BadRequest("page value too large".into()))?;
    let sdk_page_size = page_size
        .try_into()
        .map_err(|_| ApiError::BadRequest("page_size value too large".into()))?;
    let time_filter = TimeFilter {
        start: params.start_time,
        end: params.end_time,
    };

    Ok((page, page_size, sdk_page, sdk_page_size, time_filter))
}

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_by_tx::get_trades_by_tx,
        get_by_order_hashes::get_trades_by_order_hashes,
        get_by_token::get_trades_by_token,
        get_by_taker::get_trades_by_taker,
        get_by_address::get_trades_by_address
    ]
}
