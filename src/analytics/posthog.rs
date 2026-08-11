//! PostHog HTTP sink. Posts each event to the `/i/v0/e/` endpoint on a spawned
//! task, bounded by a semaphore so a backlog can never grow without limit. All
//! failures are logged at `warn` and never surface to the request.

use super::{AnalyticsEvent, AnalyticsSink};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::Instrument;

/// Maximum number of in-flight capture requests. Excess events are dropped
/// (with a warning) rather than queued unbounded.
const MAX_INFLIGHT: usize = 64;

/// Per-request timeout for a capture call.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);

/// Non-blocking PostHog event sink with bounded request concurrency.
pub(crate) struct PostHogSink {
    client: reqwest::Client,
    project_token: String,
    capture_url: String,
    semaphore: Arc<Semaphore>,
}

impl PostHogSink {
    /// Build a sink targeting `host` with the supplied PostHog project token.
    pub(crate) fn new(project_token: String, host: String) -> Result<Self, String> {
        let host = host.trim_end_matches('/');
        let capture_url = format!("{host}/i/v0/e/");
        let client = reqwest::Client::builder()
            .timeout(CAPTURE_TIMEOUT)
            .build()
            .map_err(|e| format!("failed to build analytics http client: {e}"))?;
        Ok(Self {
            client,
            project_token,
            capture_url,
            semaphore: Arc::new(Semaphore::new(MAX_INFLIGHT)),
        })
    }
}

impl AnalyticsSink for PostHogSink {
    fn capture(&self, event: AnalyticsEvent) {
        let name = event.event;
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                tracing::warn!(event = name, "dropping analytics event: sink saturated");
                return;
            }
        };

        let payload = serde_json::json!({
            "api_key": self.project_token,
            "event": name,
            "distinct_id": event.distinct_id,
            "properties": event.properties,
        });
        let client = self.client.clone();
        let url = self.capture_url.clone();

        tokio::spawn(
            async move {
                let _permit = permit;
                match client.post(&url).json(&payload).send().await {
                    Ok(resp) if resp.status().is_success() => {}
                    Ok(resp) => {
                        tracing::warn!(status = %resp.status(), event = name, "posthog capture returned non-success");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, event = name, "posthog capture request failed");
                    }
                }
            }
            .instrument(tracing::Span::current()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tracing_test::traced_test;

    fn documented_event() -> AnalyticsEvent {
        let mut properties = serde_json::Map::new();
        properties.insert("api_client_owner".to_string(), Value::from("st0x"));
        AnalyticsEvent {
            event: "swap_quoted",
            distinct_id: "client:site".to_string(),
            properties,
        }
    }

    #[tokio::test]
    async fn capture_posts_documented_event_payload() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock PostHog server");
        let address = listener.local_addr().expect("mock PostHog address");
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept capture request");
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];

            loop {
                let count = socket
                    .read(&mut buffer)
                    .await
                    .expect("read capture request");
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);

                let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                });
                if content_length.is_some_and(|length| request.len() >= header_end + 4 + length) {
                    break;
                }
            }

            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .expect("write capture response");
            let _ = request_tx.send(request);
        });

        let sink = PostHogSink::new("project-token".to_string(), format!("http://{address}"))
            .expect("PostHog sink");
        sink.capture(documented_event());

        let request = tokio::time::timeout(Duration::from_secs(1), request_rx)
            .await
            .expect("capture request timeout")
            .expect("capture request received");
        let request = String::from_utf8(request).expect("UTF-8 capture request");
        assert!(request.starts_with("POST /i/v0/e/ HTTP/1.1\r\n"));

        let (_, body) = request
            .split_once("\r\n\r\n")
            .expect("capture request body");
        let payload: Value = serde_json::from_str(body).expect("JSON capture payload");
        assert_eq!(payload["api_key"], "project-token");
        assert_eq!(payload["event"], "swap_quoted");
        assert_eq!(payload["distinct_id"], "client:site");
        assert_eq!(payload["properties"]["api_client_owner"], "st0x");
    }

    #[tokio::test]
    #[traced_test]
    async fn capture_drops_event_when_sink_is_saturated() {
        let sink = PostHogSink::new(
            "project-token".to_string(),
            "http://127.0.0.1:1".to_string(),
        )
        .expect("PostHog sink");
        let _permits = sink
            .semaphore
            .clone()
            .acquire_many_owned(MAX_INFLIGHT as u32)
            .await
            .expect("reserve all capture permits");

        sink.capture(documented_event());

        assert!(logs_contain("dropping analytics event: sink saturated"));
    }

    #[tokio::test]
    #[traced_test]
    async fn capture_warns_on_non_success_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock PostHog server");
        let address = listener.local_addr().expect("mock PostHog address");

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept capture request");
            socket
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write capture response");
        });

        let sink = PostHogSink::new("project-token".to_string(), format!("http://{address}"))
            .expect("PostHog sink");
        sink.capture(documented_event());

        let _permits = tokio::time::timeout(
            Duration::from_secs(1),
            sink.semaphore
                .clone()
                .acquire_many_owned(MAX_INFLIGHT as u32),
        )
        .await
        .expect("capture task timeout")
        .expect("reserve all capture permits");
        assert!(logs_contain("posthog capture returned non-success"));
    }

    #[tokio::test]
    #[traced_test]
    async fn capture_warns_on_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock PostHog server");
        let address = listener.local_addr().expect("mock PostHog address");
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.expect("accept capture request");
            let _ = accepted_tx.send(());
            tokio::time::sleep(CAPTURE_TIMEOUT + Duration::from_secs(1)).await;
        });

        let sink = PostHogSink::new("project-token".to_string(), format!("http://{address}"))
            .expect("PostHog sink");
        sink.capture(documented_event());
        tokio::time::timeout(Duration::from_secs(1), accepted_rx)
            .await
            .expect("capture request timeout")
            .expect("capture request received");

        let _permits = tokio::time::timeout(
            CAPTURE_TIMEOUT + Duration::from_secs(1),
            sink.semaphore
                .clone()
                .acquire_many_owned(MAX_INFLIGHT as u32),
        )
        .await
        .expect("capture task timeout")
        .expect("reserve all capture permits");
        assert!(logs_contain("posthog capture request failed"));
    }
}
