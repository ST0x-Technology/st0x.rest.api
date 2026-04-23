use crate::db::DbPool;
use crate::error::ApiError;
use crate::fairings::TracingSpan;
use crate::raindex::SharedRaindexProvider;
use crate::types::health::{
    DbStatus, DetailedHealthResponse, HealthResponse, OrderbookSyncInfo, RaindexDbStatus,
};
use rocket::serde::json::Json;
use rocket::{Route, State};
use tokio::task::spawn_blocking;
use tracing::Instrument;

#[utoipa::path(
    get,
    path = "/health",
    tag = "Health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse),
    )
)]
#[get("/health")]
pub async fn get_health(span: TracingSpan) -> Result<Json<HealthResponse>, ApiError> {
    async move {
        tracing::info!("request received");
        Ok(Json(HealthResponse {
            status: "ok".into(),
        }))
    }
    .instrument(span.0)
    .await
}

#[utoipa::path(
    get,
    path = "/health/detailed",
    tag = "Health",
    responses(
        (status = 200, description = "Detailed service health including sync status", body = DetailedHealthResponse),
    )
)]
#[get("/health/detailed")]
pub async fn get_health_detailed(
    span: TracingSpan,
    pool: &State<DbPool>,
    shared_raindex: &State<SharedRaindexProvider>,
) -> Result<Json<DetailedHealthResponse>, ApiError> {
    async move {
        tracing::info!("detailed health check request received");

        // 1. Check app database connectivity
        let app_db = check_app_db(pool).await;

        // 2. Check raindex database and sync status
        let raindex_db = check_raindex_db(shared_raindex).await;

        // 3. Determine overall status
        let status = if app_db.connected && raindex_db.connected && !raindex_db.orderbooks.is_empty()
        {
            "ok".to_string()
        } else if app_db.connected || raindex_db.connected {
            "degraded".to_string()
        } else {
            "error".to_string()
        };

        Ok(Json(DetailedHealthResponse {
            status,
            app_db,
            raindex_db,
        }))
    }
    .instrument(span.0)
    .await
}

async fn check_app_db(pool: &DbPool) -> DbStatus {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_) => DbStatus {
            connected: true,
            error: None,
        },
        Err(e) => {
            tracing::warn!(error = %e, "app database health check failed");
            DbStatus {
                connected: false,
                error: Some(e.to_string()),
            }
        }
    }
}

async fn check_raindex_db(shared_raindex: &SharedRaindexProvider) -> RaindexDbStatus {
    let raindex = shared_raindex.read().await;

    let db_path = match raindex.db_path() {
        Some(path) => path,
        None => {
            return RaindexDbStatus {
                connected: false,
                error: Some("no local db path configured".into()),
                db_path: None,
                orderbooks: vec![],
            };
        }
    };

    let db_path_str = db_path.display().to_string();

    if !db_path.exists() {
        return RaindexDbStatus {
            connected: false,
            error: Some("raindex database file does not exist".into()),
            db_path: Some(db_path_str),
            orderbooks: vec![],
        };
    }

    // Get all configured orderbooks to know which chain_id + address combos to query
    let orderbooks = match raindex.client().get_all_orderbooks() {
        Ok(obs) => obs,
        Err(e) => {
            tracing::warn!(error = %e, "failed to get orderbooks from raindex config");
            return RaindexDbStatus {
                connected: false,
                error: Some(format!("failed to read orderbook config: {e}")),
                db_path: Some(db_path_str),
                orderbooks: vec![],
            };
        }
    };

    // Open a read-only connection to raindex.db and query sync_status + latest trades
    let db_path_clone = db_path.clone();
    let orderbook_configs: Vec<(u32, String)> = orderbooks
        .values()
        .map(|ob| (ob.network.chain_id, format!("{:#x}", ob.address)))
        .collect();

    let query_result = spawn_blocking(move || {
        query_raindex_sync_status(&db_path_clone, &orderbook_configs)
    })
    .await;

    match query_result {
        Ok(Ok(orderbook_infos)) => RaindexDbStatus {
            connected: true,
            error: None,
            db_path: Some(db_path_str),
            orderbooks: orderbook_infos,
        },
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "raindex db sync status query failed");
            RaindexDbStatus {
                connected: false,
                error: Some(e),
                db_path: Some(db_path_str),
                orderbooks: vec![],
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "raindex db query task panicked");
            RaindexDbStatus {
                connected: false,
                error: Some("query task failed".into()),
                db_path: Some(db_path_str),
                orderbooks: vec![],
            }
        }
    }
}

