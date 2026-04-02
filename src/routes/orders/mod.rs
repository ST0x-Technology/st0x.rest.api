mod get_by_owner;
mod get_by_token;
mod get_by_tx;

use crate::error::ApiError;
use crate::types::common::TokenRef;
use crate::types::orders::{OrderSummary, OrdersPagination};
use async_trait::async_trait;
use futures::future::join_all;
use rain_orderbook_common::raindex_client::order_quotes::{
    get_order_quotes_batch as fetch_order_quotes_batch, RaindexOrderQuote,
};
use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};
use rain_orderbook_common::raindex_client::RaindexClient;
use rocket::Route;
use std::collections::BTreeMap;

pub(crate) const DEFAULT_PAGE_SIZE: u32 = 20;
pub(crate) const MAX_PAGE_SIZE: u16 = 50;

#[async_trait]
pub(crate) trait OrdersListDataSource: Send + Sync {
    async fn get_orders_list(
        &self,
        filters: GetOrdersFilters,
        page: Option<u16>,
        page_size: Option<u16>,
    ) -> Result<(Vec<RaindexOrder>, u32), ApiError>;

    async fn get_order_quotes(
        &self,
        order: &RaindexOrder,
    ) -> Result<Vec<RaindexOrderQuote>, ApiError>;

    async fn get_order_quotes_batch(
        &self,
        orders: &[RaindexOrder],
    ) -> Vec<Result<Vec<RaindexOrderQuote>, ApiError>> {
        let quote_futures: Vec<_> = orders
            .iter()
            .map(|order| self.get_order_quotes(order))
            .collect();
        join_all(quote_futures).await
    }
}

pub(crate) struct RaindexOrdersListDataSource<'a> {
    pub client: &'a RaindexClient,
}

#[async_trait]
impl<'a> OrdersListDataSource for RaindexOrdersListDataSource<'a> {
    async fn get_orders_list(
        &self,
        filters: GetOrdersFilters,
        page: Option<u16>,
        page_size: Option<u16>,
    ) -> Result<(Vec<RaindexOrder>, u32), ApiError> {
        let result = self
            .client
            .get_orders(None, Some(filters), page, page_size)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to query orders");
                ApiError::Internal("failed to query orders".into())
            })?;
        Ok((result.orders().to_vec(), result.total_count()))
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

    async fn get_order_quotes_batch(
        &self,
        orders: &[RaindexOrder],
    ) -> Vec<Result<Vec<RaindexOrderQuote>, ApiError>> {
        if orders.is_empty() {
            return vec![];
        }

        let mut grouped_orders: BTreeMap<u32, Vec<(usize, RaindexOrder)>> = BTreeMap::new();
        for (index, order) in orders.iter().cloned().enumerate() {
            grouped_orders
                .entry(order.chain_id())
                .or_default()
                .push((index, order));
        }

        let mut ordered_results = Vec::with_capacity(orders.len());
        ordered_results.resize_with(orders.len(), || None);

        for (chain_id, indexed_orders) in grouped_orders {
            let group_orders: Vec<RaindexOrder> = indexed_orders
                .iter()
                .map(|(_, order)| order.clone())
                .collect();

            match fetch_order_quotes_batch(&group_orders, None, None).await {
                Ok(group_quotes) => {
                    tracing::info!(
                        chain_id,
                        order_count = group_orders.len(),
                        "queried order quotes in batch"
                    );
                    for ((index, _), quotes) in indexed_orders.into_iter().zip(group_quotes) {
                        ordered_results[index] = Some(Ok(quotes));
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        chain_id,
                        order_count = group_orders.len(),
                        error = %error,
                        "batch quote fetch failed; falling back to per-order quotes"
                    );

                    let fallback_results = join_all(
                        group_orders
                            .iter()
                            .map(|order| self.get_order_quotes(order)),
                    )
                    .await;
                    for ((index, _), quotes_result) in
                        indexed_orders.into_iter().zip(fallback_results)
                    {
                        ordered_results[index] = Some(quotes_result);
                    }
                }
            }
        }

        ordered_results
            .into_iter()
            .map(|entry| {
                entry.unwrap_or_else(|| {
                    Err(ApiError::Internal("failed to query order quotes".into()))
                })
            })
            .collect()
    }
}

pub(crate) fn build_order_summary(
    order: &RaindexOrder,
    io_ratio: &str,
) -> Result<OrderSummary, ApiError> {
    let (input, output) = super::resolve_io_vaults(order)?;

    let input_token_info = input.token();
    let output_token_info = output.token();
    let created_at: u64 = order.timestamp_added().try_into().unwrap_or(0);

    Ok(OrderSummary {
        order_hash: order.order_hash(),
        owner: order.owner(),
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
        output_vault_balance: output.formatted_balance(),
        io_ratio: io_ratio.to_string(),
        created_at,
        orderbook_id: order.orderbook(),
    })
}

pub(crate) fn build_pagination(total_count: u32, page: u32, page_size: u32) -> OrdersPagination {
    let total_orders = total_count as u64;
    let total_pages = if page_size == 0 {
        0
    } else {
        total_orders.div_ceil(page_size as u64)
    };
    OrdersPagination {
        page,
        page_size,
        total_orders,
        total_pages,
        has_more: (page as u64) < total_pages,
    }
}

pub use get_by_owner::*;
pub use get_by_token::*;
pub use get_by_tx::*;

pub fn routes() -> Vec<Route> {
    rocket::routes![
        get_by_tx::get_orders_by_tx,
        get_by_owner::get_orders_by_address,
        get_by_token::get_orders_by_token
    ]
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::OrdersListDataSource;
    use crate::error::ApiError;
    use async_trait::async_trait;
    use rain_orderbook_common::raindex_client::order_quotes::RaindexOrderQuote;
    use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};

    pub struct MockOrdersListDataSource {
        pub orders: Result<Vec<RaindexOrder>, ApiError>,
        pub total_count: u32,
        pub quotes: Result<Vec<RaindexOrderQuote>, ApiError>,
    }

    #[async_trait]
    impl OrdersListDataSource for MockOrdersListDataSource {
        async fn get_orders_list(
            &self,
            _filters: GetOrdersFilters,
            _page: Option<u16>,
            _page_size: Option<u16>,
        ) -> Result<(Vec<RaindexOrder>, u32), ApiError> {
            match &self.orders {
                Ok(orders) => Ok((orders.clone(), self.total_count)),
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
    }
}
