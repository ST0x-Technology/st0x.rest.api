use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
    #[schema(example = "ok")]
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DetailedHealthResponse {
    /// Overall API status: "ok", "degraded", or "error"
    #[schema(example = "ok")]
    pub status: String,

    /// st0x application database connectivity
    pub app_db: DbStatus,

    /// raindex local database connectivity and sync status
    pub raindex_db: RaindexDbStatus,

    /// Background cache warmer health
    pub cache_warmer: CacheWarmerStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CacheWarmerStatus {
    /// True if the warmer has completed at least one cycle
    pub running: bool,

    /// Total number of cycles completed since process start
    #[schema(example = 42)]
    pub total_cycles: u64,

    /// Duration of the most recently completed cycle, in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 9500)]
    pub last_cycle_ms: Option<u64>,

    /// Number of orders refreshed in the last cycle (`tokens` × per-token orders implicit)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 11)]
    pub last_tokens: Option<u32>,

    /// Errors observed in the last cycle (per-token failures)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_errors: Option<u32>,

    /// Seconds elapsed since the warmer last completed a cycle
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = 7)]
    pub seconds_since_last_complete: Option<u64>,

    /// Human-readable age of the last completion (e.g. `12s ago`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_complete_age: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DbStatus {
    /// Whether the database is reachable
    #[schema(example = true)]
    pub connected: bool,

    /// Error message if not connected
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RaindexDbStatus {
    /// Whether the raindex database file exists and is readable
    #[schema(example = true)]
    pub connected: bool,

    /// Error message if not connected
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Path to the raindex database file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_path: Option<String>,

    /// Per-orderbook sync status from the sync_status table
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub orderbooks: Vec<OrderbookSyncInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OrderbookSyncInfo {
    /// Chain ID (e.g. 8453 for Base)
    #[schema(example = 8453)]
    pub chain_id: u32,

    /// Orderbook contract address
    #[schema(example = "0xd2938e7c9fe3597f78832ce780feb61945c377d7")]
    pub orderbook_address: String,

    /// Last block number synced by raindex
    #[schema(example = 12345678)]
    pub last_synced_block: u64,

    /// Timestamp when sync_status was last updated (ISO 8601)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,

    /// Timestamp of the most recent trade in the database
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_trade_timestamp: Option<u64>,

    /// Human-readable age of the latest trade (e.g. "2h 15m ago")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_trade_age: Option<String>,
}
