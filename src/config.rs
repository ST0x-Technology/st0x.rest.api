use serde::Deserialize;
use std::path::Path;
use url::Url;

#[derive(Deserialize)]
pub struct Config {
    pub log_dir: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub usage_log_max_concurrency: usize,
    pub response_cache_max_entries: u64,
    pub response_cache_ttl_seconds: u64,
    pub registry_url: String,
    pub private_registry_path: String,
    pub allow_registry_fallback: bool,
    pub rate_limit_global_rpm: u64,
    pub rate_limit_per_key_rpm: u64,
    pub docs_dir: String,
    pub local_db_path: String,
    /// OTLP export target. Absent ⇒ console + file logging only (no push to the
    /// observability stack). Non-secret plaintext (tailnet endpoints).
    #[serde(default)]
    pub telemetry: Option<TelemetryConfig>,
}

/// Where to push OTLP logs/traces, and how signals are labelled. Endpoints are
/// the VictoriaLogs (`:9428`) / VictoriaTraces (`:10428`) ingest URLs, reached
/// over the tailnet (e.g. `http://rain-management-observability.taile5cf8a.ts.net:9428`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    pub service_name: String,
    /// `production`, `staging`, … exported as the `deployment.environment`
    /// resource attribute so environments are distinguishable downstream.
    pub environment: String,
    pub traces_endpoint: Url,
    pub logs_endpoint: Url,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {e}"))?;
        toml::from_str(&contents).map_err(|e| format!("failed to parse config: {e}"))
    }
}
