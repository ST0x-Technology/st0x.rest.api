pub(crate) mod config;

pub(crate) use config::{RaindexProvider, RaindexProviderError};
pub(crate) type SharedRaindexProvider = tokio::sync::RwLock<RaindexProvider>;
