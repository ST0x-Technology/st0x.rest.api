pub(crate) mod config;
pub(crate) mod materialized_registry;

pub(crate) use config::RaindexProvider;
pub(crate) type SharedRaindexProvider = tokio::sync::RwLock<RaindexProvider>;
