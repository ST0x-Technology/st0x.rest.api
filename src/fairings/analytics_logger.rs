//! Emits an `api_request` analytics event for every authenticated request, mirroring
//! the `UsageLogger` fairing but sending to PostHog for cross-client volume analysis.
//! Unauthenticated requests are skipped (no client to attribute).

use crate::analytics::{api_request_event, Analytics};
use crate::auth::CachedAuthClient;
use crate::fairings::request_span_for;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::{Data, Request, Response};
use std::time::Instant;

struct AnalyticsStart(Instant);

/// Rocket fairing that records authenticated request analytics.
pub struct AnalyticsFairing;

#[rocket::async_trait]
impl Fairing for AnalyticsFairing {
    fn info(&self) -> Info {
        Info {
            name: "Analytics",
            kind: Kind::Request | Kind::Response,
        }
    }

    async fn on_request(&self, req: &mut Request<'_>, _data: &mut Data<'_>) {
        if req
            .rocket()
            .state::<Analytics>()
            .is_some_and(Analytics::is_enabled)
        {
            req.local_cache(|| AnalyticsStart(Instant::now()));
        }
    }

    async fn on_response<'r>(&self, req: &'r Request<'_>, res: &mut Response<'r>) {
        let analytics = match req.rocket().state::<Analytics>() {
            Some(analytics) if analytics.is_enabled() => analytics,
            _ => return,
        };

        // Attribute only authenticated requests; the client info is cached during auth.
        let info = match &req.local_cache(|| CachedAuthClient(None)).0 {
            Some(info) => info,
            None => return,
        };

        let start = req.local_cache(|| AnalyticsStart(Instant::now())).0;
        let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
        let method = req.method().as_str();
        let path = req.uri().path().as_str();
        let status_code = res.status().code as i32;

        request_span_for(req).in_scope(|| {
            analytics.capture(|| api_request_event(info, method, path, status_code, latency_ms));
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::analytics::{Analytics, RecordingSink};
    use crate::test_helpers::{basic_auth_header, seed_api_key, TestClientBuilder};
    use rocket::http::{Header, Status};
    use std::sync::Arc;

    #[rocket::async_test]
    async fn test_authenticated_request_emits_api_request_event() {
        let recording = RecordingSink::new();
        let client = TestClientBuilder::new()
            .analytics(Analytics::new(Arc::new(recording.clone())))
            .build()
            .await;
        let (key_id, secret) = seed_api_key(&client).await;
        let header = basic_auth_header(&key_id, &secret);

        let response = client
            .get("/v1/tokens")
            .header(Header::new("Authorization", header))
            .dispatch()
            .await;
        assert_ne!(response.status(), Status::Unauthorized);

        let events = recording.events();
        let event = events
            .iter()
            .find(|e| e.event == "api_request")
            .expect("api_request event captured");
        assert_eq!(event.distinct_id, format!("client:{key_id}"));
        assert_eq!(event.properties["method"], serde_json::json!("GET"));
        assert_eq!(
            event.properties["endpoint"],
            serde_json::json!("/v1/tokens")
        );
        assert_eq!(
            event.properties["api_client_label"],
            serde_json::json!("test-key")
        );
        assert_eq!(
            event.properties["api_client_owner"],
            serde_json::json!("test-owner")
        );
    }

    #[rocket::async_test]
    async fn test_unauthenticated_request_emits_no_event() {
        let recording = RecordingSink::new();
        let client = TestClientBuilder::new()
            .analytics(Analytics::new(Arc::new(recording.clone())))
            .build()
            .await;

        client.get("/health").dispatch().await;

        assert!(recording.events().is_empty());
    }
}
