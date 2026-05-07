use crate::db::DbPool;
use crate::error::ApiError;
use crate::fairings::TracingSpan;
use crate::raindex::SharedRaindexProvider;
use crate::types::health::{
    DbHealthStatus, DbStatus, DetailedHealthResponse, HealthResponse, HealthStatus,
    NetworkSyncInfo, OrderbookSyncInfo, RaindexSyncStatus, RaindexSyncStatusKind,
};
use rain_orderbook_common::raindex_client::local_db::{
    LocalDbSyncSnapshot, NetworkSyncStatusSnapshot, OrderbookSyncStatusSnapshot,
};
use rocket::serde::json::Json;
use rocket::{Route, State};
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
            status: HealthStatus::Ok,
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

        tracing::info!("checking application database and raindex local database");
        let (app_db, raindex) = tokio::join!(check_app_db(pool), check_raindex_db(shared_raindex));

        let status = detailed_status(&app_db, &raindex);
        tracing::info!(status = ?status, "detailed health check completed");

        Ok(Json(DetailedHealthResponse {
            status,
            app_db,
            raindex,
        }))
    }
    .instrument(span.0)
    .await
}

async fn check_app_db(pool: &DbPool) -> DbStatus {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_) => DbStatus {
            status: DbHealthStatus::Ok,
            connected: true,
            error: None,
        },
        Err(e) => {
            tracing::warn!(error = %e, "app database health check failed");
            DbStatus {
                status: DbHealthStatus::Error,
                connected: false,
                error: Some("application database unavailable".to_string()),
            }
        }
    }
}

async fn check_raindex_db(shared_raindex: &SharedRaindexProvider) -> RaindexSyncStatus {
    let client = {
        let raindex = shared_raindex.read().await;
        raindex.client().clone()
    };

    match client.get_local_db_sync_snapshot().await {
        Ok(snapshot) => map_raindex_snapshot(snapshot),
        Err(e) => {
            tracing::warn!(error = %e, "failed to get raindex local db sync snapshot");
            RaindexSyncStatus {
                status: RaindexSyncStatusKind::Failure,
                configured: false,
                healthy: false,
                error: Some("raindex local DB sync snapshot unavailable".to_string()),
                networks: vec![],
                orderbooks: vec![],
            }
        }
    }
}

fn map_raindex_snapshot(snapshot: LocalDbSyncSnapshot) -> RaindexSyncStatus {
    let status = if snapshot.configured {
        snapshot.status.into()
    } else {
        RaindexSyncStatusKind::NotConfigured
    };

    log_raindex_snapshot_errors(&snapshot);

    RaindexSyncStatus {
        status,
        configured: snapshot.configured,
        healthy: snapshot.healthy,
        error: raindex_error(&snapshot),
        networks: snapshot
            .networks
            .into_iter()
            .map(map_network_snapshot)
            .collect(),
        orderbooks: snapshot
            .orderbooks
            .into_iter()
            .map(map_orderbook_snapshot)
            .collect(),
    }
}

fn map_network_snapshot(snapshot: NetworkSyncStatusSnapshot) -> NetworkSyncInfo {
    NetworkSyncInfo {
        chain_id: snapshot.chain_id,
        network_key: snapshot.network_key,
        status: snapshot.status.into(),
        orderbook_count: snapshot.orderbook_count,
        ready: snapshot.ready,
        error: snapshot.error.map(|_| "network sync failed".to_string()),
    }
}

fn map_orderbook_snapshot(snapshot: OrderbookSyncStatusSnapshot) -> OrderbookSyncInfo {
    OrderbookSyncInfo {
        chain_id: snapshot.ob_id.chain_id,
        orderbook_address: format!("{:#x}", snapshot.ob_id.orderbook_address),
        orderbook_key: snapshot.orderbook_key,
        network_key: snapshot.network_key,
        status: snapshot.status.into(),
        ready: snapshot.ready,
        phase_message: snapshot.phase_message,
        last_synced_block: snapshot.last_synced_block,
        updated_at: snapshot.updated_at,
        error: snapshot.error.map(|_| "orderbook sync failed".to_string()),
    }
}

