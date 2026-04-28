mod get_by_owner;
mod get_by_token;
mod get_by_tx;
mod limit_cache;
mod stale_price_skip;

use crate::error::ApiError;
use crate::types::common::TokenRef;
use crate::types::orders::{OrderSummary, OrdersListResponse, OrdersPagination};
use async_trait::async_trait;
use futures::{future::join_all, stream, StreamExt};
use rain_orderbook_bindings::IOrderBookV6::SignedContextV1;
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

/// Fetch signed oracle context from an order's oracle URL.
/// Returns an empty vec if the order has no oracle URL or the fetch fails.
///
/// The oracle server expects a POST with an ABI-encoded `bytes` body.
/// An empty bytes value is: offset (0x20) + length (0x00), each as a 32-byte word.
async fn fetch_oracle_context(oracle_url: &str) -> Vec<SignedContextV1> {
    // ABI-encode an empty `bytes` value: offset=0x20, length=0
    let mut abi_body = vec![0u8; 64];
    abi_body[31] = 0x20; // offset = 32

    let client = reqwest::Client::new();
    let resp = match client
        .post(oracle_url)
        .header("Content-Type", "application/octet-stream")
        .body(abi_body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp,
        Ok(resp) => {
            tracing::warn!(
                oracle_url,
                status = %resp.status(),
                "oracle endpoint returned non-success status"
            );
            return vec![];
        }
        Err(e) => {
            tracing::warn!(oracle_url, error = %e, "failed to fetch oracle context");
            return vec![];
        }
    };

    let body = match resp.text().await {
        Ok(body) => body,
        Err(e) => {
            tracing::warn!(oracle_url, error = %e, "failed to read oracle response body");
            return vec![];
        }
    };

    // Try parsing as array first, then as single object
    if let Ok(contexts) = serde_json::from_str::<Vec<SignedContextV1>>(&body) {
        tracing::debug!(
            oracle_url,
            count = contexts.len(),
            "fetched oracle signed context"
        );
        return contexts;
    }
    if let Ok(context) = serde_json::from_str::<SignedContextV1>(&body) {
        tracing::debug!(oracle_url, "fetched single oracle signed context");
        return vec![context];
    }

    tracing::warn!(
        oracle_url,
        "failed to parse oracle response as SignedContextV1"
    );
    vec![]
}

/// For each order, fetch oracle signed context if the order has an oracle URL.
/// Returns a vec parallel to the input orders, with empty vecs for non-oracle orders.
async fn fetch_oracle_contexts_for_orders(orders: &[RaindexOrder]) -> Vec<Vec<SignedContextV1>> {
    let futures: Vec<_> = orders
        .iter()
        .map(|order| async move {
            match order.oracle_url() {
                Some(url) => fetch_oracle_context(&url).await,
                None => vec![],
            }
        })
        .collect();
    join_all(futures).await
}

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

    /// Fetch `QuoteFields` (display-level extracted quote data) for each order.
    ///
    /// Default implementation runs the multicall batch for every order and
    /// returns the extracted fields with no caching. The real implementation
    /// applies caches (e.g. limit-order ratio cache) before falling back to
    /// the multicall.
    async fn fetch_quote_fields(&self, orders: &[RaindexOrder]) -> Vec<QuoteFields> {
        let results = self.get_order_quotes_batch(orders).await;
        orders
            .iter()
            .zip(results)
            .map(|(order, result)| extract_quote_fields(order, result))
            .collect()
    }
}

