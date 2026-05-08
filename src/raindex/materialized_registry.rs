use rain_orderbook_app_settings::yaml::{load_yaml, YamlError};
use rain_orderbook_js_api::registry::DotrainRegistry;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use strict_yaml_rust::strict_yaml::Hash;
use strict_yaml_rust::{StrictYaml, StrictYamlEmitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const REGISTRY_PATH: &str = "/registry";
const SETTINGS_PATH: &str = "/settings.yaml";
const NETWORKS_KEY: &str = "networks";
const CHAIN_ID_KEY: &str = "chain-id";
const RPCS_KEY: &str = "rpcs";

#[derive(Debug)]
pub(crate) struct MaterializedRegistry {
    registry_url: String,
    stats: MaterializedRegistryStats,
    handle: tokio::task::JoinHandle<()>,
}

impl MaterializedRegistry {
    pub(crate) async fn start(
        public_registry_url: &str,
        rpc_urls_path: &Path,
    ) -> Result<Option<Self>, MaterializedRegistryError> {
        let rpc_urls = match read_private_rpc_urls(rpc_urls_path).await {
            Ok(urls) if urls.is_empty() => {
                tracing::warn!(
                    path = %rpc_urls_path.display(),
                    "private registry RPC URL file is empty; using public registry"
                );
                return Ok(None);
            }
            Ok(urls) => urls,
            Err(MaterializedRegistryError::RpcUrlsFileMissing(path)) => {
                tracing::warn!(
                    path = %path.display(),
                    "private registry RPC URL file is missing; using public registry"
                );
                return Ok(None);
            }
            Err(e) => return Err(e),
        };

        let snapshot = load_public_registry_snapshot(public_registry_url).await?;
        let materialized_settings = materialize_settings(&snapshot.settings, &rpc_urls)?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(MaterializedRegistryError::Bind)?;
        let addr = listener
            .local_addr()
            .map_err(MaterializedRegistryError::LocalAddr)?;
        let settings_url = format!("http://{addr}{SETTINGS_PATH}");
        let registry_url = format!("http://{addr}{REGISTRY_PATH}");
        let registry = rewrite_registry_settings_url(&snapshot.registry, &settings_url)?;
        let stats = materialized_settings.stats;

        let handle = tokio::spawn(async move {
            serve(listener, registry, materialized_settings.yaml).await;
        });

        tracing::info!(
            path = %rpc_urls_path.display(),
            chain_id = crate::CHAIN_ID,
            private_rpc_count = stats.private_rpc_count,
            public_rpc_count = stats.public_rpc_count,
            merged_rpc_count = stats.merged_rpc_count,
            duplicate_rpc_count = stats.duplicate_rpc_count,
            "serving materialized registry on loopback"
        );

        Ok(Some(Self {
            registry_url,
            stats,
            handle,
        }))
    }

    pub(crate) fn registry_url(&self) -> &str {
        &self.registry_url
    }

    pub(crate) fn stats(&self) -> MaterializedRegistryStats {
        self.stats
    }
}

impl Drop for MaterializedRegistry {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MaterializedRegistryStats {
    pub(crate) private_rpc_count: usize,
    pub(crate) public_rpc_count: usize,
    pub(crate) merged_rpc_count: usize,
    pub(crate) duplicate_rpc_count: usize,
}

#[derive(Debug)]
struct MaterializedSettings {
    yaml: String,
    stats: MaterializedRegistryStats,
}

#[derive(Debug)]
struct PublicRegistrySnapshot {
    registry: String,
    settings: String,
}

async fn load_public_registry_snapshot(
    registry_url: &str,
) -> Result<PublicRegistrySnapshot, MaterializedRegistryError> {
    let url = registry_url.to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(Err(MaterializedRegistryError::RegistryLoad(e.to_string())));
                return;
            }
        };

        let result = runtime.block_on(async {
            let registry = DotrainRegistry::new(url)
                .await
                .map_err(|e| MaterializedRegistryError::RegistryLoad(e.to_string()))?;
            Ok(PublicRegistrySnapshot {
                registry: registry.registry(),
                settings: registry.settings(),
            })
        });

        let _ = tx.send(result);
    });

    rx.await
        .map_err(|_| MaterializedRegistryError::WorkerPanicked)?
}

async fn read_private_rpc_urls(path: &Path) -> Result<Vec<String>, MaterializedRegistryError> {
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(MaterializedRegistryError::RpcUrlsFileMissing(
                path.to_path_buf(),
            ));
        }
        Err(e) => return Err(MaterializedRegistryError::ReadRpcUrls(e)),
    };

    parse_private_rpc_urls(&contents)
}

