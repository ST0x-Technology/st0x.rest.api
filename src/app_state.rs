use crate::cache::RouteResponseCaches;
use crate::registry_artifact::RegistryArtifactStore;
use crate::signing::GatingState;

pub(crate) struct ApplicationState {
    pub registry_artifact_store: RegistryArtifactStore,
    pub response_caches: RouteResponseCaches,
    pub gating: GatingState,
}

impl ApplicationState {
    pub(crate) fn new(
        registry_artifact_store: RegistryArtifactStore,
        response_caches: RouteResponseCaches,
        gating: GatingState,
    ) -> Self {
        Self {
            registry_artifact_store,
            response_caches,
            gating,
        }
    }
}
