use crate::error::ApiError;
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_js_api::registry::DotrainRegistry;
use rain_orderbook_js_api::registry::DotrainRegistryError;

#[derive(Debug)]
pub(crate) struct RaindexProvider {
    registry: DotrainRegistry,
}

impl RaindexProvider {
    pub(crate) async fn load(registry_url: &str) -> Result<Self, RaindexProviderError> {
        let url = registry_url.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<DotrainRegistry, String>>();

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = tx.send(Err(format!(
                        "failed to initialize registry runtime: {error}"
                    )));
                    return;
                }
            };

            let result = runtime.block_on(async { DotrainRegistry::new(url).await });
            let _ = tx.send(result.map_err(|error| error.to_string()));
        });

        let registry_result = rx.await.map_err(|_| {
            RaindexProviderError::RegistryLoad("registry loader thread panicked".into())
        })?;
        let registry = registry_result.map_err(RaindexProviderError::RegistryLoad)?;
        Ok(Self { registry })
    }

    pub(crate) fn get_raindex_client(&self) -> Result<RaindexClient, RaindexProviderError> {
        self.registry
            .get_raindex_client()
            .map_err(|error| RaindexProviderError::ClientCreate(Box::new(error)))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RaindexProviderError {
    #[error("failed to load registry: {0}")]
    RegistryLoad(String),
    #[error("failed to create raindex client")]
    ClientCreate(#[source] Box<DotrainRegistryError>),
}

impl From<RaindexProviderError> for ApiError {
    fn from(e: RaindexProviderError) -> Self {
        tracing::error!(error = %e, "raindex client provider error");
        match e {
            RaindexProviderError::RegistryLoad(_) => {
                ApiError::Internal("registry configuration error".into())
            }
            RaindexProviderError::ClientCreate(_) => {
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
        let result = RaindexProvider::load("http://127.0.0.1:1/registry.txt").await;
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

        let result = RaindexProvider::load(&format!("http://{addr}/registry.txt")).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RaindexProviderError::RegistryLoad(_)
        ));
    }

    #[rocket::async_test]
    async fn test_load_succeeds_with_valid_registry() {
        let config = crate::test_helpers::mock_raindex_config().await;
        let client = config.get_raindex_client();
        assert!(client.is_ok());
    }

    #[rocket::async_test]
    async fn test_get_raindex_client_fails_with_invalid_settings() {
        let config = crate::test_helpers::mock_invalid_raindex_config().await;
        let client = config.get_raindex_client();
        assert!(matches!(
            client.unwrap_err(),
            RaindexProviderError::ClientCreate(_)
        ));
    }

    #[test]
    fn test_error_maps_to_api_error() {
        let err = RaindexProviderError::RegistryLoad("test".into());
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "registry configuration error")
        );

        let err = RaindexProviderError::ClientCreate(Box::new(
            DotrainRegistryError::InvalidRegistryFormat("test".into()),
        ));
        let api_err: ApiError = err.into();
        assert!(
            matches!(api_err, ApiError::Internal(msg) if msg == "failed to initialize orderbook client")
        );
    }
}
