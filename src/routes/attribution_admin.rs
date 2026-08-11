use crate::attribution_reporting::report as attribution_report;
use crate::auth::AdminKey;
use crate::db::{attribution as attribution_db, DbPool};
use crate::error::{ApiError, ApiErrorResponse};
use crate::fairings::{GlobalRateLimit, TracingSpan};
use alloy::primitives::{Address, B256};
use rain_math_float::Float;
use rocket::form::FromForm;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::Serialize;
use std::ops::Sub;
use std::str::FromStr;
use tracing::Instrument;
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, FromForm, IntoParams)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct AttributionVolumeParams {
    #[field(name = "apiKeyHash")]
    pub api_key_hash: Option<String>,
    #[field(name = "startBlock")]
    pub start_block: Option<u64>,
    #[field(name = "endBlock")]
    pub end_block: Option<u64>,
    #[field(name = "afterApiKeyHash")]
    pub after_api_key_hash: Option<String>,
    #[field(name = "afterChainId")]
    pub after_chain_id: Option<u32>,
    #[field(name = "afterInputToken")]
    pub after_input_token: Option<String>,
    #[field(name = "afterOutputToken")]
    pub after_output_token: Option<String>,
    #[field(name = "limit")]
    #[param(minimum = 1, maximum = 1000, default = 100)]
    pub limit: Option<u32>,
}