fn log_raindex_snapshot_errors(snapshot: &LocalDbSyncSnapshot) {
    for network in &snapshot.networks {
        if let Some(error) = &network.error {
            tracing::warn!(
                chain_id = network.chain_id,
                network_key = network.network_key.as_deref(),
                error = %error,
                "raindex network sync failed"
            );
        }
    }

    for orderbook in &snapshot.orderbooks {
        if let Some(error) = &orderbook.error {
            tracing::warn!(
                chain_id = orderbook.ob_id.chain_id,
                orderbook_address = %format!("{:#x}", orderbook.ob_id.orderbook_address),
                orderbook_key = orderbook.orderbook_key.as_deref(),
                network_key = orderbook.network_key.as_deref(),
                error = %error,
                "raindex orderbook sync failed"
            );
        }
    }
}

fn raindex_error(snapshot: &LocalDbSyncSnapshot) -> Option<String> {
    if !snapshot.healthy {
        Some("raindex local DB sync is unhealthy".to_string())
    } else if !snapshot.configured {
        Some("raindex local DB sync is not configured".to_string())
    } else {
        None
    }
}

fn detailed_status(app_db: &DbStatus, raindex: &RaindexSyncStatus) -> HealthStatus {
    if !app_db.connected || !raindex.healthy || raindex.status == RaindexSyncStatusKind::Failure {
        HealthStatus::Error
    } else if raindex.status == RaindexSyncStatusKind::NotConfigured
        || raindex.status == RaindexSyncStatusKind::Syncing
    {
        HealthStatus::Degraded
    } else {
        HealthStatus::Ok
    }
}

