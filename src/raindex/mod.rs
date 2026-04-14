pub(crate) mod config;
pub(crate) mod gating_injector;

pub(crate) use config::{RaindexProvider, RaindexProviderError};
pub(crate) use gating_injector::ApiGatingInjector;
pub(crate) type SharedRaindexProvider = tokio::sync::RwLock<RaindexProvider>;
