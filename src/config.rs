use serde::Deserialize;
use std::path::Path;

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
    pub rate_limit_global_rpm: u64,
    pub rate_limit_per_key_rpm: u64,
    pub docs_dir: String,
    pub local_db_path: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {e}"))?;
        toml::from_str(&contents).map_err(|e| format!("failed to parse config: {e}"))
    }
}
