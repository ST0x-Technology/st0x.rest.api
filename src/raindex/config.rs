use crate::error::ApiError;
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_js_api::registry::DotrainRegistry;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct RaindexProvider {
    registry: DotrainRegistry,
    client: RaindexClient,
    db_path: Option<PathBuf>,
    rpc_overrides: HashMap<String, Vec<String>>,
}

/// Replaces the `rpcs:` list inside each `networks.<name>` block whose name
/// has an entry in `overrides`. Lets operators point at private/paid RPC
/// endpoints without forking the rain.strategies registry.
///
/// The YAML mutation is line-based and intentionally narrow: it only touches
/// the `rpcs:` list immediately under a matched `<name>:` key inside the
/// `networks:` section. Any other keys (chain-id, currency, etc.) are
/// preserved untouched.
fn apply_rpc_override(yaml: &str, overrides: &HashMap<String, Vec<String>>) -> String {
    if overrides.is_empty() {
        return yaml.to_string();
    }

    let mut out = String::with_capacity(yaml.len());
    let mut in_networks = false;
    let mut current_network: Option<String> = None;
    let mut skipping_rpcs = false;

    for line in yaml.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // Detect leaving the `networks:` block (any new top-level key).
        if in_networks && indent == 0 && !line.is_empty() {
            in_networks = false;
            current_network = None;
            skipping_rpcs = false;
        }

        // Enter the `networks:` block.
        if !in_networks && line.starts_with("networks:") {
            in_networks = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_networks {
            // A network name lives at indent 2 (one level under `networks:`).
            if indent == 2 && trimmed.ends_with(':') {
                let name = trimmed.trim_end_matches(':').to_string();
                current_network = Some(name);
                skipping_rpcs = false;
                out.push_str(line);
                out.push('\n');
                continue;
            }

            // While skipping the original `rpcs:` list, drop list entries
            // (lines starting with '-' at indent > 4).
            if skipping_rpcs {
                if indent > 4 && trimmed.starts_with('-') {
                    continue;
                }
                skipping_rpcs = false;
            }

            // Detect `rpcs:` key inside the current network and rewrite it.
            if let Some(name) = &current_network {
                if indent == 4 && trimmed.starts_with("rpcs:") {
                    if let Some(replacement_urls) = overrides.get(name) {
                        out.push_str("    rpcs:\n");
                        for url in replacement_urls {
                            out.push_str(&format!("      - {url}\n"));
                        }
                        skipping_rpcs = true;
                        continue;
                    }
                }
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

/// Neutralizes the `metaboards` section in YAML settings so the library's
/// `fetch_orders_dotrain_sources()` skips network requests to the Goldsky
/// metaboard subgraph. That function fetches `DotrainSourceV1` metadata per
/// order (~5s for 20 orders). Our API never uses `DotrainSourceV1`, so
/// replacing the metaboard keys with non-matching names causes each order's
/// `fetch_dotrain_source()` to return `Ok(())` immediately.
fn neutralize_metaboards(yaml: &str) -> String {
    let mut result = String::with_capacity(yaml.len() + 64);
    let mut in_metaboards = false;

    for line in yaml.lines() {
        if !in_metaboards && line.starts_with("metaboards:") {
            in_metaboards = true;
            result.push_str("metaboards:\n  _disabled: https://localhost\n");
            continue;
        }

        if in_metaboards {
            if line.is_empty() || line.starts_with(' ') || line.starts_with('\t') {
                continue;
            }
            in_metaboards = false;
        }

        result.push_str(line);
        result.push('\n');
    }

    result
}

impl RaindexProvider {
    pub(crate) async fn load(
        registry_url: &str,
        db_path: Option<PathBuf>,
        rpc_overrides: HashMap<String, Vec<String>>,
    ) -> Result<Self, RaindexProviderError> {
        let url = registry_url.to_string();
        let db = db_path.clone();
        let overrides = rpc_overrides;

        let (tx, rx) = tokio::sync::oneshot::channel();

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(RaindexProviderError::RegistryLoad(e.to_string())));
                    return;
                }
            };

            let result = runtime.block_on(async {
                let registry = DotrainRegistry::new(url)
                    .await
                    .map_err(|e| RaindexProviderError::RegistryLoad(e.to_string()))?;

                // Build the client with:
                //  - metaboard lookups disabled to avoid ~5s of subgraph calls
                //  - per-network RPC URLs overridden if `[rpc_override]` is set
                let mut settings = neutralize_metaboards(&registry.settings());
                if !overrides.is_empty() {
                    settings = apply_rpc_override(&settings, &overrides);
                    for (name, urls) in &overrides {
                        tracing::info!(
                            network = %name,
                            url_count = urls.len(),
                            "applied RPC override for network"
                        );
                    }
                }
                let client = RaindexClient::new(vec![settings], None, db.clone())
                    .await
                    .map_err(|e| RaindexProviderError::ClientInit(e.to_string()))?;

                Ok(RaindexProvider {
                    registry,
                    client,
                    db_path: db,
                    rpc_overrides: overrides,
                })
            });

            let _ = tx.send(result);
        });

        rx.await.map_err(|_| RaindexProviderError::WorkerPanicked)?
    }

    pub(crate) fn client(&self) -> &RaindexClient {
        &self.client
    }

    pub(crate) fn registry_url(&self) -> String {
        self.registry.registry_url()
    }

    pub(crate) fn db_path(&self) -> Option<PathBuf> {
        self.db_path.clone()
    }

    pub(crate) fn rpc_overrides(&self) -> HashMap<String, Vec<String>> {
        self.rpc_overrides.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RaindexProviderError {
    #[error("failed to load registry: {0}")]
    RegistryLoad(String),
    #[error("failed to create raindex client: {0}")]
    ClientInit(String),
    #[error("worker thread panicked")]
    WorkerPanicked,
}

impl From<RaindexProviderError> for ApiError {
    fn from(e: RaindexProviderError) -> Self {
        tracing::error!(error = %e, "raindex client provider error");
        match e {
            RaindexProviderError::RegistryLoad(_) => {
                ApiError::Internal("registry configuration error".into())
            }
            RaindexProviderError::ClientInit(_) => {
                ApiError::Internal("failed to initialize orderbook client".into())
            }
            RaindexProviderError::WorkerPanicked => {
                ApiError::Internal("failed to initialize client runtime".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rocket::async_test]
    async fn test_load_fails_with_unreachable_url() {
        let result = RaindexProvider::load("http://127.0.0.1:1/registry.txt", None, HashMap::new()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RaindexProviderError::RegistryLoad(_)
        ));
    }

    #[rocket::async_test]
    async fn test_load_fails_with_invalid_format() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let body = "this is not a valid registry file format";
        let response = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
        });

        let result = RaindexProvider::load(&format!("http://{addr}/registry.txt"), None, HashMap::new()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RaindexProviderError::RegistryLoad(_)
        ));
    }

    #[rocket::async_test]
    async fn test_load_succeeds_with_valid_registry() {
        let config = crate::test_helpers::mock_raindex_config().await;
        assert!(!config.registry_url().is_empty());
    }

    #[test]
    fn test_neutralize_metaboards_replaces_entries() {
        let yaml = "\
version: 4
networks:
  base:
    chain-id: 8453
metaboards:
  base: https://api.goldsky.com/metaboard
  ethereum: https://api.goldsky.com/metaboard-eth
orderbooks:
  base:
    address: 0xabc
";
        let result = neutralize_metaboards(yaml);
        assert!(result.contains("metaboards:\n  _disabled: https://localhost\n"));
        assert!(!result.contains("api.goldsky.com"));
        assert!(result.contains("orderbooks:"));
        assert!(result.contains("networks:"));
    }

    #[test]
    fn test_neutralize_metaboards_no_section() {
        let yaml = "\
version: 4
networks:
  base:
    chain-id: 8453
";
        let result = neutralize_metaboards(yaml);
        assert_eq!(result.trim(), yaml.trim());
        assert!(!result.contains("metaboards"));
    }

    #[test]
    fn test_neutralize_metaboards_at_end_of_file() {
        let yaml = "\
version: 4
metaboards:
  base: https://api.goldsky.com/metaboard";
        let result = neutralize_metaboards(yaml);
        assert!(result.contains("metaboards:\n  _disabled: https://localhost\n"));
        assert!(!result.contains("api.goldsky.com"));
    }

    #[test]
    fn test_apply_rpc_override_replaces_single_url() {
        let yaml = "\
version: 4
networks:
  base:
    rpcs:
      - https://base-rpc.publicnode.com
    chain-id: 8453
    currency: ETH
orderbooks:
  base:
    address: 0xabc
";
        let mut overrides = HashMap::new();
        overrides.insert(
            "base".to_string(),
            vec!["https://alchemy.example/v2/key".to_string()],
        );
        let result = apply_rpc_override(yaml, &overrides);

        assert!(result.contains("- https://alchemy.example/v2/key"));
        assert!(!result.contains("publicnode.com"));
        // Other fields preserved.
        assert!(result.contains("chain-id: 8453"));
        assert!(result.contains("currency: ETH"));
        assert!(result.contains("orderbooks:"));
    }

    #[test]
    fn test_apply_rpc_override_replaces_multi_url() {
        let yaml = "\
networks:
  base:
    rpcs:
      - https://old-1
      - https://old-2
      - https://old-3
    chain-id: 8453
";
        let mut overrides = HashMap::new();
        overrides.insert(
            "base".to_string(),
            vec!["https://new-a".to_string(), "https://new-b".to_string()],
        );
        let result = apply_rpc_override(yaml, &overrides);

        assert!(result.contains("- https://new-a"));
        assert!(result.contains("- https://new-b"));
        assert!(!result.contains("old-1"));
        assert!(!result.contains("old-2"));
        assert!(!result.contains("old-3"));
        assert!(result.contains("chain-id: 8453"));
    }

    #[test]
    fn test_apply_rpc_override_only_named_network() {
        let yaml = "\
networks:
  base:
    rpcs:
      - https://base-rpc
    chain-id: 8453
  ethereum:
    rpcs:
      - https://eth-rpc
    chain-id: 1
";
        let mut overrides = HashMap::new();
        overrides.insert("base".to_string(), vec!["https://new-base".to_string()]);
        let result = apply_rpc_override(yaml, &overrides);

        assert!(result.contains("- https://new-base"));
        assert!(!result.contains("base-rpc"));
        // Ethereum block untouched.
        assert!(result.contains("- https://eth-rpc"));
    }

    #[test]
    fn test_apply_rpc_override_empty_passthrough() {
        let yaml = "networks:\n  base:\n    rpcs:\n      - https://x\n";
        let result = apply_rpc_override(yaml, &HashMap::new());
        assert_eq!(result, yaml);
    }

    #[test]
    fn test_apply_rpc_override_unknown_network_passthrough() {
        let yaml = "\
networks:
  base:
    rpcs:
      - https://base-rpc
";
        let mut overrides = HashMap::new();
        overrides.insert("polygon".to_string(), vec!["https://poly".to_string()]);
        let result = apply_rpc_override(yaml, &overrides);
        assert!(result.contains("- https://base-rpc"));
        assert!(!result.contains("poly"));
    }

    #[test]
    fn test_error_maps_to_api_error() {
        let err = RaindexProviderError::RegistryLoad("test".into());
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "registry configuration error")
        );

        let err = RaindexProviderError::ClientInit("test".into());
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "failed to initialize orderbook client")
        );
    }
}
