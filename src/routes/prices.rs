use crate::auth::AuthenticatedKey;
use crate::db::market_price_history::{
    list_market_price_history, list_market_prices_at_or_before, MarketPriceSnapshot,
};
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use crate::market_price::{
    configured_price_markets, find_market_token, normalize_address, resolve_required_price_market,
    unix_now, MarketPriceConfig, MarketPriceState, MarketToken, PriceMarket,
};
use crate::types::common::ValidatedAddress;
use crate::wrap_ratio::{
    persist_wrap_ratio_snapshots_best_effort, read_wrap_ratio_responses_for_addresses,
    wrap_ratio_values_from_responses, WrapRatioValue,
};
use futures::{stream, StreamExt, TryStreamExt};
use rain_math_float::Float;
use rocket::form::FromForm;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::{Div, Mul, Sub};
use tracing::Instrument;
use utoipa::{IntoParams, ToSchema};

const CHANGE_WINDOW_SECONDS: i64 = 24 * 60 * 60;
const MAX_HISTORY_POINTS: u64 = 10_081;
const MAX_PRICE_QUERY_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, FromForm, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct PricesParams {
    #[field(name = "chainId")]
    #[param(example = 8453)]
    chain_id: Option<u32>,
    #[field(name = "at")]
    #[param(example = 1784800000)]
    at: Option<i64>,
}