#[derive(Debug, FromForm, IntoParams)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct AttributionExecutionParams {
    #[field(name = "apiKeyHash")]
    pub api_key_hash: Option<String>,
    #[field(name = "startBlock")]
    pub start_block: Option<u64>,
    #[field(name = "endBlock")]
    pub end_block: Option<u64>,
    #[field(name = "transactionHash")]
    pub transaction_hash: Option<String>,
    #[field(name = "beforeBlock")]
    pub before_block: Option<u64>,
    #[field(name = "beforeLogIndex")]
    pub before_log_index: Option<u64>,
    #[field(name = "beforeTradeId")]
    pub before_trade_id: Option<String>,
    #[field(name = "limit")]
    #[param(minimum = 1, maximum = 1000, default = 100)]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttributedExecution {
    pub indexed_trade_id: String,
    pub chain_id: u32,
    pub raindex_address: String,
    pub transaction_hash: String,
    pub log_index: u64,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub order_hash: String,
    pub taker: String,
    pub api_key_hash: String,
    /// API key identity captured when this execution was attributed.
    pub api_key_database_id: Option<i64>,
    /// API key identity captured when this execution was attributed.
    pub api_key_id: Option<String>,
    /// API key identity captured when this execution was attributed.
    pub api_key_label: Option<String>,
    /// API key identity captured when this execution was attributed.
    pub api_key_owner: Option<String>,
    pub input_token: String,
    pub input_amount: String,
    pub output_token: String,
    pub output_amount: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttributionExecutionsResponse {
    pub executions: Vec<AttributedExecution>,
    pub next_cursor: Option<AttributionExecutionCursor>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttributionExecutionCursor {
    pub before_block: u64,
    pub before_log_index: u64,
    pub before_trade_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttributionVolume {
    pub api_key_hash: String,
    /// Latest known identity for this API key hash, rather than an execution-time snapshot.
    pub api_key_database_id: Option<i64>,
    /// Latest known identity for this API key hash, rather than an execution-time snapshot.
    pub api_key_id: Option<String>,
    /// Latest known identity for this API key hash, rather than an execution-time snapshot.
    pub api_key_label: Option<String>,
    /// Latest known identity for this API key hash, rather than an execution-time snapshot.
    pub api_key_owner: Option<String>,
    pub chain_id: u32,
    pub input_token: String,
    pub output_token: String,
    pub trade_count: u64,
    pub total_input_amount: String,
    pub total_output_amount: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttributionVolumeResponse {
    pub volume: Vec<AttributionVolume>,
    pub next_cursor: Option<AttributionVolumeCursor>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttributionVolumeCursor {
    pub after_api_key_hash: String,
    pub after_chain_id: u32,
    pub after_input_token: String,
    pub after_output_token: String,
}

struct VolumeCursorFilter {
    api_key_hash: String,
    chain_id: i64,
    input_token: String,
    output_token: String,
}
#[utoipa::path(
    get,
    path = "/admin/attribution/executions",
    tag = "Admin",
    security(("basicAuth" = [])),
    params(AttributionExecutionParams),
    responses(
        (status = 200, description = "Confirmed attributed executions", body = AttributionExecutionsResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden", body = ApiErrorResponse),
        (status = 422, description = "Invalid query parameters", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/executions?<params..>")]
pub async fn get_attributed_executions(
    _global: GlobalRateLimit,
    admin: AdminKey,
    pool: &State<DbPool>,
    span: TracingSpan,
    params: AttributionExecutionParams,
) -> Result<Json<AttributionExecutionsResponse>, ApiError> {
    async move {
        tracing::info!(
            admin_key_id = %admin.0.key_id,
            params = ?params,
            "attribution executions request received"
        );
        validate_block_range(params.start_block, params.end_block)?;
        let limit = params.limit.unwrap_or(100).clamp(1, 1000);
        let cursor = execution_cursor_filter(&params)?;
        let api_key_hash = normalize_hash(params.api_key_hash.as_deref(), "apiKeyHash")?;
        let transaction_hash =
            normalize_hash(params.transaction_hash.as_deref(), "transactionHash")?;
        let mut rows = query_attributed_trades(
            pool,
            api_key_hash.as_deref(),
            params.start_block,
            params.end_block,
            transaction_hash.as_deref(),
            cursor.as_ref(),
            limit + 1,
        )
        .await?;
        let has_more = rows.len() > limit as usize;
        if has_more {
            rows.truncate(limit as usize);
        }
        let next_cursor = if has_more {
            rows.last()
                .map(AttributionExecutionCursor::try_from)
                .transpose()?
        } else {
            None
        };
        let executions = rows
            .into_iter()
            .map(AttributedExecution::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Json(AttributionExecutionsResponse {
            executions,
            next_cursor,
        }))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/admin/attribution/volume",
    tag = "Admin",
    security(("basicAuth" = [])),
    params(AttributionVolumeParams),
    responses(
        (status = 200, description = "Attributed volume grouped by API key and token pair", body = AttributionVolumeResponse),
        (status = 400, description = "Bad request", body = ApiErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden", body = ApiErrorResponse),
        (status = 422, description = "Invalid query parameters", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ApiErrorResponse),
    )
)]
#[get("/volume?<params..>")]
pub async fn get_attribution_volume(
    _global: GlobalRateLimit,
    admin: AdminKey,
    pool: &State<DbPool>,
    span: TracingSpan,
    params: AttributionVolumeParams,
) -> Result<Json<AttributionVolumeResponse>, ApiError> {
    async move {
        tracing::info!(
            admin_key_id = %admin.0.key_id,
            params = ?params,
            "attribution volume request received"
        );
        validate_block_range(params.start_block, params.end_block)?;
        let limit = params.limit.unwrap_or(100).clamp(1, 1000);
        let cursor = volume_cursor_filter(&params)?;
        let api_key_hash = normalize_hash(params.api_key_hash.as_deref(), "apiKeyHash")?;
        let (volume, next_cursor) = aggregate_volume(
            pool,
            api_key_hash.as_deref(),
            params.start_block,
            params.end_block,
            cursor.as_ref(),
            limit,
        )
        .await?;
        Ok(Json(AttributionVolumeResponse {
            volume,
            next_cursor,
        }))
    }
    .instrument(span.0)
    .await
}

async fn query_attributed_trades(
    pool: &DbPool,
    api_key_hash: Option<&str>,
    start_block: Option<u64>,
    end_block: Option<u64>,
    transaction_hash: Option<&str>,
    cursor: Option<&attribution_db::ExecutionCursor<'_>>,
    limit: u32,
) -> Result<Vec<attribution_db::AttributedTradeRow>, ApiError> {
    let start_block = optional_sqlite_integer(start_block, "startBlock")?;
    let end_block = optional_sqlite_integer(end_block, "endBlock")?;
    attribution_db::list_attributed_trades(
        pool,
        attribution_db::AttributionFilter {
            api_key_hash,
            start_block,
            end_block,
            transaction_hash,
        },
        cursor.map(|cursor| attribution_db::ExecutionCursor {
            block: cursor.block,
            log_index: cursor.log_index,
            trade_id: cursor.trade_id,
        }),
        limit,
    )
    .await
    .map_err(query_error)
}

async fn aggregate_volume(
    pool: &DbPool,
    api_key_hash: Option<&str>,
    start_block: Option<u64>,
    end_block: Option<u64>,
    cursor: Option<&VolumeCursorFilter>,
    limit: u32,
) -> Result<(Vec<AttributionVolume>, Option<AttributionVolumeCursor>), ApiError> {
    let start_block = optional_sqlite_integer(start_block, "startBlock")?;
    let end_block = optional_sqlite_integer(end_block, "endBlock")?;
    let page = attribution_report::aggregate_volume(
        pool,
        attribution_db::AttributionFilter {
            api_key_hash,
            start_block,
            end_block,
            transaction_hash: None,
        },
        cursor.map(|cursor| attribution_db::VolumeCursor {
            api_key_hash: &cursor.api_key_hash,
            chain_id: cursor.chain_id,
            input_token: &cursor.input_token,
            output_token: &cursor.output_token,
        }),
        limit,
    )
    .await
    .map_err(attribution_read_error)?;
    let volume = page
        .records
        .into_iter()
        .map(|record| {
            Ok(AttributionVolume {
                api_key_hash: record.api_key_hash,
                api_key_database_id: record.api_key_database_id,
                api_key_id: record.api_key_id,
                api_key_label: record.api_key_label,
                api_key_owner: record.api_key_owner,
                chain_id: to_u32(record.chain_id, "chain id")?,
                input_token: record.input_token,
                output_token: record.output_token,
                trade_count: record.trade_count,
                total_input_amount: record.total_input.format().map_err(float_error)?,
                total_output_amount: record.total_output.format().map_err(float_error)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let next_cursor = if page.has_more {
        volume
            .last()
            .map(AttributionVolumeCursor::try_from)
            .transpose()?
    } else {
        None
    };
    Ok((volume, next_cursor))
}

fn attribution_read_error(error: attribution_report::AttributionReportError) -> ApiError {
    match error {
        attribution_report::AttributionReportError::Database(error) => query_error(error),
        attribution_report::AttributionReportError::InvalidAmount(error) => float_error(error),
    }
}

fn query_error(error: sqlx::Error) -> ApiError {
    tracing::error!(%error, "failed to query attributed trades");
    ApiError::Internal("failed to query attribution report".into())
}

impl TryFrom<attribution_db::AttributedTradeRow> for AttributedExecution {
    type Error = ApiError;

    fn try_from(row: attribution_db::AttributedTradeRow) -> Result<Self, Self::Error> {
        let signed_output = parse_float(&row.output_amount)?;
        let output = Float::zero()
            .and_then(|zero| zero.sub(signed_output))
            .and_then(Float::format)
            .map_err(float_error)?;
        Ok(Self {
            indexed_trade_id: row.indexed_trade_id,
            chain_id: to_u32(row.chain_id, "chain id")?,
            raindex_address: row.raindex_address,
            transaction_hash: row.transaction_hash,
            log_index: to_u64(row.log_index, "log index")?,
            block_number: to_u64(row.block_number, "block number")?,
            block_timestamp: to_u64(row.block_timestamp, "block timestamp")?,
            order_hash: row.order_hash,
            taker: row.taker,
            api_key_hash: row.api_key_hash,
            api_key_database_id: row.api_key_database_id,
            api_key_id: row.api_key_id,
            api_key_label: row.api_key_label,
            api_key_owner: row.api_key_owner,
            input_token: row.input_token,
            input_amount: parse_float(&row.input_amount)?
                .format()
                .map_err(float_error)?,
            output_token: row.output_token,
            output_amount: output,
        })
    }
}

impl TryFrom<&attribution_db::AttributedTradeRow> for AttributionExecutionCursor {
    type Error = ApiError;

    fn try_from(row: &attribution_db::AttributedTradeRow) -> Result<Self, Self::Error> {
        Ok(Self {
            before_block: to_u64(row.block_number, "block number")?,
            before_log_index: to_u64(row.log_index, "log index")?,
            before_trade_id: row.indexed_trade_id.clone(),
        })
    }
}

impl TryFrom<&AttributionVolume> for AttributionVolumeCursor {
    type Error = ApiError;

    fn try_from(volume: &AttributionVolume) -> Result<Self, Self::Error> {
        Ok(Self {
            after_api_key_hash: volume.api_key_hash.clone(),
            after_chain_id: volume.chain_id,
            after_input_token: volume.input_token.clone(),
            after_output_token: volume.output_token.clone(),
        })
    }
}

fn execution_cursor_filter(
    params: &AttributionExecutionParams,
) -> Result<Option<attribution_db::ExecutionCursor<'_>>, ApiError> {
    match (
        params.before_block,
        params.before_log_index,
        params.before_trade_id.as_deref(),
    ) {
        (None, None, None) => Ok(None),
        (Some(block), Some(log_index), Some(trade_id)) if !trade_id.is_empty() => {
            Ok(Some(attribution_db::ExecutionCursor {
                block: i64::try_from(block)
                    .map_err(|_| ApiError::BadRequest("beforeBlock is too large".into()))?,
                log_index: i64::try_from(log_index)
                    .map_err(|_| ApiError::BadRequest("beforeLogIndex is too large".into()))?,
                trade_id,
            }))
        }
        _ => Err(ApiError::BadRequest(
            "beforeBlock, beforeLogIndex, and beforeTradeId must be provided together".into(),
        )),
    }
}

fn volume_cursor_filter(
    params: &AttributionVolumeParams,
) -> Result<Option<VolumeCursorFilter>, ApiError> {
    match (
        params.after_api_key_hash.as_deref(),
        params.after_chain_id,
        params.after_input_token.as_deref(),
        params.after_output_token.as_deref(),
    ) {
        (None, None, None, None) => Ok(None),
        (Some(api_key_hash), Some(chain_id), Some(input_token), Some(output_token)) => {
            let api_key_hash = normalize_hash(Some(api_key_hash), "afterApiKeyHash")?
                .ok_or_else(|| ApiError::BadRequest("afterApiKeyHash is required".into()))?;
            Ok(Some(VolumeCursorFilter {
                api_key_hash,
                chain_id: i64::from(chain_id),
                input_token: normalize_address(input_token, "afterInputToken")?,
                output_token: normalize_address(output_token, "afterOutputToken")?,
            }))
        }
        _ => Err(ApiError::BadRequest(
            "afterApiKeyHash, afterChainId, afterInputToken, and afterOutputToken must be provided together"
                .into(),
        )),
    }
}

fn parse_float(value: &str) -> Result<Float, ApiError> {
    Float::from_hex(value).map_err(float_error)
}

fn float_error(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(%error, "invalid indexed Rain float in attribution report");
    ApiError::Internal("invalid attributed trade amount".into())
}

fn optional_sqlite_integer(value: Option<u64>, name: &str) -> Result<Option<i64>, ApiError> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| ApiError::BadRequest(format!("{name} is too large")))
        })
        .transpose()
}

fn normalize_hash(value: Option<&str>, name: &str) -> Result<Option<String>, ApiError> {
    value
        .map(|value| {
            B256::from_str(value)
                .map(|value| value.to_string())
                .map_err(|_| ApiError::BadRequest(format!("{name} must be a 32-byte hex value")))
        })
        .transpose()
}

fn normalize_address(value: &str, name: &str) -> Result<String, ApiError> {
    Address::from_str(value)
        .map(|_| value.to_string())
        .map_err(|_| ApiError::BadRequest(format!("{name} must be a 20-byte hex address")))
}

fn validate_block_range(start: Option<u64>, end: Option<u64>) -> Result<(), ApiError> {
    if start.zip(end).is_some_and(|(start, end)| start > end) {
        return Err(ApiError::BadRequest(
            "startBlock must be less than or equal to endBlock".into(),
        ));
    }
    Ok(())
}

fn to_u64(value: i64, label: &str) -> Result<u64, ApiError> {
    u64::try_from(value).map_err(|_| {
        tracing::error!(value, label, "negative attribution database value");
        ApiError::Internal("invalid attribution report data".into())
    })
}

fn to_u32(value: i64, label: &str) -> Result<u32, ApiError> {
    u32::try_from(value).map_err(|_| {
        tracing::error!(value, label, "invalid attribution database value");
        ApiError::Internal("invalid attribution report data".into())
    })
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_attributed_executions, get_attribution_volume]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{basic_auth_header, seed_admin_key, TestClientBuilder};
    use rocket::http::{Header, Status};

    async fn insert_trade(client: &rocket::local::asynchronous::Client, indexed_trade_id: &str) {
        let pool = client.rocket().state::<DbPool>().expect("pool");
        let input = Float::parse("0.001".to_string()).expect("input").as_hex();
        let output = Float::parse("-0.205184".to_string())
            .expect("output")
            .as_hex();
        sqlx::query(
            "INSERT INTO attributed_trades (\
                chain_id, raindex_address, indexed_trade_id, transaction_hash, log_index, \
                block_number, block_timestamp, order_hash, taker, api_key_hash, \
                api_key_database_id, api_key_id, api_key_label, api_key_owner, \
                input_token, input_amount, \
                output_token, output_amount\
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(8453_i64)
        .bind("0x1111111111111111111111111111111111111111")
        .bind(indexed_trade_id)
        .bind("0x48f6ed8a67769c007491262e72b78eb934a0a53bc0d67506081fc9a1c35c276f")
        .bind(7_i64)
        .bind(48_963_501_i64)
        .bind(1_753_000_000_i64)
        .bind("0x7df0fb50accfab436c5cbe06bfb5e305a2acc1cd986870220510e67e5970be45")
        .bind("0xA9d6ab42f32dC476269dB2407715D8c15A36D781")
        .bind("0xee79738dcdfded041361028dcf0162b49eafa0a2fed61ba63d8fe275253aa8dc")
        .bind(1_i64)
        .bind("customer-key")
        .bind("Customer")
        .bind("customer@example.com")
        .bind("0x2222222222222222222222222222222222222222")
        .bind(input)
        .bind("0x3333333333333333333333333333333333333333")
        .bind(output)
        .execute(pool)
        .await
        .expect("trade");
    }

    async fn set_trade_group(
        client: &rocket::local::asynchronous::Client,
        indexed_trade_id: &str,
        api_key_hash: &str,
        input_token: &str,
        output_token: &str,
    ) {
        let pool = client.rocket().state::<DbPool>().expect("pool");
        sqlx::query(
            "UPDATE attributed_trades \
             SET api_key_hash = ?, input_token = ?, output_token = ? \
             WHERE indexed_trade_id = ?",
        )
        .bind(api_key_hash)
        .bind(input_token)
        .bind(output_token)
        .bind(indexed_trade_id)
        .execute(pool)
        .await
        .expect("trade group");
    }

    async fn insert_current_identity(
        client: &rocket::local::asynchronous::Client,
        api_key_hash: &str,
        label: &str,
        owner: &str,
    ) {
        let pool = client.rocket().state::<DbPool>().expect("pool");
        sqlx::query(
            "INSERT INTO attribution_api_keys (\
                api_key_hash, api_key_database_id, api_key_id, api_key_label, api_key_owner\
             ) VALUES (?, 2, 'customer-key', ?, ?)",
        )
        .bind(api_key_hash)
        .bind(label)
        .bind(owner)
        .execute(pool)
        .await
        .expect("current identity");
    }

    #[rocket::async_test]
    async fn admin_can_query_execution_and_exact_volume() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        insert_trade(&client, "0x01").await;
        let auth = Header::new("Authorization", basic_auth_header(&key_id, &secret));

        let response = client
            .get(
                "/admin/attribution/executions?startBlock=48963501&transactionHash=0x48f6ed8a67769c007491262e72b78eb934a0a53bc0d67506081fc9a1c35c276f",
            )
            .header(auth.clone())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("execution body");
        assert_eq!(body["executions"][0]["inputAmount"], "0.001");
        assert_eq!(body["executions"][0]["outputAmount"], "0.205184");

        let response = client
            .get("/admin/attribution/volume")
            .header(auth)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("volume body");
        assert_eq!(body["volume"][0]["tradeCount"], 1);
        assert_eq!(body["volume"][0]["totalInputAmount"], "0.001");
        assert_eq!(body["volume"][0]["totalOutputAmount"], "0.205184");
        assert!(body["nextCursor"].is_null());
    }

    #[rocket::async_test]
    async fn executions_use_stable_keyset_pagination() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        insert_trade(&client, "0x01").await;
        insert_trade(&client, "0x02").await;
        let auth = Header::new("Authorization", basic_auth_header(&key_id, &secret));

        let response = client
            .get("/admin/attribution/executions?limit=1")
            .header(auth.clone())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("first page");
        assert_eq!(body["executions"][0]["indexedTradeId"], "0x02");
        assert_eq!(body["nextCursor"]["beforeBlock"], 48_963_501);
        assert_eq!(body["nextCursor"]["beforeLogIndex"], 7);
        assert_eq!(body["nextCursor"]["beforeTradeId"], "0x02");

        let response = client
            .get(
                "/admin/attribution/executions?limit=1&beforeBlock=48963501&beforeLogIndex=7&beforeTradeId=0x02",
            )
            .header(auth.clone())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("second page");
        assert_eq!(body["executions"][0]["indexedTradeId"], "0x01");
        assert!(body["nextCursor"].is_null());

        let response = client
            .get("/admin/attribution/volume")
            .header(auth)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("grouped volume");
        assert_eq!(body["volume"].as_array().expect("volume rows").len(), 1);
        assert_eq!(body["volume"][0]["tradeCount"], 2);
    }

    #[rocket::async_test]
    async fn volume_uses_bounded_keyset_pagination() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        insert_trade(&client, "0x01").await;
        insert_trade(&client, "0x02").await;
        let first_hash = B256::new([0x11; 32]).to_string();
        let second_hash = B256::new([0x22; 32]).to_string();
        let input_token = Address::new([0x33; 20]).to_string();
        let output_token = Address::new([0x44; 20]).to_string();
        set_trade_group(&client, "0x01", &first_hash, &input_token, &output_token).await;
        set_trade_group(&client, "0x02", &second_hash, &input_token, &output_token).await;
        let auth = Header::new("Authorization", basic_auth_header(&key_id, &secret));

        let response = client
            .get("/admin/attribution/volume?limit=1")
            .header(auth.clone())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("first volume page");
        assert_eq!(body["volume"].as_array().expect("volume rows").len(), 1);
        assert_eq!(body["volume"][0]["apiKeyHash"], first_hash);
        assert_eq!(body["nextCursor"]["afterApiKeyHash"], first_hash);

        let next_url = format!(
            "/admin/attribution/volume?limit=1&afterApiKeyHash={}&afterChainId={}&afterInputToken={}&afterOutputToken={}",
            body["nextCursor"]["afterApiKeyHash"]
                .as_str()
                .expect("cursor API key hash"),
            body["nextCursor"]["afterChainId"]
                .as_u64()
                .expect("cursor chain ID"),
            body["nextCursor"]["afterInputToken"]
                .as_str()
                .expect("cursor input token"),
            body["nextCursor"]["afterOutputToken"]
                .as_str()
                .expect("cursor output token"),
        );
        let response = client.get(next_url).header(auth).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("second volume page");
        assert_eq!(body["volume"].as_array().expect("volume rows").len(), 1);
        assert_eq!(body["volume"][0]["apiKeyHash"], second_hash);
        assert!(body["nextCursor"].is_null());
    }

    #[rocket::async_test]
    async fn volume_uses_current_identity_while_executions_keep_snapshot() {
        let client = TestClientBuilder::new().build().await;
        let (key_id, secret) = seed_admin_key(&client).await;
        insert_trade(&client, "0x01").await;
        let api_key_hash = "0xee79738dcdfded041361028dcf0162b49eafa0a2fed61ba63d8fe275253aa8dc";
        insert_current_identity(
            &client,
            api_key_hash,
            "Current Customer",
            "current@example.com",
        )
        .await;
        let auth = Header::new("Authorization", basic_auth_header(&key_id, &secret));

        let response = client
            .get("/admin/attribution/executions")
            .header(auth.clone())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("execution");
        assert_eq!(body["executions"][0]["apiKeyLabel"], "Customer");
        assert_eq!(body["executions"][0]["apiKeyOwner"], "customer@example.com");

        let response = client
            .get("/admin/attribution/volume")
            .header(auth)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let body: serde_json::Value = response.into_json().await.expect("volume");
        assert_eq!(body["volume"][0]["apiKeyLabel"], "Current Customer");
        assert_eq!(body["volume"][0]["apiKeyOwner"], "current@example.com");
    }

    #[rocket::async_test]
    async fn attribution_routes_require_admin_key() {
        let client = TestClientBuilder::new().build().await;
        let response = client.get("/admin/attribution/executions").dispatch().await;
        assert_eq!(response.status(), Status::Unauthorized);
    }
}
