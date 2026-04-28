use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Deserialize)]
pub struct Config {
    pub log_dir: String,
    pub database_url: String,
    pub registry_url: String,
    pub rate_limit_global_rpm: u64,
    pub rate_limit_per_key_rpm: u64,
    pub docs_dir: String,
    pub local_db_path: String,
    /// Optional per-network RPC URL override.
    ///
    /// Replaces the `rpcs:` list in the rain.strategies registry settings for
    /// the named network. Use to point at private/paid RPC endpoints without
    /// forking the registry. Example in `config.toml`:
    ///
    /// ```toml
    /// [rpc_override]
    /// base = [
    ///     "https://base-mainnet.g.alchemy.com/v2/YOUR_KEY",
    ///     "https://base.drpc.org",
    /// ]
    /// ```
    ///
    /// When multiple URLs are given the underlying provider treats them as
    /// health-routed failover (alloy `FallbackLayer` with
    /// `active_transport_count = 1`).
    #[serde(default)]
    pub rpc_override: HashMap<String, Vec<String>>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {e}"))?;
        toml::from_str(&contents).map_err(|e| format!("failed to parse config: {e}"))
    }
}