fn query_raindex_sync_status(
    db_path: &std::path::Path,
    orderbook_configs: &[(u32, String)],
) -> Result<Vec<OrderbookSyncInfo>, String> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("failed to open raindex db: {e}"))?;

    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| format!("failed to set busy_timeout: {e}"))?;

    let mut results = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for (chain_id, ob_address) in orderbook_configs {
        let mut info = OrderbookSyncInfo {
            chain_id: *chain_id,
            orderbook_address: ob_address.clone(),
            last_synced_block: 0,
            updated_at: None,
            latest_trade_timestamp: None,
            latest_trade_age: None,
        };

        // Query sync_status table
        match conn.query_row(
            "SELECT last_synced_block, updated_at FROM sync_status WHERE chain_id = ?1 AND orderbook_address = ?2",
            rusqlite::params![*chain_id as i64, ob_address],
            |row| {
                let block: i64 = row.get(0)?;
                let updated_at: Option<String> = row.get(1)?;
                Ok((block, updated_at))
            },
        ) {
            Ok((block, updated_at)) => {
                info.last_synced_block = block as u64;
                info.updated_at = updated_at;
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // sync_status row doesn't exist yet — sync hasn't started
                tracing::info!(
                    chain_id = chain_id,
                    orderbook = %ob_address,
                    "no sync_status row found"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    chain_id = chain_id,
                    orderbook = %ob_address,
                    "failed to query sync_status"
                );
            }
        }

        // Query latest trade timestamp from take_orders table
        match conn.query_row(
            "SELECT MAX(block_timestamp) FROM take_orders WHERE chain_id = ?1 AND orderbook_address = ?2",
            rusqlite::params![*chain_id as i64, ob_address],
            |row| {
                let ts: Option<i64> = row.get(0)?;
                Ok(ts)
            },
        ) {
            Ok(Some(ts)) if ts > 0 => {
                let ts_u64 = ts as u64;
                info.latest_trade_timestamp = Some(ts_u64);
                info.latest_trade_age = Some(format_age(now, ts_u64));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    chain_id = chain_id,
                    "failed to query latest trade timestamp"
                );
            }
        }

        results.push(info);
    }

    Ok(results)
}

fn format_age(now_secs: u64, timestamp_secs: u64) -> String {
    if timestamp_secs > now_secs {
        return "just now".to_string();
    }

    let diff = now_secs - timestamp_secs;

    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        let minutes = diff / 60;
        format!("{minutes}m ago")
    } else if diff < 86400 {
        let hours = diff / 3600;
        let minutes = (diff % 3600) / 60;
        if minutes > 0 {
            format!("{hours}h {minutes}m ago")
        } else {
            format!("{hours}h ago")
        }
    } else {
        let days = diff / 86400;
        let hours = (diff % 86400) / 3600;
        if hours > 0 {
            format!("{days}d {hours}h ago")
        } else {
            format!("{days}d ago")
        }
    }
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_health, get_health_detailed]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_age_seconds() {
        assert_eq!(format_age(1000, 970), "30s ago");
    }

    #[test]
    fn test_format_age_minutes() {
        assert_eq!(format_age(1000, 700), "5m ago");
    }

    #[test]
    fn test_format_age_hours_and_minutes() {
        assert_eq!(format_age(10000, 2200), "2h 10m ago");
    }

    #[test]
    fn test_format_age_days_and_hours() {
        assert_eq!(format_age(200000, 10000), "2d 4h ago");
    }

    #[test]
    fn test_format_age_future_timestamp() {
        assert_eq!(format_age(1000, 2000), "just now");
    }

    #[test]
    fn test_format_age_exact_hour() {
        assert_eq!(format_age(3600, 0), "1h ago");
    }

    #[test]
    fn test_format_age_exact_day() {
        assert_eq!(format_age(86400, 0), "1d ago");
    }
}
