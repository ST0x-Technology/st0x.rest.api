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
    #[serde(default)]
    pub response_cache_max_trade_rows: Option<u64>,
    pub response_cache_ttl_seconds: u64,
    pub registry_url: String,
    pub private_registry_path: String,
    pub allow_registry_fallback: bool,
    pub rate_limit_global_rpm: u64,
    pub rate_limit_per_key_rpm: u64,
    pub swap_max_concurrent_global: usize,
    pub swap_max_concurrent_per_key: usize,
    pub swap_request_timeout_seconds: u64,
    pub docs_dir: String,
    pub local_db_path: String,
    pub price_sampler_enabled: bool,
    pub price_sample_interval_seconds: u64,
    pub price_history_retention_seconds: u64,
    #[serde(default)]
    pub attribution_start_block: Option<u64>,
    #[serde(default = "default_attribution_sync_interval_seconds")]
    pub attribution_sync_interval_seconds: u64,
    #[serde(default = "default_attribution_sync_batch_size")]
    pub attribution_sync_batch_size: u32,
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

fn default_attribution_sync_interval_seconds() -> u64 {
    60
}

fn default_attribution_sync_batch_size() -> u32 {
    250
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {e}"))?;
        let config: Self =
            toml::from_str(&contents).map_err(|e| format!("failed to parse config: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if self.swap_max_concurrent_global == 0 {
            return Err("swap_max_concurrent_global must be greater than zero".into());
        }
        if self.swap_max_concurrent_per_key == 0 {
            return Err("swap_max_concurrent_per_key must be greater than zero".into());
        }
        if self.swap_request_timeout_seconds == 0 {
            return Err("swap_request_timeout_seconds must be greater than zero".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> Config {
        toml::from_str(include_str!("../config/dev.toml")).expect("valid development config")
    }

    #[test]
    fn swap_capacity_settings_must_be_nonzero() {
        let mut config = valid_config();
        config.swap_max_concurrent_global = 0;
        assert_eq!(
            config.validate(),
            Err("swap_max_concurrent_global must be greater than zero".into())
        );

        let mut config = valid_config();
        config.swap_max_concurrent_per_key = 0;
        assert_eq!(
            config.validate(),
            Err("swap_max_concurrent_per_key must be greater than zero".into())
        );

        let mut config = valid_config();
        config.swap_request_timeout_seconds = 0;
        assert_eq!(
            config.validate(),
            Err("swap_request_timeout_seconds must be greater than zero".into())
        );
    }
}
