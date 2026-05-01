use crate::auth::AuthenticatedKey;
use crate::cache::AppCache;
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::types::order::OrderTradeEntry;
use crate::types::trades::{TradesBatchEntry, TradesBatchRequest, TradesBatchResponse};
use alloy::primitives::B256;
use futures::future::join_all;
use rocket::serde::json::Json;
use rocket::State;
use std::time::{Duration, Instant};
use tracing::Instrument;

const TRADES_BY_ORDER_HASH_CACHE_TTL: Duration = Duration::from_secs(60);
const TRADES_BY_ORDER_HASH_CACHE_CAPACITY: u64 = 1_000;
const TRADES_BATCH_MAX_HASHES: usize = 50;

pub(crate) type TradesByOrderHashCache = AppCache<B256, Vec<OrderTradeEntry>>;

pub(crate) fn trades_by_order_hash_cache() -> TradesByOrderHashCache {
    AppCache::new(
        TRADES_BY_ORDER_HASH_CACHE_CAPACITY,
        TRADES_BY_ORDER_HASH_CACHE_TTL,
    )
}

async fn fetch_trades_for_hash(
    ds: &dyn super::TradesDataSource,
    hash: B256,
) -> Result<Vec<OrderTradeEntry>, ApiError> {
    let order = match ds.find_order_by_hash(hash).await? {
        Some(o) => o,
        None => return Ok(vec![]),
    };
    let trades = ds.get_order_trades(&order, None, None).await?;
    Ok(trades.iter().map(super::super::order::map_trade).collect())
}

pub(crate) async fn process_trades_batch(
    ds: &dyn super::TradesDataSource,
    cache: &TradesByOrderHashCache,
    direct_trades: Option<&crate::direct_trades::DirectTradesFetcher>,
    hashes: Vec<B256>,
) -> Result<TradesBatchResponse, ApiError> {
    let total_start = Instant::now();

    let mut cached_map: std::collections::HashMap<B256, Vec<OrderTradeEntry>> =
        std::collections::HashMap::new();
    let mut uncached: Vec<B256> = Vec::new();

    for &hash in &hashes {
        if let Some(trades) = cache.get(&hash).await {
            cached_map.insert(hash, trades);
        } else {
            uncached.push(hash);
        }
    }

    tracing::info!(
        total_hashes = hashes.len(),
        cached = cached_map.len(),
        uncached = uncached.len(),
        "batch trades cache check"
    );

    if !uncached.is_empty() {
        if let Some(fetcher) = direct_trades {
            // Fast path: single batch query via direct SQLite connection
            match fetcher.batch_fetch(&uncached).await {
                Ok(batch_result) => {
                    for &hash in &uncached {
                        let trades = batch_result.get(&hash).cloned().unwrap_or_default();
                        cache.insert(hash, trades.clone()).await;
                        cached_map.insert(hash, trades);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "direct batch trades failed; falling back to library");
                    let results =
                        join_all(uncached.iter().map(|&hash| fetch_trades_for_hash(ds, hash)))
                            .await;
                    for (&hash, result) in uncached.iter().zip(results) {
                        match result {
                            Ok(trades) => {
                                cache.insert(hash, trades.clone()).await;
                                cached_map.insert(hash, trades);
                            }
                            Err(e) => {
                                tracing::warn!(order_hash = %hash, error = %e, "failed to fetch trades for order in batch");
                                cached_map.insert(hash, vec![]);
                            }
                        }
                    }
                }
            }
        } else {
            // Fallback: N parallel queries via library
            let results =
                join_all(uncached.iter().map(|&hash| fetch_trades_for_hash(ds, hash))).await;
            for (&hash, result) in uncached.iter().zip(results) {
                match result {
                    Ok(trades) => {
                        cache.insert(hash, trades.clone()).await;
                        cached_map.insert(hash, trades);
                    }
                    Err(e) => {
                        tracing::warn!(order_hash = %hash, error = %e, "failed to fetch trades for order in batch");
                        cached_map.insert(hash, vec![]);
                    }
                }
            }
        }
    }

    let entries = hashes
        .iter()
        .map(|hash| TradesBatchEntry {
            order_hash: *hash,
            trades: cached_map.remove(hash).unwrap_or_default(),
        })
        .collect();

    tracing::info!(
        total_duration_ms = total_start.elapsed().as_millis(),
        total_hashes = hashes.len(),
        "batch trades request processed"
    );

    Ok(TradesBatchResponse { orders: entries })
}

#[utoipa::path(
    post,
    path = "/v1/trades/batch",
    tag = "Trades",
    security(("basicAuth" = [])),
    request_body = TradesBatchRequest,
    responses(
        (status = 200, description = "Trades grouped by order hash", body = TradesBatchResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[post("/batch", data = "<body>")]
pub async fn post_trades_batch(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    shared_raindex: &State<crate::raindex::SharedRaindexProvider>,
    trades_by_order_hash_cache: &State<TradesByOrderHashCache>,
    direct_trades: &State<Option<crate::direct_trades::DirectTradesFetcher>>,
    span: TracingSpan,
    body: Json<TradesBatchRequest>,
) -> Result<Json<TradesBatchResponse>, ApiError> {
    async move {
        tracing::info!(
            hash_count = body.order_hashes.len(),
            "batch trades request received"
        );

        if body.order_hashes.is_empty() {
            return Ok(Json(TradesBatchResponse { orders: vec![] }));
        }

        if body.order_hashes.len() > TRADES_BATCH_MAX_HASHES {
            return Err(ApiError::BadRequest(format!(
                "maximum {} order hashes per batch request",
                TRADES_BATCH_MAX_HASHES
            )));
        }

        let raindex = shared_raindex.read().await;
        let ds = super::RaindexTradesDataSource {
            client: raindex.client(),
        };
        let response = process_trades_batch(
            &ds,
            trades_by_order_hash_cache,
            direct_trades.inner().as_ref(),
            body.into_inner().order_hashes,
        )
        .await?;
        Ok(Json(response))
    }
    .instrument(span.0)
    .await
}
