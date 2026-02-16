use crate::error::ApiError;
use rain_orderbook_common::raindex_client::RaindexClient;
use rain_orderbook_js_api::registry::DotrainRegistry;

enum WorkerError {
    RuntimeInit(std::io::Error),
    Api(String),
}

#[derive(Debug)]
pub(crate) struct RaindexProvider {
    registry: DotrainRegistry,
}

impl RaindexProvider {
    pub(crate) async fn load(registry_url: &str) -> Result<Self, RaindexProviderError> {
        let url = registry_url.to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<DotrainRegistry, WorkerError>>();

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = tx.send(Err(WorkerError::RuntimeInit(error)));
                    return;
                }
            };

            let result = runtime.block_on(async { DotrainRegistry::new(url).await });
            let _ = tx.send(result.map_err(|e| WorkerError::Api(e.to_string())));
        });

        rx.await
            .map_err(|_| RaindexProviderError::WorkerPanicked)?
            .map(|registry| Self { registry })
            .map_err(|e| match e {
                WorkerError::RuntimeInit(e) => RaindexProviderError::RegistryRuntimeInit(e),
                WorkerError::Api(e) => RaindexProviderError::RegistryLoad(e),
            })
    }

    pub(crate) async fn run_with_registry<T, F, Fut>(&self, f: F) -> Result<T, RaindexProviderError>
    where
        T: Send + 'static,
        F: FnOnce(DotrainRegistry) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = T>,
    {
        let registry = self.registry.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<T, WorkerError>>();

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(error) => {
                    tracing::error!(error = %error, "failed to build registry runtime");
                    let _ = tx.send(Err(WorkerError::RuntimeInit(error)));
                    return;
                }
            };

            let _ = tx.send(Ok(runtime.block_on(f(registry))));
        });

        rx.await
            .map_err(|_| RaindexProviderError::WorkerPanicked)?
            .map_err(|e| match e {
                WorkerError::RuntimeInit(e) => RaindexProviderError::RegistryRuntimeInit(e),
                WorkerError::Api(e) => RaindexProviderError::RegistryLoad(e),
            })
    }

    pub(crate) async fn run_with_client<T, F, Fut>(&self, f: F) -> Result<T, RaindexProviderError>
    where
        T: Send + 'static,
        F: FnOnce(RaindexClient) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = T>,
    {
        let registry = self.registry.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<T, WorkerError>>();

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(error) => {
                    tracing::error!(error = %error, "failed to build client runtime");
                    let _ = tx.send(Err(WorkerError::RuntimeInit(error)));
                    return;
                }
            };

            let result = runtime.block_on(async {
                let client = registry
                    .get_raindex_client()
                    .map_err(|e| WorkerError::Api(e.to_string()))?;
                Ok(f(client).await)
            });

            let _ = tx.send(result);
        });

        rx.await
            .map_err(|_| RaindexProviderError::WorkerPanicked)?
            .map_err(|e| match e {
                WorkerError::RuntimeInit(e) => RaindexProviderError::ClientRuntimeInit(e),
                WorkerError::Api(e) => RaindexProviderError::ClientInit(e),
            })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RaindexProviderError {
    #[error("failed to load registry: {0}")]
    RegistryLoad(String),
    #[error("failed to initialize registry runtime")]
    RegistryRuntimeInit(#[source] std::io::Error),
    #[error("failed to create raindex client: {0}")]
    ClientInit(String),
    #[error("failed to initialize client runtime")]
    ClientRuntimeInit(#[source] std::io::Error),
    #[error("worker thread panicked")]
    WorkerPanicked,
}

impl From<RaindexProviderError> for ApiError {
    fn from(e: RaindexProviderError) -> Self {
        tracing::error!(error = %e, "raindex client provider error");
        match e {
            RaindexProviderError::RegistryLoad(_)
            | RaindexProviderError::RegistryRuntimeInit(_) => {
                ApiError::Internal("registry configuration error".into())
            }
            RaindexProviderError::ClientInit(_) => {
                ApiError::Internal("failed to initialize orderbook client".into())
            }
            RaindexProviderError::ClientRuntimeInit(_) | RaindexProviderError::WorkerPanicked => {
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
        let result = config.run_with_client(|_client| async { "ok" }).await;
        assert!(result.is_ok());
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
