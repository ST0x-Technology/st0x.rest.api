use crate::error::ApiError;
use rain_orderbook_js_api::registry::DotrainRegistry;

#[derive(Debug)]
pub(crate) struct RaindexClientProvider {
    registry: DotrainRegistry,
}

impl RaindexClientProvider {
    pub(crate) async fn load(registry_url: &str) -> Result<Self, RaindexClientProviderError> {
        let url = registry_url.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("thread-local runtime");
            let result = rt.block_on(async {
                DotrainRegistry::new(url)
                    .await
                    .map_err(|e| RaindexClientProviderError::RegistryLoad(e.to_string()))
            });
            let _ = tx.send(result);
        });
        let registry = rx.await.map_err(|_| {
            RaindexClientProviderError::RegistryLoad("registry loader thread panicked".into())
        })??;
        Ok(Self { registry })
    }

    pub(crate) fn registry(&self) -> &DotrainRegistry {
        &self.registry
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RaindexClientProviderError {
    #[error("failed to load registry: {0}")]
    RegistryLoad(String),
    #[error("failed to create raindex client: {0}")]
    ClientCreate(String),
}

impl From<RaindexClientProviderError> for ApiError {
    fn from(e: RaindexClientProviderError) -> Self {
        tracing::error!(error = %e, "raindex client provider error");
        match e {
            RaindexClientProviderError::RegistryLoad(_) => {
                ApiError::Internal("registry configuration error".into())
            }
            RaindexClientProviderError::ClientCreate(_) => {
                ApiError::Internal("failed to initialize orderbook client".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rocket::async_test]
    async fn test_load_fails_with_unreachable_url() {
        let result = RaindexClientProvider::load("http://127.0.0.1:1/registry.txt").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RaindexClientProviderError::RegistryLoad(_)
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

        let result = RaindexClientProvider::load(&format!("http://{addr}/registry.txt")).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RaindexClientProviderError::RegistryLoad(_)
        ));
    }

    #[rocket::async_test]
    async fn test_load_succeeds_with_valid_registry() {
        let config = crate::test_helpers::mock_raindex_config().await;
        assert!(!config.registry().settings().is_empty());
    }

    #[test]
    fn test_error_maps_to_api_error() {
        let err = RaindexClientProviderError::RegistryLoad("test".into());
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "registry configuration error")
        );

        let err = RaindexClientProviderError::ClientCreate("test".into());
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "failed to initialize orderbook client")
        );
    }
}