pub(crate) struct RaindexOrdersListDataSource<'a> {
    pub client: &'a RaindexClient,
    pub block_number_cache: &'a crate::raindex::BlockNumberCache,
    pub limit_ratio_cache: &'a LimitOrderRatioCache,
    pub stale_price_skip_cache: &'a StalePriceSkipCache,
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

        // Fetch oracle signed context for orders that have an oracle URL.
        // This enables accurate quoting for oracle-dependent orders (e.g. SPYM).
        let signed_contexts = fetch_oracle_contexts_for_orders(orders).await;
        let has_any_context = signed_contexts.iter().any(|ctx| !ctx.is_empty());

        // Resolve the block number once via our short-TTL cache so multiple
        // concurrent batches hit the RPC at most once per cache window. If the
        // cache fetch fails we fall through to `None` and let the upstream
        // library do its own (uncached) lookup.
        let block_number = if let Some(first_order) = orders.first() {
            let rpc_urls: Vec<String> = first_order
                .get_rpc_urls()
                .map(|urls| urls.into_iter().map(|u| u.to_string()).collect())
                .unwrap_or_default();
            crate::raindex::get_or_fetch_block_number(self.block_number_cache, chain_id, &rpc_urls)
                .await
        } else {
            None
        };

        // Chunk size 16 matches the upstream library default. A multicall is
        // a single eth_call regardless of chunk size, so larger chunks reduce
        // RPC volume without adding latency. The library has a probe-and-split
        // safety net if a chunk exceeds the RPC's gas budget.
        fetch_order_quotes_batch(
            orders,
            block_number,
            Some(16),
            if has_any_context {
                Some(&signed_contexts)
            } else {
                None
            },
        )
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

    async fn fetch_quote_fields(&self, orders: &[RaindexOrder]) -> Vec<QuoteFields> {
        fetch_quote_fields_with_caches(
            self,
            self.limit_ratio_cache,
            self.stale_price_skip_cache,
            crate::market_calendar::is_nyse_open(chrono::Utc::now()),
            orders,
        )
        .await
    }
}

/// Apply per-order quote caches around a batched quote call:
/// - **Limit-order ratio cache**: orders identified as limit orders that
///   already have a cached io_ratio bypass the multicall entirely.
///   `max_output` is left `None`, which causes the downstream summary
///   builder to fall back to `vault_balance` (the right behavior for
///   limit orders, where max_output is bounded by the output vault).
/// - **Stale-price skip cache**: orders previously known to revert with
///   `StalePrice` are skipped while NYSE is closed (their oracle won't
///   refresh until the cash session reopens). When NYSE is open, every
///   order is quoted normally — fresh `StalePrice` failures re-mark the
///   order so it stays skipped during the next off-hours window.
/// - All other orders go through the standard batched quote path.
/// - After the batch, successful quotes for limit orders populate the
///   limit cache, and any quote whose error includes `StalePrice` is
///   added to the skip cache.
pub(crate) async fn fetch_quote_fields_with_caches<T>(
    ds: &T,
    limit_cache: &LimitOrderRatioCache,
    stale_skip_cache: &StalePriceSkipCache,
    nyse_open: bool,
    orders: &[RaindexOrder],
) -> Vec<QuoteFields>
where
    T: OrdersListDataSource + ?Sized,
{
    let mut fields: Vec<Option<QuoteFields>> = vec![None; orders.len()];
    let mut to_quote_indices: Vec<usize> = Vec::new();
    let mut to_quote_orders: Vec<RaindexOrder> = Vec::new();

    for (i, order) in orders.iter().enumerate() {
        if is_limit_order(order) {
            if let Some(cached_ratio) = limit_cache.get(&order.order_hash()).await {
                fields[i] = Some(QuoteFields {
                    io_ratio: cached_ratio,
                    max_output: None,
                });
                continue;
            }
        }
        if !nyse_open && stale_skip_cache.get(&order.order_hash()).await.is_some() {
            tracing::debug!(
                order_hash = ?order.order_hash(),
                "skipping quote for stale-marked order (NYSE closed)"
            );
            fields[i] = Some(QuoteFields {
                io_ratio: "-".into(),
                max_output: None,
            });
            continue;
        }
        to_quote_indices.push(i);
        to_quote_orders.push(order.clone());
    }

    if !to_quote_orders.is_empty() {
        let quote_results = ds.get_order_quotes_batch(&to_quote_orders).await;
        for (qi, &original_idx) in to_quote_indices.iter().enumerate() {
            let order = &orders[original_idx];
            let result = quote_results
                .get(qi)
                .cloned()
                .unwrap_or_else(|| Err(ApiError::Internal("missing quote result".into())));

            if let Ok(quotes) = &result {
                if quotes
                    .iter()
                    .any(|q| q.error.as_deref().is_some_and(quote_indicates_stale_price))
                {
                    stale_skip_cache.insert(order.order_hash(), ()).await;
                }
            }

            let extracted = extract_quote_fields(order, result);
            if is_limit_order(order) && extracted.io_ratio != "-" {
                limit_cache
                    .insert(order.order_hash(), extracted.io_ratio.clone())
                    .await;
            }
            fields[original_idx] = Some(extracted);
        }
    }

    fields
        .into_iter()
        .map(|opt| {
            opt.unwrap_or_else(|| QuoteFields {
                io_ratio: "-".into(),
                max_output: None,
            })
        })
        .collect()
}

