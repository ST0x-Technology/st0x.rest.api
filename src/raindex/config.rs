use crate::error::ApiError;
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_js_api::registry::DotrainRegistry;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct RaindexProvider {
    registry: DotrainRegistry,
    client: RaindexClient,
    db_path: Option<PathBuf>,
}

impl RaindexProvider {
    pub(crate) async fn load(
        registry_url: &str,
        db_path: Option<PathBuf>,
    ) -> Result<Self, RaindexProviderError> {
        let url = registry_url.to_string();
        let db = db_path.clone();

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

                let client = registry
                    .get_raindex_client(db.clone())
                    .map_err(|e| RaindexProviderError::ClientInit(e.to_string()))?;

                Ok(RaindexProvider {
                    registry,
                    client,
                    db_path: db,
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
        let result = RaindexProvider::load("http://127.0.0.1:1/registry.txt", None).await;
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

        let result = RaindexProvider::load(&format!("http://{addr}/registry.txt"), None).await;
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
