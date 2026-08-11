pub(crate) mod config;

pub(crate) use config::{RaindexProvider, RaindexProviderError};
pub(crate) type SharedRaindexProvider = std::sync::Arc<tokio::sync::RwLock<RaindexProvider>>;
