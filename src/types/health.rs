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
