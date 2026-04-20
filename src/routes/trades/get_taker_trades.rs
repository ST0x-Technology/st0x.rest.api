use crate::auth::AuthenticatedKey;
use crate::cache::AppCache;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::common::ValidatedAddress;
use crate::types::trades::{
    TakerTradesResponse, TradesByTxResponse, TradesPagination, TradesPaginationParams,
};
use alloy::primitives::{Address, B256};
use rocket::serde::json::Json;
use rocket::State;
use std::time::Duration;
use tracing::Instrument;

const TAKER_TX_HASH_CACHE_TTL: Duration = Duration::from_secs(15);
const TAKER_TX_HASH_CACHE_CAPACITY: u64 = 1_000;

pub(crate) type TakerTradesTxHashCache = AppCache<Address, Vec<(B256, u64)>>;

pub(crate) fn taker_trades_tx_hash_cache() -> TakerTradesTxHashCache {
    AppCache::new(TAKER_TX_HASH_CACHE_CAPACITY, TAKER_TX_HASH_CACHE_TTL)
}

pub(crate) async fn process_get_taker_trades(
    ds: &dyn super::TradesDataSource,
    direct_trades: Option<&crate::direct_trades::DirectTradesFetcher>,
    trades_by_tx_cache: &super::TradesByTxCache,
    taker_tx_cache: &TakerTradesTxHashCache,
    sender: Address,
    params: TradesPaginationParams,
) -> Result<TakerTradesResponse, ApiError> {
    // Step 1: Get tx hashes (cached)
    let tx_hashes = match direct_trades {
        Some(fetcher) => taker_tx_cache
            .get_or_try_insert(sender, || async {
                fetcher.fetch_taker_tx_hashes(&sender).await
            })
            .await
            .map_err(ApiError::from)?,
        None => {
            tracing::warn!("direct trades fetcher unavailable; returning empty taker trades");
            return Ok(TakerTradesResponse {
                market_orders: vec![],
                pagination: TradesPagination {
                    page: 1,
                    page_size: params.page_size.unwrap_or(20),
                    total_trades: 0,
                    total_pages: 0,
                    has_more: false,
                },
            });
        }
    };

    // Step 2: Paginate
    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20);
    let total = tx_hashes.len() as u64;
    let total_pages = if page_size == 0 {
        0
    } else {
        total.div_ceil(u64::from(page_size))
    };
    let offset = (u64::from(page.saturating_sub(1)) * u64::from(page_size)) as usize;
    let page_hashes: Vec<B256> = if offset >= tx_hashes.len() {
        vec![]
    } else {
        let end = std::cmp::min(offset + page_size as usize, tx_hashes.len());
        tx_hashes[offset..end].iter().map(|(h, _)| *h).collect()
    };

    // Step 3: Resolve each tx via existing cached trade-by-tx lookup
    let mut market_orders = Vec::with_capacity(page_hashes.len());
    for tx_hash in page_hashes {
        match super::get_cached_trades_by_tx(trades_by_tx_cache, ds, tx_hash, None).await {
            Ok(tx_trades) => market_orders.push(tx_trades),
            Err(e) => {
                tracing::warn!(tx_hash = %tx_hash, error = %e, "failed to resolve taker tx; skipping");
            }
        }
    }

    Ok(TakerTradesResponse {
        market_orders,
        pagination: TradesPagination {
            page,
            page_size,
            total_trades: total,
            total_pages,
            has_more: u64::from(page) < total_pages,
        },
    })
}

#[utoipa::path(
    get,
    path = "/v1/trades/taker/{address}",
    tag = "Trades",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Taker address"),
        TradesPaginationParams,
    ),
    responses(
        (status = 200, description = "Paginated list of market orders (taker transactions)", body = TakerTradesResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/taker/<address>?<params..>")]
pub async fn get_taker_trades(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    trades_by_tx_cache: &State<super::TradesByTxCache>,
    taker_tx_cache: &State<TakerTradesTxHashCache>,
    direct_trades: &State<Option<crate::direct_trades::DirectTradesFetcher>>,
    span: TracingSpan,
    address: ValidatedAddress,
    params: TradesPaginationParams,
) -> Result<Json<TakerTradesResponse>, ApiError> {
    async move {
        tracing::info!(address = ?address, params = ?params, "taker trades request received");
        let raindex = shared_raindex.read().await;
        let ds = super::RaindexTradesDataSource {
            client: raindex.client(),
        };
        let response = process_get_taker_trades(
            &ds,
            direct_trades.inner().as_ref(),
            trades_by_tx_cache,
            taker_tx_cache,
            address.0,
            params,
        )
        .await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}
