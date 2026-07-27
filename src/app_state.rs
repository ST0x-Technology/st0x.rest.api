use crate::attribution::AttributionState;
use crate::cache::RouteResponseCaches;
use crate::registry_artifact::RegistryArtifactStore;

pub(crate) struct ApplicationState {
    pub registry_artifact_store: RegistryArtifactStore,
    pub response_caches: RouteResponseCaches,
    pub attribution: AttributionState,
}

impl ApplicationState {
    pub(crate) fn new(
        registry_artifact_store: RegistryArtifactStore,
        response_caches: RouteResponseCaches,
        attribution: AttributionState,
    ) -> Self {
        Self {
            registry_artifact_store,
            response_caches,
            attribution,
        }
    }
}