fn parse_private_rpc_urls(contents: &str) -> Result<Vec<String>, MaterializedRegistryError> {
    let mut urls = Vec::new();

    for (index, raw_url) in contents.split(',').enumerate() {
        let url = raw_url.trim();
        if url.is_empty() {
            continue;
        }

        reqwest::Url::parse(url)
            .map_err(|_| MaterializedRegistryError::InvalidRpcUrl { index: index + 1 })?;
        urls.push(url.to_string());
    }

    Ok(urls)
}

fn materialize_settings(
    settings: &str,
    private_rpcs: &[String],
) -> Result<MaterializedSettings, MaterializedRegistryError> {
    let mut settings = load_yaml(settings).map_err(MaterializedRegistryError::ParseSettings)?;
    let network = network_for_chain_id_mut(&mut settings, crate::CHAIN_ID)?;
    let network = strict_hash_mut(network)
        .ok_or_else(|| invalid_settings("matched network must be a mapping"))?;

    let rpcs_key = yaml_key(RPCS_KEY);
    let existing_rpcs = match network.get(&rpcs_key) {
        Some(StrictYaml::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect::<Vec<_>>(),
        Some(_) => {
            return Err(invalid_settings("matched network rpcs must be a list"));
        }
        None => Vec::new(),
    };

    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for rpc in private_rpcs.iter().chain(existing_rpcs.iter()) {
        if seen.insert(rpc.clone()) {
            merged.push(StrictYaml::String(rpc.clone()));
        }
    }

    let stats = MaterializedRegistryStats {
        private_rpc_count: private_rpcs.len(),
        public_rpc_count: existing_rpcs.len(),
        merged_rpc_count: merged.len(),
        duplicate_rpc_count: private_rpcs.len() + existing_rpcs.len() - merged.len(),
    };

    network.insert(rpcs_key, StrictYaml::Array(merged));
    let yaml = emit_yaml(&settings).map_err(MaterializedRegistryError::SerializeSettings)?;

    Ok(MaterializedSettings { yaml, stats })
}

fn network_for_chain_id_mut(
    settings: &mut StrictYaml,
    chain_id: u32,
) -> Result<&mut StrictYaml, MaterializedRegistryError> {
    let root = strict_hash_mut(settings)
        .ok_or_else(|| invalid_settings("settings root must be a mapping"))?;
    let networks = strict_hash_mut(mapping_get_mut(root, NETWORKS_KEY)?)
        .ok_or_else(|| invalid_settings("networks must be a mapping"))?;

    for (_, network) in networks.iter_mut() {
        let Some(mapping) = network.as_hash() else {
            continue;
        };
        let Some(value) = mapping_get(mapping, CHAIN_ID_KEY) else {
            continue;
        };
        if yaml_chain_id(value) == Some(chain_id) {
            return Ok(network);
        }
    }

    Err(MaterializedRegistryError::ChainIdNotFound(chain_id))
}

fn mapping_get_mut<'a>(
    mapping: &'a mut Hash,
    key: &str,
) -> Result<&'a mut StrictYaml, MaterializedRegistryError> {
    mapping
        .get_mut(&yaml_key(key))
        .ok_or_else(|| invalid_settings(&format!("missing {key}")))
}

fn mapping_get<'a>(mapping: &'a Hash, key: &str) -> Option<&'a StrictYaml> {
    mapping.get(&yaml_key(key))
}

fn yaml_key(key: &str) -> StrictYaml {
    StrictYaml::String(key.to_string())
}

fn strict_hash_mut(value: &mut StrictYaml) -> Option<&mut Hash> {
    match value {
        StrictYaml::Hash(hash) => Some(hash),
        _ => None,
    }
}

fn yaml_chain_id(value: &StrictYaml) -> Option<u32> {
    value.as_str()?.parse().ok()
}

fn emit_yaml(document: &StrictYaml) -> Result<String, YamlError> {
    let mut out = String::new();
    let mut emitter = StrictYamlEmitter::new(&mut out);
    emitter.dump(document)?;

    if out.starts_with("---") {
        Ok(out.trim_start_matches("---").trim_start().to_string())
    } else {
        Ok(out)
    }
}

fn invalid_settings(message: &str) -> MaterializedRegistryError {
    MaterializedRegistryError::InvalidSettings(message.to_string())
}