/// Extracted quote fields for building order summaries.
#[derive(Clone)]
pub(crate) struct QuoteFields {
    pub io_ratio: String,
    /// Simulated max output from on-chain quote. None when quote failed or unavailable.
    pub max_output: Option<String>,
}

pub(crate) fn build_order_summary(
    order: &RaindexOrder,
    quote: &QuoteFields,
) -> Result<OrderSummary, ApiError> {
    let (input, output) = super::resolve_io_vaults(order)?;

    let input_token_info = input.token();
    let output_token_info = output.token();
    let created_at: u64 = order.timestamp_added().try_into().unwrap_or(0);
    let vault_balance = output.formatted_balance();
    let max_output = quote
        .max_output
        .clone()
        .unwrap_or_else(|| vault_balance.clone());

    Ok(OrderSummary {
        order_hash: order.order_hash(),
        owner: order.owner(),
        order_bytes: order.order_bytes(),
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
        output_vault_balance: vault_balance,
        max_output,
        io_ratio: quote.io_ratio.clone(),
        created_at,
        orderbook_id: order.orderbook(),
    })
}

pub(crate) fn extract_quote_fields(
    order: &RaindexOrder,
    quotes_result: OrderQuoteResult,
) -> QuoteFields {
    match quotes_result {
        Ok(quotes) => {
            let first = quotes.first();
            let data = first.and_then(|quote| quote.data.as_ref());
            if data.is_none() {
                if let Some(quote) = first {
                    tracing::warn!(
                        order_hash = ?order.order_hash(),
                        success = quote.success,
                        error = ?quote.error,
                        "quote returned no data; using fallback io_ratio"
                    );
                }
            }
            QuoteFields {
                io_ratio: data
                    .map(|d| d.formatted_ratio.clone())
                    .unwrap_or_else(|| "-".into()),
                max_output: data.map(|d| d.formatted_max_output.clone()),
            }
        }
        Err(err) => {
            tracing::warn!(
                order_hash = ?order.order_hash(),
                error = ?err,
                "quote fetch failed; using fallback io_ratio"
            );
            QuoteFields {
                io_ratio: "-".into(),
                max_output: None,
            }
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
        let quote = extract_quote_fields(order, quotes_result);
        summaries.push(build_order_summary(order, &quote)?);
    }

    Ok(OrdersListResponse {
        orders: summaries,
        pagination: build_pagination(total_count, page, page_size),
    })
}

pub use get_by_owner::*;
pub use get_by_token::*;
pub use get_by_tx::*;

pub(crate) use get_by_owner::orders_by_owner_cache;
pub(crate) use get_by_token::{
    orders_by_token_cache, process_get_orders_by_token, OrdersByTokenCache,
};
pub(crate) use limit_cache::{is_limit_order, limit_order_ratio_cache, LimitOrderRatioCache};
pub(crate) use stale_price_skip::{
    quote_indicates_stale_price, stale_price_skip_cache, StalePriceSkipCache,
};

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

    fn limit_order_for_chain(chain_id: u32, order_hash: &str, deployment: &str) -> RaindexOrder {
        let mut value = order_json();
        value["chainId"] = json!(chain_id);
        value["orderHash"] = json!(order_hash);
        value["parsedMeta"] = json!([{
            "DotrainGuiStateV1": {
                "dotrain_hash": "0x0000000000000000000000000000000000000000000000000000000000000001",
                "field_values": {},
                "deposits": {},
                "select_tokens": {},
                "vault_ids": {},
                "selected_deployment": deployment,
            }
        }]);
        serde_json::from_value(value).expect("deserialize limit-order mock")
    }

    #[rocket::async_test]
    async fn test_limit_cache_hit_skips_multicall() {
        let cache = limit_order_ratio_cache();
        let order = limit_order_for_chain(
            8453,
            "0x00000000000000000000000000000000000000000000000000000000000000aa",
            "fixed-limit-buy",
        );
        cache.insert(order.order_hash(), "0.42".to_string()).await;

        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::new(),
            batched_quotes: HashMap::new(),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let stale_skip_cache = stale_price_skip_cache();
        let fields = fetch_quote_fields_with_caches(
            &ds,
            &cache,
            &stale_skip_cache,
            true,
            std::slice::from_ref(&order),
        )
        .await;

        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].io_ratio, "0.42");
        assert!(fields[0].max_output.is_none());
        // No multicall and no per-order call should have happened.
        assert!(batch_calls.lock().expect("lock").is_empty());
        assert!(single_calls.lock().expect("lock").is_empty());
    }

    #[rocket::async_test]
    async fn test_limit_cache_miss_populates_cache() {
        let cache = limit_order_ratio_cache();
        let order = limit_order_for_chain(
            8453,
            "0x00000000000000000000000000000000000000000000000000000000000000bb",
            "fixed-limit-sell",
        );

        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::new(),
            batched_quotes: HashMap::from([(8453, Ok(vec![vec![mock_quote("1.234")]]))]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let stale_skip_cache = stale_price_skip_cache();
        let fields = fetch_quote_fields_with_caches(
            &ds,
            &cache,
            &stale_skip_cache,
            true,
            std::slice::from_ref(&order),
        )
        .await;

        assert_eq!(fields[0].io_ratio, "1.234");
        // Batch happened exactly once for the uncached limit order.
        assert_eq!(batch_calls.lock().expect("lock").len(), 1);
        // Cache now contains the freshly-fetched ratio.
        assert_eq!(
            cache.get(&order.order_hash()).await.as_deref(),
            Some("1.234")
        );
    }

    #[rocket::async_test]
    async fn test_limit_cache_does_not_cache_non_limit_orders() {
        let cache = limit_order_ratio_cache();
        let order = mock_order_for_chain(
            8453,
            "0x00000000000000000000000000000000000000000000000000000000000000cc",
        );

        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::new(),
            batched_quotes: HashMap::from([(8453, Ok(vec![vec![mock_quote("9.99")]]))]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let stale_skip_cache = stale_price_skip_cache();
        let _ = fetch_quote_fields_with_caches(
            &ds,
            &cache,
            &stale_skip_cache,
            true,
            std::slice::from_ref(&order),
        )
        .await;

        // Non-limit order: cache should not be populated.
        assert!(cache.get(&order.order_hash()).await.is_none());
    }

    #[rocket::async_test]
    async fn test_limit_cache_does_not_cache_failed_quote() {
        let cache = limit_order_ratio_cache();
        let order = limit_order_for_chain(
            8453,
            "0x00000000000000000000000000000000000000000000000000000000000000dd",
            "fixed-limit",
        );

        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::from([(
                order_hash_key(&order),
                Err(ApiError::Internal("nope".into())),
            )]),
            batched_quotes: HashMap::from([(8453, Err(ApiError::Internal("batch failed".into())))]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let stale_skip_cache = stale_price_skip_cache();
        let fields = fetch_quote_fields_with_caches(
            &ds,
            &cache,
            &stale_skip_cache,
            true,
            std::slice::from_ref(&order),
        )
        .await;

        // io_ratio is the placeholder, no cache write.
        assert_eq!(fields[0].io_ratio, "-");
        assert!(cache.get(&order.order_hash()).await.is_none());
    }

    #[rocket::async_test]
    async fn test_limit_cache_mixed_orders_only_quotes_uncached() {
        let cache = limit_order_ratio_cache();
        let cached_limit = limit_order_for_chain(
            8453,
            "0x0000000000000000000000000000000000000000000000000000000000000111",
            "fixed-limit-buy",
        );
        cache
            .insert(cached_limit.order_hash(), "0.5".to_string())
            .await;
        let regular = mock_order_for_chain(
            8453,
            "0x0000000000000000000000000000000000000000000000000000000000000222",
        );

        let orders = vec![cached_limit.clone(), regular.clone()];

        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        // The batch only sees the regular order; one mock quote.
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::new(),
            batched_quotes: HashMap::from([(8453, Ok(vec![vec![mock_quote("7.0")]]))]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let stale_skip_cache = stale_price_skip_cache();
        let fields =
            fetch_quote_fields_with_caches(&ds, &cache, &stale_skip_cache, true, &orders).await;

        // Position 0: cached limit value.
        assert_eq!(fields[0].io_ratio, "0.5");
        assert!(fields[0].max_output.is_none());
        // Position 1: from the batch.
        assert_eq!(fields[1].io_ratio, "7.0");
        // Batch was called with exactly the regular order.
        let batch_calls = batch_calls.lock().expect("lock");
        assert_eq!(batch_calls.as_slice(), &[(8453, 1)]);
    }

    fn stale_price_quote() -> rain_orderbook_common::raindex_client::order_quotes::RaindexOrderQuote
    {
        serde_json::from_value(json!({
            "pair": { "pairName": "USDC/WETH", "inputIndex": 0, "outputIndex": 0 },
            "blockNumber": 1,
            "data": null,
            "success": false,
            "error": "Execution reverted with error: StalePrice\n"
        }))
        .expect("deserialize stale-price quote")
    }

    #[rocket::async_test]
    async fn test_stale_price_marker_set_after_quote_failure() {
        let limit_cache = limit_order_ratio_cache();
        let stale_skip_cache = stale_price_skip_cache();
        let order = mock_order_for_chain(
            8453,
            "0x0000000000000000000000000000000000000000000000000000000000000abc",
        );

        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::new(),
            batched_quotes: HashMap::from([(8453, Ok(vec![vec![stale_price_quote()]]))]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let _ = fetch_quote_fields_with_caches(
            &ds,
            &limit_cache,
            &stale_skip_cache,
            true, // NYSE open: still quote, but mark on failure
            std::slice::from_ref(&order),
        )
        .await;

        assert!(stale_skip_cache.get(&order.order_hash()).await.is_some());
    }

    #[rocket::async_test]
    async fn test_stale_marked_order_skipped_when_nyse_closed() {
        let limit_cache = limit_order_ratio_cache();
        let stale_skip_cache = stale_price_skip_cache();
        let order = mock_order_for_chain(
            8453,
            "0x0000000000000000000000000000000000000000000000000000000000000def",
        );
        // Pre-mark the order as stale.
        stale_skip_cache.insert(order.order_hash(), ()).await;

        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::new(),
            batched_quotes: HashMap::new(), // Empty: any batch attempt would panic.
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let fields = fetch_quote_fields_with_caches(
            &ds,
            &limit_cache,
            &stale_skip_cache,
            false, // NYSE closed
            std::slice::from_ref(&order),
        )
        .await;

        // Skipped: placeholder fields, no batch call.
        assert_eq!(fields[0].io_ratio, "-");
        assert!(fields[0].max_output.is_none());
        assert!(batch_calls.lock().expect("lock").is_empty());
    }

    #[rocket::async_test]
    async fn test_stale_marked_order_quoted_when_nyse_open() {
        let limit_cache = limit_order_ratio_cache();
        let stale_skip_cache = stale_price_skip_cache();
        let order = mock_order_for_chain(
            8453,
            "0x000000000000000000000000000000000000000000000000000000000000beef",
        );
        // Pre-mark the order: NYSE-open should still quote it.
        stale_skip_cache.insert(order.order_hash(), ()).await;

        let batch_calls = Arc::new(Mutex::new(Vec::new()));
        let single_calls = Arc::new(Mutex::new(Vec::new()));
        let ds = BatchingTestDataSource {
            per_order_quotes: HashMap::new(),
            batched_quotes: HashMap::from([(8453, Ok(vec![vec![mock_quote("3.14")]]))]),
            batch_calls: Arc::clone(&batch_calls),
            single_calls: Arc::clone(&single_calls),
        };

        let fields = fetch_quote_fields_with_caches(
            &ds,
            &limit_cache,
            &stale_skip_cache,
            true, // NYSE open
            std::slice::from_ref(&order),
        )
        .await;

        assert_eq!(fields[0].io_ratio, "3.14");
        assert_eq!(batch_calls.lock().expect("lock").len(), 1);
    }
}
