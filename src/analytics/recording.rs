//! Test-only sink that records captured events for assertions.

use super::{AnalyticsEvent, AnalyticsSink};
use std::sync::{Arc, Mutex};

/// In-memory analytics sink used to assert captured events in tests.
#[derive(Clone, Default)]
pub(crate) struct RecordingSink {
    events: Arc<Mutex<Vec<AnalyticsEvent>>>,
}

impl RecordingSink {
    /// Create an empty recording sink.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A snapshot of the events captured so far.
    pub(crate) fn events(&self) -> Vec<AnalyticsEvent> {
        self.events
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

impl AnalyticsSink for RecordingSink {
    fn capture(&self, event: AnalyticsEvent) {
        if let Ok(mut guard) = self.events.lock() {
            guard.push(event);
        }
    }
}