fn rewrite_registry_settings_url(
    registry: &str,
    settings_url: &str,
) -> Result<String, MaterializedRegistryError> {
    let mut lines = registry
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    lines
        .next()
        .ok_or(MaterializedRegistryError::EmptyRegistry)?;

    let mut rewritten = settings_url.to_string();
    for line in lines {
        rewritten.push('\n');
        rewritten.push_str(line);
    }
    rewritten.push('\n');

    Ok(rewritten)
}

async fn serve(listener: TcpListener, registry: String, settings: String) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            break;
        };
        let registry = registry.clone();
        let settings = settings.clone();

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1));

            let (status, content_type, body) = match path {
                Some(REGISTRY_PATH) => ("200 OK", "text/plain; charset=utf-8", registry),
                Some(SETTINGS_PATH) => ("200 OK", "application/yaml; charset=utf-8", settings),
                _ => (
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    "not found".to_string(),
                ),
            };

            let response = format!(
                "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MaterializedRegistryError {
    #[error("private RPC URL file is missing: {0}")]
    RpcUrlsFileMissing(PathBuf),
    #[error("failed to read private RPC URL file: {0}")]
    ReadRpcUrls(io::Error),
    #[error("invalid private RPC URL at comma-separated position {index}")]
    InvalidRpcUrl { index: usize },
    #[error("failed to load public registry: {0}")]
    RegistryLoad(String),
    #[error("registry worker thread panicked")]
    WorkerPanicked,
    #[error("failed to parse settings YAML: {0}")]
    ParseSettings(YamlError),
    #[error("invalid settings YAML: {0}")]
    InvalidSettings(String),
    #[error("settings YAML does not define chain id {0}")]
    ChainIdNotFound(u32),
    #[error("failed to serialize settings YAML: {0}")]
    SerializeSettings(YamlError),
    #[error("registry file is empty")]
    EmptyRegistry,
    #[error("failed to bind private registry listener: {0}")]
    Bind(io::Error),
    #[error("failed to read private registry listener address: {0}")]
    LocalAddr(io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = r#"version: 5
networks:
  base:
    rpcs:
      - https://mainnet.base.org
      - https://public.example/rpc
    chain-id: 8453
    currency: ETH
"#;

    #[test]
    fn parse_private_rpc_urls_trims_and_skips_empty_entries() {
        let urls = parse_private_rpc_urls(
            " https://private-a.example/rpc ,, https://private-b.example/rpc ",
        )
        .expect("valid urls");

        assert_eq!(
            urls,
            vec![
                "https://private-a.example/rpc".to_string(),
                "https://private-b.example/rpc".to_string()
            ]
        );
    }

    #[test]
    fn parse_private_rpc_urls_rejects_invalid_urls() {
        let err = parse_private_rpc_urls("https://private.example/rpc,not a url")
            .expect_err("invalid url");

        assert!(matches!(
            err,
            MaterializedRegistryError::InvalidRpcUrl { index: 2 }
        ));
    }

    #[test]
    fn prepend_private_rpcs_errors_when_chain_id_is_missing() {
        let settings = r#"version: 5
networks:
  arbitrum:
    rpcs:
      - https://arb.example/rpc
    chain-id: 42161
    currency: ETH
"#;

        let err = materialize_settings(settings, &["https://private.example/rpc".to_string()])
            .expect_err("missing configured chain id");

        assert!(matches!(
            err,
            MaterializedRegistryError::ChainIdNotFound(crate::CHAIN_ID)
        ));
    }

    #[test]
    fn prepend_private_rpcs_errors_when_rpcs_is_not_a_list() {
        let settings = r#"version: 5
networks:
  base:
    rpcs: https://mainnet.base.org
    chain-id: 8453
    currency: ETH
"#;

        let err = materialize_settings(settings, &["https://private.example/rpc".to_string()])
            .expect_err("invalid rpcs shape");

        assert!(matches!(
            err,
            MaterializedRegistryError::InvalidSettings(message) if message == "matched network rpcs must be a list"
        ));
    }

    #[test]
    fn prepend_private_rpcs_preserves_private_first_and_dedupes() {
        let materialized = materialize_settings(
            SETTINGS,
            &[
                "https://private-a.example/rpc".to_string(),
                "https://private-b.example/rpc".to_string(),
                "https://mainnet.base.org".to_string(),
            ],
        )
        .expect("merge settings");

        assert_eq!(
            materialized.stats,
            MaterializedRegistryStats {
                private_rpc_count: 3,
                public_rpc_count: 2,
                merged_rpc_count: 4,
                duplicate_rpc_count: 1,
            }
        );

        let parsed = load_yaml(&materialized.yaml).expect("parse merged settings");
        let rpcs = parsed["networks"]["base"]["rpcs"]
            .as_vec()
            .expect("rpcs list")
            .iter()
            .map(|value| value.as_str().expect("rpc string"))
            .collect::<Vec<_>>();

        assert_eq!(
            rpcs,
            vec![
                "https://private-a.example/rpc",
                "https://private-b.example/rpc",
                "https://mainnet.base.org",
                "https://public.example/rpc"
            ]
        );
    }

    #[test]
    fn rewrite_registry_settings_url_replaces_first_non_empty_line() {
        let registry = rewrite_registry_settings_url(
            "\nhttps://public.example/settings.yaml\norder https://example.com/order.rain\n",
            &format!("http://127.0.0.1:1{SETTINGS_PATH}"),
        )
        .expect("rewrite registry");

        assert_eq!(
            registry,
            format!("http://127.0.0.1:1{SETTINGS_PATH}\norder https://example.com/order.rain\n")
        );
    }

    #[rocket::async_test]
    async fn start_returns_none_when_private_rpc_file_is_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing_path = dir.path().join("missing-private-rpcs.txt");
        let public_registry_url = crate::test_helpers::mock_raindex_registry_url().await;

        let materialized = MaterializedRegistry::start(&public_registry_url, &missing_path)
            .await
            .expect("missing private RPC file should fall back");

        assert!(materialized.is_none());
    }

    #[rocket::async_test]
    async fn start_returns_none_when_private_rpc_file_is_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rpc_urls_path = dir.path().join("private-rpcs.txt");
        tokio::fs::write(&rpc_urls_path, " , ")
            .await
            .expect("write empty private rpcs");
        let public_registry_url = crate::test_helpers::mock_raindex_registry_url().await;

        let materialized = MaterializedRegistry::start(&public_registry_url, &rpc_urls_path)
            .await
            .expect("empty private RPC file should fall back");

        assert!(materialized.is_none());
    }

    #[rocket::async_test]
    async fn start_errors_when_private_rpc_file_has_invalid_url() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rpc_urls_path = dir.path().join("private-rpcs.txt");
        tokio::fs::write(&rpc_urls_path, "not a url")
            .await
            .expect("write invalid private rpcs");
        let public_registry_url = crate::test_helpers::mock_raindex_registry_url().await;

        let err = MaterializedRegistry::start(&public_registry_url, &rpc_urls_path)
            .await
            .expect_err("invalid private RPC file should error");

        assert!(matches!(
            err,
            MaterializedRegistryError::InvalidRpcUrl { index: 1 }
        ));
    }

    #[rocket::async_test]
    async fn start_serves_materialized_registry_on_loopback() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rpc_urls_path = dir.path().join("private-rpcs.txt");
        tokio::fs::write(
            &rpc_urls_path,
            "https://private-a.example/rpc,https://private-b.example/rpc",
        )
        .await
        .expect("write private rpcs");
        let public_registry_url = crate::test_helpers::mock_raindex_registry_url().await;

        let materialized = MaterializedRegistry::start(&public_registry_url, &rpc_urls_path)
            .await
            .expect("start materialized registry")
            .expect("materialized registry");

        assert!(materialized.registry_url().starts_with("http://127.0.0.1:"));
        assert_eq!(
            materialized.stats(),
            MaterializedRegistryStats {
                private_rpc_count: 2,
                public_rpc_count: 1,
                merged_rpc_count: 3,
                duplicate_rpc_count: 0,
            }
        );

        let registry = reqwest::get(materialized.registry_url())
            .await
            .expect("fetch materialized registry")
            .text()
            .await
            .expect("registry body");
        let settings_url = registry.lines().next().expect("settings url");
        assert!(settings_url.starts_with("http://127.0.0.1:"));
        assert_ne!(settings_url, public_registry_url);

        let settings = reqwest::get(settings_url)
            .await
            .expect("fetch materialized settings")
            .text()
            .await
            .expect("settings body");
        let parsed = load_yaml(&settings).expect("parse settings");
        let rpcs = parsed["networks"]["base"]["rpcs"]
            .as_vec()
            .expect("rpcs list")
            .iter()
            .map(|value| value.as_str().expect("rpc string"))
            .collect::<Vec<_>>();

        assert_eq!(
            rpcs,
            vec![
                "https://private-a.example/rpc",
                "https://private-b.example/rpc",
                "https://mainnet.base.org"
            ]
        );
    }
}