pub fn routes() -> Vec<Route> {
    rocket::routes![get_health, get_health_detailed]
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use rain_orderbook_common::local_db::OrderbookIdentifier;
    use rain_orderbook_common::raindex_client::local_db::{LocalDbStatus, SchedulerState};

    #[test]
    fn detailed_status_is_degraded_when_raindex_has_not_started() {
        let app_db = DbStatus {
            status: DbHealthStatus::Ok,
            connected: true,
            error: None,
        };
        let raindex = RaindexSyncStatus {
            status: RaindexSyncStatusKind::Syncing,
            configured: true,
            healthy: true,
            error: None,
            networks: vec![],
            orderbooks: vec![],
        };

        assert_eq!(detailed_status(&app_db, &raindex), HealthStatus::Degraded);
    }

    #[test]
    fn detailed_status_is_error_when_app_db_is_down() {
        let app_db = DbStatus {
            status: DbHealthStatus::Error,
            connected: false,
            error: Some("db unavailable".to_string()),
        };
        let raindex = RaindexSyncStatus {
            status: RaindexSyncStatusKind::Active,
            configured: true,
            healthy: true,
            error: None,
            networks: vec![],
            orderbooks: vec![],
        };

        assert_eq!(detailed_status(&app_db, &raindex), HealthStatus::Error);
    }

    #[test]
    fn detailed_status_is_degraded_when_raindex_is_not_configured() {
        let app_db = DbStatus {
            status: DbHealthStatus::Ok,
            connected: true,
            error: None,
        };
        let raindex = RaindexSyncStatus {
            status: RaindexSyncStatusKind::NotConfigured,
            configured: false,
            healthy: true,
            error: Some("raindex local DB sync is not configured".to_string()),
            networks: vec![],
            orderbooks: vec![],
        };

        assert_eq!(detailed_status(&app_db, &raindex), HealthStatus::Degraded);
    }

    #[test]
    fn map_raindex_snapshot_preserves_network_and_orderbook_status() {
        let orderbook_id =
            OrderbookIdentifier::new(8453, address!("d2938e7c9fe3597f78832ce780feb61945c377d7"));
        let snapshot = LocalDbSyncSnapshot::from_parts(
            vec![NetworkSyncStatusSnapshot {
                chain_id: 8453,
                network_key: Some("base".to_string()),
                status: LocalDbStatus::Active,
                scheduler_state: SchedulerState::Leader,
                orderbook_count: 1,
                ready: true,
                error: None,
            }],
            vec![OrderbookSyncStatusSnapshot {
                ob_id: orderbook_id,
                orderbook_key: Some("base-orderbook".to_string()),
                network_key: Some("base".to_string()),
                status: LocalDbStatus::Active,
                scheduler_state: SchedulerState::Leader,
                ready: true,
                phase_message: None,
                last_synced_block: Some(12_345_678),
                updated_at: Some("2026-05-01 12:00:00".to_string()),
                error: None,
            }],
        );

        let raindex = map_raindex_snapshot(snapshot);

        assert_eq!(raindex.status, RaindexSyncStatusKind::Active);
        assert!(raindex.configured);
        assert!(raindex.healthy);
        assert_eq!(raindex.networks.len(), 1);
        assert_eq!(raindex.networks[0].network_key.as_deref(), Some("base"));
        assert_eq!(raindex.orderbooks.len(), 1);
        assert_eq!(
            raindex.orderbooks[0].orderbook_address,
            "0xd2938e7c9fe3597f78832ce780feb61945c377d7"
        );
        assert_eq!(raindex.orderbooks[0].last_synced_block, Some(12_345_678));
        assert_eq!(
            raindex.orderbooks[0].updated_at.as_deref(),
            Some("2026-05-01 12:00:00")
        );
    }

    #[test]
    fn map_raindex_snapshot_sanitizes_sync_errors() {
        let orderbook_id =
            OrderbookIdentifier::new(8453, address!("d2938e7c9fe3597f78832ce780feb61945c377d7"));
        let snapshot = LocalDbSyncSnapshot::from_parts(
            vec![NetworkSyncStatusSnapshot {
                chain_id: 8453,
                network_key: Some("base".to_string()),
                status: LocalDbStatus::Failure,
                scheduler_state: SchedulerState::Leader,
                orderbook_count: 1,
                ready: false,
                error: Some("sqlite: no such table sync_status".to_string()),
            }],
            vec![OrderbookSyncStatusSnapshot {
                ob_id: orderbook_id,
                orderbook_key: Some("base-orderbook".to_string()),
                network_key: Some("base".to_string()),
                status: LocalDbStatus::Failure,
                scheduler_state: SchedulerState::Leader,
                ready: false,
                phase_message: None,
                last_synced_block: None,
                updated_at: None,
                error: Some("provider url includes internal host".to_string()),
            }],
        );

        let raindex = map_raindex_snapshot(snapshot);

        assert_eq!(
            raindex.error.as_deref(),
            Some("raindex local DB sync is unhealthy")
        );
        assert_eq!(
            raindex.networks[0].error.as_deref(),
            Some("network sync failed")
        );
        assert_eq!(
            raindex.orderbooks[0].error.as_deref(),
            Some("orderbook sync failed")
        );
    }

    #[test]
    fn detailed_response_does_not_expose_scheduler_state() {
        let response = DetailedHealthResponse {
            status: HealthStatus::Ok,
            app_db: DbStatus {
                status: DbHealthStatus::Ok,
                connected: true,
                error: None,
            },
            raindex: RaindexSyncStatus {
                status: RaindexSyncStatusKind::Active,
                configured: true,
                healthy: true,
                error: None,
                networks: vec![],
                orderbooks: vec![],
            },
        };

        let serialized = match serde_json::to_value(response) {
            Ok(value) => value,
            Err(error) => panic!("detailed health response should serialize: {error}"),
        };

        assert!(serialized.get("scheduler_state").is_none());
        assert!(serialized["raindex"].get("scheduler_state").is_none());
        assert_eq!(serialized["status"], "ok");
        assert_eq!(serialized["app_db"]["status"], "ok");
        assert_eq!(serialized["raindex"]["status"], "active");
    }

    #[test]
    fn map_raindex_snapshot_reports_not_configured() {
        let raindex = map_raindex_snapshot(LocalDbSyncSnapshot::not_configured());

        assert_eq!(raindex.status, RaindexSyncStatusKind::NotConfigured);
        assert!(!raindex.configured);
        assert!(raindex.healthy);
        assert_eq!(
            raindex.error.as_deref(),
            Some("raindex local DB sync is not configured")
        );
    }
}
