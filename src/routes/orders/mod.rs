mod get_by_owner;
mod get_by_token;
mod get_by_tx;

use crate::error::ApiError;
use crate::types::common::TokenRef;
use crate::types::orders::{OrderSummary, OrdersListResponse, OrdersPagination};
use async_trait::async_trait;
use futures::{future::join_all, stream, StreamExt};
use rain_orderbook_common::raindex_client::order_quotes::{
    get_order_quotes_batch as fetch_order_quotes_batch, RaindexOrderQuote,
};
use rain_orderbook_common::raindex_client::orders::{GetOrdersFilters, RaindexOrder};
use rain_orderbook_common::raindex_client::RaindexClient;
use rocket::Route;
use std::collections::BTreeMap;

pub(crate) const DEFAULT_PAGE_SIZE: u32 = 20;
pub(crate) const MAX_PAGE_SIZE: u16 = 50;
const MAX_CHAIN_BATCH_CONCURRENCY: usize = 4;

type OrderQuoteResult = Result<Vec<RaindexOrderQuote>, ApiError>;
type OrderQuoteBatchResult = Result<Vec<Vec<RaindexOrderQuote>>, ApiError>;
type IndexedOrder = (usize, RaindexOrder);
type GroupedOrders = BTreeMap<u32, Vec<IndexedOrder>>;

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

    async fn get_order_quotes_batch_for_chain(
        &self,
        _orders: &[RaindexOrder],
    ) -> OrderQuoteBatchResult {
        Err(ApiError::Internal(
            "batched order quote fetch unavailable".into(),
        ))
    }

    async fn get_order_quotes_batch(&self, orders: &[RaindexOrder]) -> Vec<OrderQuoteResult> {
        fetch_order_quotes_grouped(self, orders).await
    }
}

pub(crate) struct RaindexOrdersListDataSource<'a> {
    pub client: &'a RaindexClient,
}

fn group_orders_by_chain(orders: &[RaindexOrder]) -> GroupedOrders {
    let mut grouped_orders: GroupedOrders = BTreeMap::new();
    for (index, order) in orders.iter().cloned().enumerate() {
        grouped_orders
            .entry(order.chain_id())
            .or_default()
            .push((index, order));
    }
    grouped_orders
}

async fn fetch_order_quotes_individually<T>(
    ds: &T,
    chain_id: u32,
    indexed_orders: Vec<IndexedOrder>,
) -> Vec<(usize, OrderQuoteResult)>
where
    T: OrdersListDataSource + ?Sized,
{
    tracing::info!(
        chain_id,
        order_count = indexed_orders.len(),
        "falling back to per-order quotes"
    );

    join_all(indexed_orders.into_iter().map(|(index, order)| async move {
        let result = ds.get_order_quotes(&order).await;
        (index, result)
    }))
    .await
}

async fn fetch_quotes_for_chain_group<T>(
    ds: &T,
    chain_id: u32,
    indexed_orders: Vec<IndexedOrder>,
) -> Vec<(usize, OrderQuoteResult)>
where
    T: OrdersListDataSource + ?Sized,
{
    let group_orders: Vec<RaindexOrder> = indexed_orders
        .iter()
        .map(|(_, order)| order.clone())
        .collect();

    match ds.get_order_quotes_batch_for_chain(&group_orders).await {
        Ok(group_quotes) if group_quotes.len() == group_orders.len() => {
            tracing::info!(
                chain_id,
                order_count = group_orders.len(),
                "queried order quotes in batch"
            );
            indexed_orders
                .into_iter()
                .zip(group_quotes)
                .map(|((index, _), quotes)| (index, Ok(quotes)))
                .collect()
        }
        Ok(group_quotes) => {
            tracing::warn!(
                chain_id,
                order_count = group_orders.len(),
                received_quote_sets = group_quotes.len(),
                "batch quote fetch returned unexpected result count; falling back"
            );
            fetch_order_quotes_individually(ds, chain_id, indexed_orders).await
        }
        Err(error) => {
            tracing::warn!(
                chain_id,
                order_count = group_orders.len(),
                error = ?error,
                "batch quote fetch failed; falling back"
            );
            fetch_order_quotes_individually(ds, chain_id, indexed_orders).await
        }
    }
}

