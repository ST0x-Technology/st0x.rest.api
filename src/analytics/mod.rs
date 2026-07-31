//! Server-side PostHog analytics.
//!
//! Events are attributed to the calling API client (via the Basic-auth key's
//! `label`/`owner`) so we can understand trade volume and unique traders per client
//! — the st0x site versus third-party integrators. Sends go to the same PostHog
//! project as the site so both halves live together.
//!
//! Capture is fire-and-forget: a failing or slow PostHog never affects a request.
//! When `POSTHOG_PROJECT_TOKEN` is unset, event construction and attribution are
//! skipped.

mod events;
mod posthog;
#[cfg(test)]
mod recording;

pub(crate) use events::{
    api_request_event, swap_calldata_failed_event, swap_calldata_generated_event,
    swap_quote_failed_event, swap_quoted_event, swap_quoted_v2_event, ApiVersion, SwapFailure,
};
pub(crate) use posthog::PostHogSink;
#[cfg(test)]
pub(crate) use recording::RecordingSink;

use std::sync::Arc;

/// A single analytics event ready to be sent to PostHog.
#[derive(Debug, Clone)]
pub(crate) struct AnalyticsEvent {
    pub event: &'static str,
    pub distinct_id: String,
    pub properties: serde_json::Map<String, serde_json::Value>,
}

/// Destination for analytics events. Implementations must be non-blocking.
pub(crate) trait AnalyticsSink: Send + Sync {
    fn capture(&self, event: AnalyticsEvent);
}

/// Rocket-managed handle to the active analytics sink.
#[derive(Clone)]
pub(crate) struct Analytics(Option<Arc<dyn AnalyticsSink>>);

impl Analytics {
    /// Wrap an explicit sink. Used by tests to inject a recording sink.
    #[cfg(test)]
    pub(crate) fn new(sink: Arc<dyn AnalyticsSink>) -> Self {
        Self(Some(sink))
    }

    /// A disabled (no-op) handle.
    pub(crate) fn disabled() -> Self {
        Self(None)
    }

    /// Whether analytics is configured with an active sink.
    pub(crate) fn is_enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Build and capture an event only when analytics is enabled.
    pub(crate) fn capture(&self, build_event: impl FnOnce() -> AnalyticsEvent) {
        if let Some(sink) = &self.0 {
            sink.capture(build_event());
        }
    }

    /// Build from the environment.
    ///
    /// Analytics is disabled when `POSTHOG_PROJECT_TOKEN` is unset or empty.
    /// `POSTHOG_HOST` defaults to the EU PostHog cloud.
    pub(crate) fn from_env() -> Self {
        match std::env::var("POSTHOG_PROJECT_TOKEN") {
            Ok(project_token) if !project_token.trim().is_empty() => {
                let host = std::env::var("POSTHOG_HOST")
                    .ok()
                    .filter(|h| !h.trim().is_empty())
                    .unwrap_or_else(|| "https://eu.i.posthog.com".to_string());
                match PostHogSink::new(project_token, host) {
                    Ok(sink) => {
                        tracing::info!("posthog analytics enabled");
                        Self(Some(Arc::new(sink)))
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to initialize posthog analytics; disabling");
                        Self::disabled()
                    }
                }
            }
            _ => {
                tracing::info!("posthog analytics disabled (POSTHOG_PROJECT_TOKEN not set)");
                Self::disabled()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn disabled_analytics_does_not_build_events() {
        let analytics = Analytics::disabled();
        let built = AtomicBool::new(false);

        analytics.capture(|| {
            built.store(true, Ordering::Relaxed);
            AnalyticsEvent {
                event: "should_not_be_built",
                distinct_id: "unused".to_string(),
                properties: serde_json::Map::new(),
            }
        });

        assert!(!built.load(Ordering::Relaxed));
    }
}
