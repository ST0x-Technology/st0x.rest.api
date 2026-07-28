//! Attribution report aggregation over persisted execution rows.

use crate::db::{attribution as attribution_db, DbPool};
use rain_math_float::{Float, FloatError};
use std::ops::{Add, Sub};

#[derive(Debug, thiserror::Error)]
pub(crate) enum AttributionReportError {
    #[error("database query failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid attributed trade amount: {0}")]
    InvalidAmount(#[from] FloatError),
}

#[derive(Debug)]
pub(crate) struct AttributionVolumeRecord {
    pub(crate) api_key_hash: String,
    pub(crate) api_key_database_id: Option<i64>,
    pub(crate) api_key_id: Option<String>,
    pub(crate) api_key_label: Option<String>,
    pub(crate) api_key_owner: Option<String>,
    pub(crate) chain_id: i64,
    pub(crate) input_token: String,
    pub(crate) output_token: String,
    pub(crate) trade_count: u64,
    pub(crate) total_input: Float,
    pub(crate) total_output: Float,
}

pub(crate) struct AttributionVolumePage {
    pub(crate) records: Vec<AttributionVolumeRecord>,
    pub(crate) has_more: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct VolumeKey {
    api_key_hash: String,
    chain_id: i64,
    input_token: String,
    output_token: String,
}

struct VolumeAccumulator {
    trade_count: u64,
    total_input: Float,
    total_output: Float,
}

struct VolumeGroup {
    key: VolumeKey,
    api_key_database_id: Option<i64>,
    api_key_id: Option<String>,
    api_key_label: Option<String>,
    api_key_owner: Option<String>,
    accumulator: VolumeAccumulator,
}

pub(crate) async fn aggregate_volume(
    pool: &DbPool,
    filter: attribution_db::AttributionFilter<'_>,
    cursor: Option<attribution_db::VolumeCursor<'_>>,
    limit: u32,
) -> Result<AttributionVolumePage, AttributionReportError> {
    let mut groups = Vec::<VolumeGroup>::with_capacity(limit as usize);
    let mut has_more = false;
    attribution_db::visit_volume_rows(pool, filter, cursor, |row| {
        let input = Float::from_hex(&row.input_amount)?;
        let output = Float::zero()?.sub(Float::from_hex(&row.output_amount)?)?;
        let key = VolumeKey {
            api_key_hash: row.api_key_hash,
            chain_id: row.chain_id,
            input_token: row.input_token,
            output_token: row.output_token,
        };
        if let Some(group) = groups.last_mut().filter(|group| group.key == key) {
            group.accumulator.trade_count += 1;
            group.accumulator.total_input = group.accumulator.total_input.add(input)?;
            group.accumulator.total_output = group.accumulator.total_output.add(output)?;
        } else if groups.len() < limit as usize {
            groups.push(VolumeGroup {
                key,
                api_key_database_id: row.api_key_database_id,
                api_key_id: row.api_key_id,
                api_key_label: row.api_key_label,
                api_key_owner: row.api_key_owner,
                accumulator: VolumeAccumulator {
                    trade_count: 1,
                    total_input: input,
                    total_output: output,
                },
            });
        } else {
            has_more = true;
            return Ok(false);
        }
        Ok(true)
    })
    .await
    .map_err(|error| match error {
        attribution_db::VisitRowsError::Database(error) => AttributionReportError::Database(error),
        attribution_db::VisitRowsError::Visitor(error) => {
            AttributionReportError::InvalidAmount(error)
        }
    })?;

    let records = groups
        .into_iter()
        .map(|group| AttributionVolumeRecord {
            api_key_hash: group.key.api_key_hash,
            api_key_database_id: group.api_key_database_id,
            api_key_id: group.api_key_id,
            api_key_label: group.api_key_label,
            api_key_owner: group.api_key_owner,
            chain_id: group.key.chain_id,
            input_token: group.key.input_token,
            output_token: group.key.output_token,
            trade_count: group.accumulator.trade_count,
            total_input: group.accumulator.total_input,
            total_output: group.accumulator.total_output,
        })
        .collect();
    Ok(AttributionVolumePage { records, has_more })
}
