//! Prometheus metrics for st0x-rest-api.
//!
//! Installs the process-global recorder plus a standalone HTTP listener serving
//! `GET /metrics` on `:8001`. The port binds all interfaces but is only
//! reachable over the tailnet: the host firewall keeps it closed to the public
//! internet (only 22/80/443 are public; `tailscale0` is a trusted interface),
//! and the observability box scrapes it over the tailnet.
//!
//! This lives on a **separate** listener, not the public Rocket app (which is
//! fronted by nginx at `api.st0x.io`) — so `/metrics` is never exposed publicly.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder};

/// Port the `/metrics` listener binds. Must match the devops scrape target
/// (`st0x-rest-api-nixos:8001` / `st0x-rest-api-staging:8001`).
const METRICS_PORT: u16 = 8001;

/// Latency histogram buckets (seconds). Rendering the duration metric as a real
/// Prometheus histogram (`_bucket` series) rather than the exporter's default
/// summary lets Grafana compute aggregatable `histogram_quantile` percentiles.
const LATENCY_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Install the global recorder and start the `/metrics` listener. Call once at
/// startup, from within the tokio runtime (the listener is spawned on it).
pub fn install() -> Result<(), BuildError> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), METRICS_PORT);
    PrometheusBuilder::new()
        .with_http_listener(addr)
        .set_buckets_for_metric(
            Matcher::Full("rest_api_http_request_duration_seconds".to_string()),
            LATENCY_BUCKETS,
        )?
        .install()?;
    describe();
    Ok(())
}

fn describe() {
    metrics::describe_counter!(
        "rest_api_http_requests_total",
        "HTTP requests handled, by method, endpoint, and status"
    );
    metrics::describe_histogram!(
        "rest_api_http_request_duration_seconds",
        metrics::Unit::Seconds,
        "HTTP request handling latency in seconds, by method and endpoint"
    );
}

/// Record one completed HTTP request. Called from the response fairing.
///
/// `endpoint` is the matched route pattern (e.g. `/v1/trades/tx/<tx>`), not the
/// raw URI, to keep label cardinality bounded.
pub(crate) fn record_request(method: &str, endpoint: &str, status: u16, duration_secs: f64) {
    metrics::counter!(
        "rest_api_http_requests_total",
        "method" => method.to_string(),
        "endpoint" => endpoint.to_string(),
        "status" => status.to_string(),
    )
    .increment(1);
    metrics::histogram!(
        "rest_api_http_request_duration_seconds",
        "method" => method.to_string(),
        "endpoint" => endpoint.to_string(),
    )
    .record(duration_secs);
}