async fn fetch_order_quotes_grouped<T>(ds: &T, orders: &[RaindexOrder]) -> Vec<OrderQuoteResult>
where
    T: OrdersListDataSource + ?Sized,
{
    if orders.is_empty() {
        return vec![];
    }

    let grouped_orders = group_orders_by_chain(orders);
    let grouped_results = stream::iter(grouped_orders.into_iter().map(
        |(chain_id, indexed_orders)| async move {
            fetch_quotes_for_chain_group(ds, chain_id, indexed_orders).await
        },
    ))
    .buffer_unordered(MAX_CHAIN_BATCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut ordered_results = Vec::with_capacity(orders.len());
    ordered_results.resize_with(orders.len(), || None);

    for group_results in grouped_results {
        for (index, quotes_result) in group_results {
            ordered_results[index] = Some(quotes_result);
        }
    }

    ordered_results
        .into_iter()
        .map(|entry| {
            entry.unwrap_or_else(|| Err(ApiError::Internal("failed to query order quotes".into())))
        })
        .collect()
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

    async fn get_order_quotes_batch_for_chain(
        &self,
        orders: &[RaindexOrder],
    ) -> OrderQuoteBatchResult {
        let chain_id = orders
            .first()
            .map(RaindexOrder::chain_id)
            .unwrap_or_default();
        fetch_order_quotes_batch(orders, None, None)
            .await
            .map_err(|error| {
                tracing::error!(
                    chain_id,
                    error = %error,
                    "failed to batch query order quotes"
                );
                ApiError::Internal("failed to query order quotes".into())
            })
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

pub(crate) fn quote_result_to_io_ratio(
    order: &RaindexOrder,
    quotes_result: OrderQuoteResult,
) -> String {
    match quotes_result {
        Ok(quotes) => quotes
            .first()
            .and_then(|quote| quote.data.as_ref())
            .map(|quote| quote.formatted_ratio.clone())
            .unwrap_or_else(|| "-".into()),
        Err(err) => {
            tracing::warn!(
                order_hash = ?order.order_hash(),
                error = ?err,
                "quote fetch failed; using fallback io_ratio"
            );
            "-".into()
        }
    }
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

pub(crate) fn build_orders_list_response(
    orders: &[RaindexOrder],
    total_count: u32,
    page: u32,
    page_size: u32,
    quote_results: Vec<OrderQuoteResult>,
) -> Result<OrdersListResponse, ApiError> {
    if quote_results.len() != orders.len() {
        tracing::error!(
            expected_results = orders.len(),
            actual_results = quote_results.len(),
            "order quote results length mismatch"
        );
        return Err(ApiError::Internal("failed to query order quotes".into()));
    }

    let mut summaries = Vec::with_capacity(orders.len());
    for (order, quotes_result) in orders.iter().zip(quote_results) {
        let io_ratio = quote_result_to_io_ratio(order, quotes_result);
        summaries.push(build_order_summary(order, &io_ratio)?);
    }

    Ok(OrdersListResponse {
        orders: summaries,
        pagination: build_pagination(total_count, page, page_size),
    })
}

pub use get_by_owner::*;
pub use get_by_token::*;
pub use get_by_tx::*;

pub(crate) use get_by_owner::{orders_by_owner_cache, OrdersByOwnerCache};
pub(crate) use get_by_token::{orders_by_token_cache, OrdersByTokenCache};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::order::test_fixtures::{mock_quote, order_json};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    struct BatchingTestDataSource {
        per_order_quotes: HashMap<String, OrderQuoteResult>,
        batched_quotes: HashMap<u32, OrderQuoteBatchResult>,
        batch_calls: Arc<Mutex<Vec<(u32, usize)>>>,
        single_calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl OrdersListDataSource for BatchingTestDataSource {
        async fn get_orders_list(
            &self,
            _filters: GetOrdersFilters,
            _page: Option<u16>,
            _page_size: Option<u16>,
        ) -> Result<(Vec<RaindexOrder>, u32), ApiError> {
            unreachable!("not used in batching tests")
        }

        async fn get_order_quotes(
            &self,
            order: &RaindexOrder,
        ) -> Result<Vec<RaindexOrderQuote>, ApiError> {
            let order_hash = format!("{:?}", order.order_hash());
            self.single_calls
                .lock()
                .expect("lock single calls")
                .push(order_hash.clone());
            self.per_order_quotes
                .get(&order_hash)
                .cloned()
                .expect("per-order quote should exist")
        }

        async fn get_order_quotes_batch_for_chain(
            &self,
            orders: &[RaindexOrder],
        ) -> OrderQuoteBatchResult {
            let chain_id = orders
                .first()
                .map(RaindexOrder::chain_id)
                .unwrap_or_default();
            self.batch_calls
                .lock()
                .expect("lock batch calls")
                .push((chain_id, orders.len()));
            self.batched_quotes
                .get(&chain_id)
                .cloned()
                .expect("batch quote result should exist")
        }
    }

    fn mock_order_for_chain(chain_id: u32, order_hash: &str) -> RaindexOrder {
        let mut value = order_json();
        value["chainId"] = json!(chain_id);
        value["orderHash"] = json!(order_hash);
        serde_json::from_value(value).expect("deserialize chain-specific mock order")
    }

    fn quote_ratio(result: &OrderQuoteResult) -> String {
        result
            .as_ref()
            .expect("quote result should succeed")
            .first()
            .and_then(|quote| quote.data.as_ref())
            .map(|quote| quote.formatted_ratio.clone())
            .expect("quote should contain ratio")
    }

    fn order_hash_key(order: &RaindexOrder) -> String {
        format!("{:?}", order.order_hash())
    }

    #[rocket::async_test]
    async fn test_fetch_order_quotes_grouped_preserves_input_order() {
        let orders = vec![
            mock_order_for_chain(
                1,
                "0x0000000000000000000000000000000000000000000000000000000000000001",
            ),
            mock_order_for_chain(
                10,
                "0x0000000000000000000000000000000000000000000000000000000000000002",
            ),
            mock_order_for_chain(
                1,
                "0x0000000000000000000000000000000000000000000000000000000000000003",
            ),
        ];
        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::new(),
            batched_quotes: HashMap::from([
                (
                    1,
                    Ok(vec![vec![mock_quote("1.1")], vec![mock_quote("1.3")]]),
                ),
                (10, Ok(vec![vec![mock_quote("1.2")]])),
            ]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let results = ds.get_order_quotes_batch(&orders).await;

        assert_eq!(results.len(), 3);
        assert_eq!(quote_ratio(&results[0]), "1.1");
        assert_eq!(quote_ratio(&results[1]), "1.2");
        assert_eq!(quote_ratio(&results[2]), "1.3");

        let batch_calls = batch_calls.lock().expect("lock batch calls");
        assert_eq!(batch_calls.len(), 2);
        assert!(batch_calls.contains(&(1, 2)));
        assert!(batch_calls.contains(&(10, 1)));
        assert!(single_calls.lock().expect("lock single calls").is_empty());
    }

    #[rocket::async_test]
    async fn test_fetch_order_quotes_grouped_falls_back_when_batch_fails() {
        let orders = vec![
            mock_order_for_chain(
                137,
                "0x0000000000000000000000000000000000000000000000000000000000000011",
            ),
            mock_order_for_chain(
                137,
                "0x0000000000000000000000000000000000000000000000000000000000000012",
            ),
        ];
        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::from([
                (order_hash_key(&orders[0]), Ok(vec![mock_quote("2.5")])),
                (order_hash_key(&orders[1]), Ok(vec![mock_quote("2.5")])),
            ]),
            batched_quotes: HashMap::from([(137, Err(ApiError::Internal("batch failed".into())))]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let results = ds.get_order_quotes_batch(&orders).await;

        assert_eq!(results.len(), 2);
        assert_eq!(quote_ratio(&results[0]), "2.5");
        assert_eq!(quote_ratio(&results[1]), "2.5");
        assert_eq!(
            batch_calls.lock().expect("lock batch calls").as_slice(),
            &[(137, 2)]
        );
        assert_eq!(single_calls.lock().expect("lock single calls").len(), 2);
    }

    #[rocket::async_test]
    async fn test_fetch_order_quotes_grouped_falls_back_on_result_count_mismatch() {
        let orders = vec![
            mock_order_for_chain(
                42161,
                "0x0000000000000000000000000000000000000000000000000000000000000021",
            ),
            mock_order_for_chain(
                42161,
                "0x0000000000000000000000000000000000000000000000000000000000000022",
            ),
        ];
        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::from([
                (order_hash_key(&orders[0]), Ok(vec![mock_quote("3.5")])),
                (order_hash_key(&orders[1]), Ok(vec![mock_quote("3.5")])),
            ]),
            batched_quotes: HashMap::from([(42161, Ok(vec![vec![mock_quote("9.9")]]))]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let results = ds.get_order_quotes_batch(&orders).await;

        assert_eq!(results.len(), 2);
        assert_eq!(quote_ratio(&results[0]), "3.5");
        assert_eq!(quote_ratio(&results[1]), "3.5");
        assert_eq!(
            batch_calls.lock().expect("lock batch calls").as_slice(),
            &[(42161, 2)]
        );
        assert_eq!(single_calls.lock().expect("lock single calls").len(), 2);
    }

    #[test]
    fn test_build_orders_list_response_fails_on_length_mismatch() {
        let orders = vec![mock_order_for_chain(
            1,
            "0x0000000000000000000000000000000000000000000000000000000000000101",
        )];

        let result = build_orders_list_response(&orders, 1, 1, 20, vec![]);

        assert!(matches!(result, Err(ApiError::Internal(_))));
    }
}