#[derive(Debug, Clone, FromForm, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct PriceHistoryParams {
    #[field(name = "chainId")]
    #[param(example = 8453)]
    chain_id: Option<u32>,
    #[field(name = "startTime")]
    #[param(example = 1784195200)]
    start_time: Option<i64>,
    #[field(name = "endTime")]
    #[param(example = 1784800000)]
    end_time: Option<i64>,
    #[field(name = "interval")]
    #[param(example = 900)]
    interval: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MarketPriceSource {
    Live,
    Cached,
    Historical,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarketPriceResponse {
    #[schema(example = 8453)]
    chain_id: u32,
    #[schema(example = "0xfb5b41acdba20a3230f84be995173cfb98b8d6e7")]
    asset_address: String,
    #[schema(example = "wtNVDA")]
    symbol: String,
    #[schema(example = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")]
    quote_address: String,
    #[schema(nullable = true, example = "123.4")]
    best_bid: Option<String>,
    #[schema(nullable = true, example = "123.6")]
    best_ask: Option<String>,
    #[schema(nullable = true, example = "123.5")]
    midpoint: Option<String>,
    source: MarketPriceSource,
    #[schema(nullable = true, example = 1784800000)]
    observed_at: Option<i64>,
    #[schema(nullable = true, example = "1.42")]
    change_24h_percent: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarketPricesResponse {
    data: Vec<MarketPriceResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarketPriceHistoryPoint {
    #[schema(example = "123.4")]
    best_bid: String,
    #[schema(example = "123.6")]
    best_ask: String,
    #[schema(example = "123.5")]
    midpoint: String,
    #[schema(example = 1784800000)]
    observed_at: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarketPriceHistoryResponse {
    #[schema(example = 8453)]
    chain_id: u32,
    #[schema(example = "0xfb5b41acdba20a3230f84be995173cfb98b8d6e7")]
    asset_address: String,
    #[schema(example = "wtNVDA")]
    symbol: String,
    #[schema(example = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")]
    quote_address: String,
    #[schema(example = 1784195200)]
    start_time: i64,
    #[schema(example = 1784800000)]
    end_time: i64,
    #[schema(example = 900)]
    interval: u64,
    points: Vec<MarketPriceHistoryPoint>,
}

#[utoipa::path(
    get,
    path = "/v2/prices",
    tag = "Prices",
    security(("basicAuth" = [])),
    params(PricesParams),
    responses(
        (status = 200, description = "Latest or historical ST0x midpoint prices", body = MarketPricesResponse),
        (status = 400, description = "Invalid query parameters", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/?<params..>")]
pub async fn get_prices(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    state: &State<MarketPriceState>,
    params: PricesParams,
) -> Result<Json<MarketPricesResponse>, ApiError> {
    async move {
        let config = &state.config;
        tracing::info!(params = ?params, "request received");
        let now = unix_now()?;
        let query_time = params.at.unwrap_or(now);
        if query_time < 0 {
            return Err(ApiError::BadRequest(
                "at must be a non-negative Unix timestamp".into(),
            ));
        }

        let markets = configured_price_markets(&state.shared_raindex, params.chain_id).await?;
        let retention_seconds = duration_seconds_i64(config.retention, "retention")?;
        let retained_start = now.saturating_sub(retention_seconds);

        let historical = params.at.is_some();
        let market_count = markets.len();
        let market_responses = stream::iter(markets.into_iter().map(|market| async move {
            price_responses_for_market(
                state,
                config,
                &market,
                retained_start,
                query_time,
                now,
                historical,
            )
            .await
        }))
        .buffer_unordered(MAX_PRICE_QUERY_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;
        let mut data = market_responses.into_iter().flatten().collect::<Vec<_>>();

        data.sort_by(|left, right| {
            left.chain_id
                .cmp(&right.chain_id)
                .then_with(|| left.asset_address.cmp(&right.asset_address))
        });

        tracing::info!(
            price_count = data.len(),
            market_count,
            historical,
            "returning market prices"
        );
        Ok(Json(MarketPricesResponse { data }))
    }
    .instrument(span.0)
    .await
}

async fn price_responses_for_market(
    state: &MarketPriceState,
    config: &MarketPriceConfig,
    market: &PriceMarket,
    retained_start: i64,
    query_time: i64,
    now: i64,
    historical: bool,
) -> Result<Vec<MarketPriceResponse>, ApiError> {
    let quote_address = normalize_address(market.quote_token_address);
    let current_query = list_market_prices_at_or_before(
        &state.pool,
        i64::from(market.chain_id),
        &quote_address,
        retained_start,
        query_time,
    );
    let (rows, previous) = if historical {
        (current_query.await.map_err(database_error)?, Vec::new())
    } else {
        tokio::try_join!(
            async { current_query.await.map_err(database_error) },
            async {
                list_market_prices_at_or_before(
                    &state.pool,
                    i64::from(market.chain_id),
                    &quote_address,
                    retained_start,
                    now.saturating_sub(CHANGE_WINDOW_SECONDS),
                )
                .await
                .map_err(database_error)
            }
        )?
    };
    let rows_by_asset = rows
        .into_iter()
        .map(|row| (row.asset_token_address.clone(), row))
        .collect::<HashMap<_, _>>();
    let previous_by_asset = previous
        .into_iter()
        .map(|row| (row.asset_token_address.clone(), row))
        .collect::<HashMap<_, _>>();
    let required_ratios = market
        .tokens
        .iter()
        .filter(|token| {
            [&rows_by_asset, &previous_by_asset]
                .into_iter()
                .any(|rows| {
                    latest_row_for_token(rows, token).is_some_and(|row| {
                        row.asset_token_address != normalize_address(token.canonical_address)
                    })
                })
        })
        .map(|token| token.canonical_address)
        .collect::<Vec<_>>();
    let current_wrap_ratios = current_wrap_ratios(
        state,
        market,
        &required_ratios,
        rows_by_asset.values().chain(previous_by_asset.values()),
        now,
    )
    .await?;

    market
        .tokens
        .iter()
        .map(|token| {
            let row = latest_row_for_token(&rows_by_asset, token)
                .map(|row| normalize_snapshot_for_token(row, token, &current_wrap_ratios))
                .transpose()?;
            let previous = latest_row_for_token(&previous_by_asset, token)
                .map(|row| normalize_snapshot_for_token(row, token, &current_wrap_ratios))
                .transpose()?;
            let change_24h_percent = if historical {
                None
            } else {
                match (&row, &previous) {
                    (Some(current), Some(previous)) => {
                        match percentage_change(&current.midpoint, &previous.midpoint) {
                            Ok(change) => Some(change),
                            Err(error) => {
                                tracing::warn!(
                                    chain_id = market.chain_id,
                                    asset_address = %token.canonical_address,
                                    error = %error,
                                    "failed to compute 24h market price change; omitting value"
                                );
                                None
                            }
                        }
                    }
                    _ => None,
                }
            };
            price_response(
                config,
                market,
                token,
                row.as_ref(),
                historical,
                now,
                change_24h_percent,
            )
        })
        .collect()
}

#[utoipa::path(
    get,
    path = "/v2/prices/{address}/history",
    tag = "Prices",
    security(("basicAuth" = [])),
    params(
        ("address" = String, Path, description = "Wrapped, unwrapped, or legacy ST0x token address"),
        PriceHistoryParams
    ),
    responses(
        (status = 200, description = "Retained midpoint price history", body = MarketPriceHistoryResponse),
        (status = 400, description = "Invalid query parameters", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 404, description = "ST0x token not found", body = ApiErrorResponse),
        (status = 422, description = "Invalid token address", body = ApiErrorResponse),
        (status = 429, description = "Rate limited", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/<address>/history?<params..>")]
pub async fn get_price_history(
    _global: GlobalRateLimit,
    _key: AuthenticatedKey,
    span: TracingSpan,
    state: &State<MarketPriceState>,
    address: ValidatedAddress,
    params: PriceHistoryParams,
) -> Result<Json<MarketPriceHistoryResponse>, ApiError> {
    async move {
        let config = &state.config;
        tracing::info!(address = %address.0, params = ?params, "request received");
        let now = unix_now()?;
        let retention_seconds = duration_seconds_i64(config.retention, "retention")?;
        let sample_interval = config.sample_interval.as_secs();
        let requested_interval = params
            .interval
            .unwrap_or(sample_interval)
            .max(sample_interval);
        if requested_interval > config.retention.as_secs() {
            return Err(ApiError::BadRequest(
                "interval cannot exceed the retained price history window".into(),
            ));
        }

        let end_time = params.end_time.unwrap_or(now).min(now);
        let retained_start = now.saturating_sub(retention_seconds);
        let start_time = params
            .start_time
            .unwrap_or(retained_start)
            .max(retained_start);
        if start_time < 0 || end_time < 0 {
            return Err(ApiError::BadRequest(
                "price history timestamps must be non-negative".into(),
            ));
        }
        if start_time > end_time {
            return Err(ApiError::BadRequest(
                "startTime must be less than or equal to endTime".into(),
            ));
        }
        let interval = effective_history_interval(requested_interval, start_time, end_time)?;
        let interval_i64 = i64::try_from(interval)
            .map_err(|_| ApiError::BadRequest("interval is too large".into()))?;

        let market = resolve_required_price_market(&state.shared_raindex, params.chain_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("ST0x token not found".into()))?;
        let token = find_market_token(&market.tokens, address.0)
            .ok_or_else(|| ApiError::NotFound("ST0x token not found".into()))?;
        let canonical_address = normalize_address(token.canonical_address);
        let asset_addresses = token
            .variants
            .iter()
            .copied()
            .map(normalize_address)
            .collect::<Vec<_>>();
        let quote_address = normalize_address(market.quote_token_address);
        let rows = list_market_price_history(
            &state.pool,
            i64::from(market.chain_id),
            &asset_addresses,
            &quote_address,
            start_time,
            end_time,
            interval_i64,
        )
        .await
        .map_err(database_error)?;
        let requires_ratio = rows
            .iter()
            .any(|row| row.asset_token_address != normalize_address(token.canonical_address));
        let required_ratios = requires_ratio
            .then_some(token.canonical_address)
            .into_iter()
            .collect::<Vec<_>>();
        let known_rows = if requires_ratio {
            list_market_prices_at_or_before(
                &state.pool,
                i64::from(market.chain_id),
                &quote_address,
                retained_start,
                now,
            )
            .await
            .map_err(database_error)?
        } else {
            Vec::new()
        };
        let current_wrap_ratios =
            current_wrap_ratios(state, &market, &required_ratios, known_rows.iter(), now).await?;
        let points = rows
            .into_iter()
            .map(|row| {
                let row = normalize_snapshot_for_token(&row, token, &current_wrap_ratios)?;
                Ok(MarketPriceHistoryPoint {
                    best_bid: row.best_bid,
                    best_ask: row.best_ask,
                    midpoint: row.midpoint,
                    observed_at: row.observed_at,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;

        tracing::info!(
            asset_address = %canonical_address,
            point_count = points.len(),
            "returning market price history"
        );
        Ok(Json(MarketPriceHistoryResponse {
            chain_id: market.chain_id,
            asset_address: canonical_address,
            symbol: token.symbol.clone(),
            quote_address,
            start_time,
            end_time,
            interval,
            points,
        }))
    }
    .instrument(span.0)
    .await
}

fn price_response(
    config: &MarketPriceConfig,
    market: &PriceMarket,
    token: &MarketToken,
    row: Option<&MarketPriceSnapshot>,
    historical: bool,
    now: i64,
    change_24h_percent: Option<String>,
) -> Result<MarketPriceResponse, ApiError> {
    let source = match row {
        None => MarketPriceSource::Unavailable,
        Some(_) if historical => MarketPriceSource::Historical,
        Some(row) => {
            let live_window =
                duration_seconds_i64(config.sample_interval, "sample interval")?.saturating_mul(2);
            if now.saturating_sub(row.observed_at) <= live_window {
                MarketPriceSource::Live
            } else {
                MarketPriceSource::Cached
            }
        }
    };

    Ok(MarketPriceResponse {
        chain_id: market.chain_id,
        asset_address: normalize_address(token.canonical_address),
        symbol: token.symbol.clone(),
        quote_address: normalize_address(market.quote_token_address),
        best_bid: row.map(|row| row.best_bid.clone()),
        best_ask: row.map(|row| row.best_ask.clone()),
        midpoint: row.map(|row| row.midpoint.clone()),
        source,
        observed_at: row.map(|row| row.observed_at),
        change_24h_percent,
    })
}

fn latest_row_for_token<'a>(
    rows_by_asset: &'a HashMap<String, MarketPriceSnapshot>,
    token: &MarketToken,
) -> Option<&'a MarketPriceSnapshot> {
    token
        .variants
        .iter()
        .map(|address| normalize_address(*address))
        .filter_map(|address| rows_by_asset.get(&address))
        .max_by_key(|row| row.observed_at)
}

async fn current_wrap_ratios<'a>(
    state: &MarketPriceState,
    market: &PriceMarket,
    share_addresses: &[alloy::primitives::Address],
    known_rows: impl IntoIterator<Item = &'a MarketPriceSnapshot>,
    now: i64,
) -> Result<HashMap<alloy::primitives::Address, WrapRatioValue>, ApiError> {
    let mut requested = share_addresses.to_vec();
    requested.sort_unstable();
    requested.dedup();
    if requested.is_empty() {
        return Ok(HashMap::new());
    }

    let freshness_window = state.config.sample_interval.as_secs().saturating_mul(2);
    let mut ratios = known_rows
        .into_iter()
        .filter_map(|row| {
            let share_address = row.asset_token_address.parse().ok()?;
            if !requested.contains(&share_address)
                || !u64::try_from(now.saturating_sub(row.observed_at))
                    .is_ok_and(|age| age <= freshness_window)
            {
                return None;
            }
            Some((
                share_address,
                WrapRatioValue {
                    share_address,
                    assets_per_share: row.assets_per_share.clone(),
                },
            ))
        })
        .collect::<HashMap<_, _>>();
    let missing = requested
        .into_iter()
        .filter(|address| !ratios.contains_key(address))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let responses =
            read_wrap_ratio_responses_for_addresses(&market.registry_tokens, &missing).await?;
        persist_wrap_ratio_snapshots_best_effort(&state.pool, &responses).await;
        ratios.extend(wrap_ratio_values_from_responses(responses));
    }
    Ok(ratios)
}

fn normalize_snapshot_for_token(
    row: &MarketPriceSnapshot,
    token: &MarketToken,
    current_wrap_ratios: &HashMap<alloy::primitives::Address, WrapRatioValue>,
) -> Result<MarketPriceSnapshot, ApiError> {
    let canonical_address = normalize_address(token.canonical_address);
    if row.asset_token_address == canonical_address {
        return Ok(row.clone());
    }

    let current_ratio = current_wrap_ratios
        .get(&token.canonical_address)
        .ok_or_else(|| {
            tracing::error!(
                canonical_address = %token.canonical_address,
                stored_address = %row.asset_token_address,
                "missing current denomination for retained market price alias"
            );
            ApiError::Internal("failed to normalize retained market price".into())
        })?;
    let current_assets_per_share =
        Float::parse(current_ratio.assets_per_share.clone()).map_err(float_error)?;
    let stored_assets_per_share =
        Float::parse(row.assets_per_share.clone()).map_err(float_error)?;
    let multiplier = current_assets_per_share
        .div(stored_assets_per_share)
        .map_err(float_error)?;
    let normalize = |value: &str| {
        Float::parse(value.to_string())
            .and_then(|value| value.mul(multiplier))
            .and_then(Float::format)
            .map_err(float_error)
    };

    Ok(MarketPriceSnapshot {
        chain_id: row.chain_id,
        asset_token_address: canonical_address,
        quote_token_address: row.quote_token_address.clone(),
        best_bid: normalize(&row.best_bid)?,
        best_ask: normalize(&row.best_ask)?,
        midpoint: normalize(&row.midpoint)?,
        assets_per_share: current_ratio.assets_per_share.clone(),
        observed_at: row.observed_at,
    })
}

fn percentage_change(current: &str, previous: &str) -> Result<String, ApiError> {
    let current = Float::parse(current.to_string()).map_err(float_error)?;
    let previous = Float::parse(previous.to_string()).map_err(float_error)?;
    let zero = Float::zero().map_err(float_error)?;
    if previous.eq(zero).map_err(float_error)? {
        return Err(ApiError::Internal(
            "stored market price cannot be zero".into(),
        ));
    }
    let one = Float::parse("1".to_string()).map_err(float_error)?;
    let hundred = Float::parse("100".to_string()).map_err(float_error)?;
    current
        .div(previous)
        .and_then(|ratio| ratio.sub(one))
        .and_then(|change| change.mul(hundred))
        .and_then(Float::format)
        .map_err(float_error)
}

fn effective_history_interval(
    requested_interval: u64,
    start_time: i64,
    end_time: i64,
) -> Result<u64, ApiError> {
    let window_seconds = u64::try_from(end_time.saturating_sub(start_time))
        .map_err(|_| ApiError::BadRequest("price history window is too large".into()))?;
    Ok(requested_interval.max(
        window_seconds
            .div_ceil(MAX_HISTORY_POINTS.saturating_sub(1))
            .max(1),
    ))
}

fn duration_seconds_i64(duration: std::time::Duration, field: &str) -> Result<i64, ApiError> {
    i64::try_from(duration.as_secs())
        .map_err(|_| ApiError::Internal(format!("market price {field} is too large")))
}

fn database_error(error: sqlx::Error) -> ApiError {
    tracing::error!(error = %error, "failed to query market prices");
    ApiError::Internal("failed to query market prices".into())
}

fn float_error(error: rain_math_float::FloatError) -> ApiError {
    tracing::error!(error = %error, "failed to calculate market price change");
    ApiError::Internal("failed to calculate market price change".into())
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_prices, get_price_history]
}

pub fn routes_v2() -> Vec<Route> {
    routes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::market_price_history::{insert_market_price_snapshots, NewMarketPriceSnapshot};
    use crate::db::DbPool;
    use crate::test_helpers::{
        basic_auth_header, mock_raindex_registry_url_with_settings_and_tokens, seed_api_key,
        TestClientBuilder,
    };
    use alloy::primitives::{address, Address};
    use rocket::http::{Header, Status};

    const ASSET: Address = address!("1111111111111111111111111111111111111111");
    const UNWRAPPED: Address = address!("2222222222222222222222222222222222222222");
    const LEGACY: Address = address!("abcdefabcdefabcdefabcdefabcdefabcdefabcd");
    const QUOTE: Address = address!("833589fcd6edb6e08f4c7c32d4f71b54bda02913");
    const ASSET_TWO: Address = address!("4444444444444444444444444444444444444444");
    const QUOTE_TWO: Address = address!("5555555555555555555555555555555555555555");

    #[test]
    fn calculates_exact_percentage_change() {
        assert_eq!(
            percentage_change("125", "100").expect("percentage change"),
            "25"
        );
        assert_eq!(
            percentage_change("90", "100").expect("percentage change"),
            "-10"
        );
    }

    #[test]
    fn history_interval_caps_response_point_count() {
        let interval = effective_history_interval(60, 0, 1_209_600).expect("effective interval");
        assert_eq!(interval, 120);
        assert!(1_209_600 / interval < MAX_HISTORY_POINTS);
    }

    async fn price_client() -> rocket::local::asynchronous::Client {
        let settings = r#"version: 6
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://example.com/subgraph
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
        let remote_tokens = format!(
            r#"{{
  "name": "Market Price Tokens",
  "timestamp": "2026-07-23T00:00:00.000Z",
  "version": {{ "major": 1, "minor": 0, "patch": 0 }},
  "tokens": [
    {{
      "chainId": 8453,
      "address": "{ASSET:#x}",
      "decimals": 18,
      "name": "Wrapped Test ST0x",
      "symbol": "wtTEST",
      "extensions": {{
        "category": "ST0x",
        "unwrappedAddress": "{UNWRAPPED:#x}",
        "legacyAddress": "{LEGACY:#x}"
      }}
    }},
    {{
      "chainId": 8453,
      "address": "{QUOTE:#x}",
      "decimals": 6,
      "name": "USD Coin",
      "symbol": "USDC"
    }}
  ]
}}"#
        );
        let registry_url =
            mock_raindex_registry_url_with_settings_and_tokens(settings, &remote_tokens).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load market price registry");
        TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await
    }

    async fn multichain_price_client() -> rocket::local::asynchronous::Client {
        let settings = r#"version: 6
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
  optimism:
    rpcs:
      - https://mainnet.optimism.io
    chain-id: 10
    currency: ETH
subgraphs:
  base: https://example.com/base-subgraph
  optimism: https://example.com/optimism-subgraph
raindexes:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
  optimism:
    address: 0x1111111111111111111111111111111111111111
    network: optimism
    subgraph: optimism
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
  optimism:
    address: 0x2222222222222222222222222222222222222222
    network: optimism
using-tokens-from:
  - __TOKENS_URL__
"#;
        let remote_tokens = format!(
            r#"{{
  "name": "Multichain Market Price Tokens",
  "timestamp": "2026-07-23T00:00:00.000Z",
  "version": {{ "major": 1, "minor": 0, "patch": 0 }},
  "tokens": [
    {{
      "chainId": 8453,
      "address": "{ASSET:#x}",
      "decimals": 18,
      "name": "Wrapped Base Test ST0x",
      "symbol": "wtBASE",
      "extensions": {{ "category": "ST0x" }}
    }},
    {{
      "chainId": 8453,
      "address": "{QUOTE:#x}",
      "decimals": 6,
      "name": "USD Coin",
      "symbol": "USDC"
    }},
    {{
      "chainId": 10,
      "address": "{ASSET_TWO:#x}",
      "decimals": 18,
      "name": "Wrapped Optimism Test ST0x",
      "symbol": "wtOP",
      "extensions": {{ "category": "ST0x" }}
    }},
    {{
      "chainId": 10,
      "address": "{QUOTE_TWO:#x}",
      "decimals": 6,
      "name": "USD Coin",
      "symbol": "usdc"
    }}
  ]
}}"#
        );
        let registry_url =
            mock_raindex_registry_url_with_settings_and_tokens(settings, &remote_tokens).await;
        let config = crate::raindex::RaindexProvider::load(&registry_url, None)
            .await
            .expect("load multichain market price registry");
        TestClientBuilder::new()
            .raindex_config(config)
            .build()
            .await
    }

    async fn seed_market_price(
        client: &rocket::local::asynchronous::Client,
        chain_id: u32,
        asset: Address,
        quote: Address,
        observed_at: i64,
        midpoint: &str,
    ) {
        seed_market_price_with_ratio(client, chain_id, asset, quote, observed_at, midpoint, "1")
            .await;
    }

    async fn seed_market_price_with_ratio(
        client: &rocket::local::asynchronous::Client,
        chain_id: u32,
        asset: Address,
        quote: Address,
        observed_at: i64,
        midpoint: &str,
        assets_per_share: &str,
    ) {
        let pool = client.rocket().state::<DbPool>().expect("database pool");
        insert_market_price_snapshots(
            pool,
            &[NewMarketPriceSnapshot {
                chain_id: i64::from(chain_id),
                asset_token_address: normalize_address(asset),
                quote_token_address: normalize_address(quote),
                best_bid: "99".to_string(),
                best_ask: "101".to_string(),
                midpoint: midpoint.to_string(),
                assets_per_share: assets_per_share.to_string(),
                observed_at,
            }],
        )
        .await
        .expect("seed market price");
    }

    async fn seed_price(
        client: &rocket::local::asynchronous::Client,
        observed_at: i64,
        midpoint: &str,
    ) {
        seed_market_price(client, 8453, ASSET, QUOTE, observed_at, midpoint).await;
    }

    async fn authorized_get<'a>(
        client: &'a rocket::local::asynchronous::Client,
        path: &'a str,
    ) -> rocket::local::asynchronous::LocalResponse<'a> {
        let (key_id, secret) = seed_api_key(client).await;
        client
            .get(path)
            .header(Header::new(
                "Authorization",
                basic_auth_header(&key_id, &secret),
            ))
            .dispatch()
            .await
    }

    #[rocket::async_test]
    async fn latest_prices_use_canonical_lowercase_and_camel_case_fields() {
        let client = price_client().await;
        let now = unix_now().expect("current time");
        seed_price(&client, now - 60, "100").await;

        let response = authorized_get(&client, "/v1/prices?chainId=8453").await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("price response");
        assert_eq!(body["data"][0]["assetAddress"], normalize_address(ASSET));
        assert_eq!(body["data"][0]["quoteAddress"], normalize_address(QUOTE));
        assert_eq!(body["data"][0]["midpoint"], "100");
        assert!(body["data"][0].get("observedAt").is_some());
        assert!(body["data"][0].get("change24hPercent").is_some());
        assert!(body["data"][0].get("asset_address").is_none());
    }

    #[rocket::async_test]
    async fn invalid_previous_midpoint_omits_change_without_failing_prices() {
        let client = price_client().await;
        let now = unix_now().expect("current time");
        seed_price(&client, now - CHANGE_WINDOW_SECONDS - 60, "0").await;
        seed_price(&client, now - 60, "100").await;

        let response = authorized_get(&client, "/v1/prices?chainId=8453").await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("price response");
        assert_eq!(body["data"][0]["midpoint"], "100");
        assert!(body["data"][0]["change24hPercent"].is_null());
    }

    #[rocket::async_test]
    async fn history_accepts_legacy_address_and_returns_canonical_token() {
        let client = price_client().await;
        let now = unix_now().expect("current time");
        seed_price(&client, now - 60, "100").await;

        let legacy_mixed_case = format!("{LEGACY:#x}")
            .chars()
            .enumerate()
            .map(|(index, character)| {
                if index > 1 && index % 2 == 0 {
                    character.to_ascii_uppercase()
                } else {
                    character
                }
            })
            .collect::<String>();
        let path = format!(
            "/v1/prices/{legacy_mixed_case}/history?chainId=8453&startTime={}&endTime={now}",
            now - 120
        );
        let response = authorized_get(&client, &path).await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("history response");
        assert_eq!(body["assetAddress"], normalize_address(ASSET));
        assert_eq!(body["points"][0]["midpoint"], "100");
        assert_eq!(body["interval"], 60);
    }

    #[rocket::async_test]
    async fn retained_legacy_rows_remain_visible_after_canonical_rotation() {
        let client = price_client().await;
        let now = unix_now().expect("current time");
        seed_market_price_with_ratio(&client, 8453, ASSET, QUOTE, now - 60, "200", "4").await;
        seed_market_price_with_ratio(&client, 8453, LEGACY, QUOTE, now - 30, "98", "2").await;

        let latest = authorized_get(&client, "/v1/prices?chainId=8453").await;
        assert_eq!(latest.status(), Status::Ok);
        let latest_body: serde_json::Value =
            latest.into_json().await.expect("latest price response");
        assert_eq!(
            latest_body["data"][0]["assetAddress"],
            normalize_address(ASSET)
        );
        assert_eq!(latest_body["data"][0]["midpoint"], "196");

        let path = format!(
            "/v1/prices/{ASSET:#x}/history?chainId=8453&startTime={}&endTime={now}",
            now - 120
        );
        let history = authorized_get(&client, &path).await;
        assert_eq!(history.status(), Status::Ok);
        let history_body: serde_json::Value = history.into_json().await.expect("history response");
        assert_eq!(history_body["assetAddress"], normalize_address(ASSET));
        assert_eq!(history_body["points"][0]["midpoint"], "196");
    }

    #[rocket::async_test]
    async fn history_keeps_latest_observation_per_interval() {
        let client = price_client().await;
        let now = unix_now().expect("current time");
        let start_time = now - 180;
        seed_price(&client, start_time + 10, "100").await;
        seed_price(&client, start_time + 50, "101").await;
        seed_price(&client, start_time + 110, "102").await;

        let path = format!(
            "/v1/prices/{ASSET:#x}/history?chainId=8453&startTime={start_time}&endTime={now}&interval=60"
        );
        let response = authorized_get(&client, &path).await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("history response");
        let points = body["points"].as_array().expect("history points");
        assert_eq!(points.len(), 2);
        assert_eq!(points[0]["midpoint"], "101");
        assert_eq!(points[1]["midpoint"], "102");
    }

    #[rocket::async_test]
    async fn prices_reject_unsupported_chain() {
        let client = price_client().await;
        let response = authorized_get(&client, "/v1/prices?chainId=1").await;
        assert_eq!(response.status(), Status::BadRequest);
    }

    #[rocket::async_test]
    async fn prices_without_chain_return_all_registry_markets() {
        let client = multichain_price_client().await;
        let now = unix_now().expect("current time");
        seed_market_price(&client, 8453, ASSET, QUOTE, now - 60, "100").await;
        seed_market_price(&client, 10, ASSET_TWO, QUOTE_TWO, now - 60, "200").await;

        let response = authorized_get(&client, "/v1/prices").await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("price response");
        let data = body["data"].as_array().expect("price data");
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["chainId"], 10);
        assert_eq!(data[0]["quoteAddress"], normalize_address(QUOTE_TWO));
        assert_eq!(data[1]["chainId"], 8453);
        assert_eq!(data[1]["quoteAddress"], normalize_address(QUOTE));
    }

    #[rocket::async_test]
    async fn history_requires_chain_when_registry_has_multiple_networks() {
        let client = multichain_price_client().await;
        let path = format!("/v1/prices/{ASSET:#x}/history");
        let response = authorized_get(&client, &path).await;
        assert_eq!(response.status(), Status::BadRequest);
    }
}
