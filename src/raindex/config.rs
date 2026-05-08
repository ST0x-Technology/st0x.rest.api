use crate::error::ApiError;
use crate::raindex::materialized_registry::{MaterializedRegistry, MaterializedRegistryStats};
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_js_api::registry::DotrainRegistry;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct RaindexProvider {
    client: RaindexClient,
    db_path: Option<PathBuf>,
    public_registry_url: String,
    rpc_urls_path: Option<PathBuf>,
    materialized_registry: Option<MaterializedRegistry>,
}

impl RaindexProvider {
    pub(crate) async fn load(
        registry_url: &str,
        db_path: Option<PathBuf>,
        rpc_urls_path: Option<&Path>,
    ) -> Result<Self, RaindexProviderError> {
        let rpc_urls_path = rpc_urls_path.map(Path::to_path_buf);
        let materialized_registry = match rpc_urls_path.as_deref() {
            Some(path) => match MaterializedRegistry::start(registry_url, path).await {
                Ok(materialized_registry) => materialized_registry,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        path = %path.display(),
                        "failed to materialize private registry; using public registry"
                    );
                    None
                }
            },
            None => None,
        };

        if let Some(materialized_registry) = materialized_registry {
            match Self::load_from_source(
                RegistrySource::materialized(registry_url, materialized_registry),
                db_path.clone(),
                rpc_urls_path.clone(),
            )
            .await
            {
                Ok(provider) => return Ok(provider),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "failed to load materialized registry; using public registry"
                    );
                }
            }
        }

        Self::load_from_source(RegistrySource::public(registry_url), db_path, rpc_urls_path).await
    }

    async fn load_from_source(
        source: RegistrySource,
        db_path: Option<PathBuf>,
        rpc_urls_path: Option<PathBuf>,
    ) -> Result<Self, RaindexProviderError> {
        let db = db_path.clone();
        let source_kind = source.kind();
        let RegistrySource {
            load_url,
            public_registry_url,
            materialized_registry,
        } = source;

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
                let registry = DotrainRegistry::new(load_url)
                    .await
                    .map_err(|e| RaindexProviderError::RegistryLoad(e.to_string()))?;

                let client = registry
                    .get_raindex_client(db.clone())
                    .await
                    .map_err(|e| RaindexProviderError::ClientInit(e.to_string()))?;

                Ok(RaindexProvider {
                    client,
                    db_path: db,
                    public_registry_url,
                    rpc_urls_path,
                    materialized_registry,
                })
            });

            let _ = tx.send(result);
        });

        let provider = rx
            .await
            .map_err(|_| RaindexProviderError::WorkerPanicked)??;
        let maybe_stats = provider.private_registry_stats();
        let stats = maybe_stats.unwrap_or_default();

        tracing::info!(
            registry_source = source_kind,
            private_registry_active = maybe_stats.is_some(),
            private_rpc_count = stats.private_rpc_count,
            public_rpc_count = stats.public_rpc_count,
            merged_rpc_count = stats.merged_rpc_count,
            duplicate_rpc_count = stats.duplicate_rpc_count,
            "raindex provider loaded"
        );

        Ok(provider)
    }

    pub(crate) fn client(&self) -> &RaindexClient {
        &self.client
    }

    pub(crate) fn registry_url(&self) -> String {
        self.public_registry_url.clone()
    }

    pub(crate) fn db_path(&self) -> Option<PathBuf> {
        self.db_path.clone()
    }

    pub(crate) fn rpc_urls_path(&self) -> Option<PathBuf> {
        self.rpc_urls_path.clone()
    }

    pub(crate) fn private_registry_stats(&self) -> Option<MaterializedRegistryStats> {
        self.materialized_registry
            .as_ref()
            .map(MaterializedRegistry::stats)
    }
}

#[derive(Debug)]
struct RegistrySource {
    load_url: String,
    public_registry_url: String,
    materialized_registry: Option<MaterializedRegistry>,
}

impl RegistrySource {
    fn kind(&self) -> &'static str {
        if self.materialized_registry.is_some() {
            "materialized"
        } else {
            "public"
        }
    }

    fn public(registry_url: &str) -> Self {
        Self {
            load_url: registry_url.to_string(),
            public_registry_url: registry_url.to_string(),
            materialized_registry: None,
        }
    }

    fn materialized(
        public_registry_url: &str,
        materialized_registry: MaterializedRegistry,
    ) -> Self {
        Self {
            load_url: materialized_registry.registry_url().to_string(),
            public_registry_url: public_registry_url.to_string(),
            materialized_registry: Some(materialized_registry),
        }
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
        let result = RaindexProvider::load("http://127.0.0.1:1/registry.txt", None, None).await;
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

        let result =
            RaindexProvider::load(&format!("http://{addr}/registry.txt"), None, None).await;
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

    #[rocket::async_test]
    async fn test_load_keeps_public_registry_url() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rpc_urls_path = dir.path().join("private-rpcs.txt");
        tokio::fs::write(&rpc_urls_path, "https://private.example/rpc")
            .await
            .expect("write private rpcs");
        let registry_url = crate::test_helpers::mock_raindex_registry_url().await;

        let config = RaindexProvider::load(&registry_url, None, Some(&rpc_urls_path))
            .await
            .expect("load private materialized registry");

        assert_eq!(config.registry_url(), registry_url);
        let materialized_registry = config
            .materialized_registry
            .as_ref()
            .expect("materialized registry retained");
        assert!(materialized_registry
            .registry_url()
            .starts_with("http://127.0.0.1:"));
        assert_ne!(materialized_registry.registry_url(), registry_url);
        assert_eq!(
            config.rpc_urls_path().as_deref(),
            Some(rpc_urls_path.as_path())
        );
        assert_eq!(
            config.private_registry_stats(),
            Some(MaterializedRegistryStats {
                private_rpc_count: 1,
                public_rpc_count: 1,
                merged_rpc_count: 2,
                duplicate_rpc_count: 0,
            })
        );
    }

    #[rocket::async_test]
    async fn test_load_with_invalid_private_rpcs_falls_back_to_public_registry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rpc_urls_path = dir.path().join("private-rpcs.txt");
        tokio::fs::write(&rpc_urls_path, "not a url")
            .await
            .expect("write invalid private rpcs");
        let registry_url = crate::test_helpers::mock_raindex_registry_url().await;

        let config = RaindexProvider::load(&registry_url, None, Some(&rpc_urls_path))
            .await
            .expect("fallback public registry loads");

        assert_eq!(config.registry_url(), registry_url);
        assert!(config.materialized_registry.is_none());
        assert_eq!(
            config.rpc_urls_path().as_deref(),
            Some(rpc_urls_path.as_path())
        );
        assert_eq!(config.private_registry_stats(), None);
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
