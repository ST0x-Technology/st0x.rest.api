//! Observability setup for st0x-rest-api.
//!
//! Always installs console + rolling-file JSON logging. When a `[telemetry]`
//! section is present in config, it *additionally* exports logs and traces via
//! OTLP/HTTP to the self-hosted VictoriaLogs/VictoriaTraces stack (reached over
//! the tailnet). The OTLP path is **fail-open**: if the exporters cannot be
//! built the service still runs with console + file logging.
//!
//! ## Blocking HTTP client requirement
//!
//! OTLP batch processors run on their own background threads, outside the tokio
//! runtime, and require `reqwest::blocking`. An async client panics with "no
//! reactor running". The client is built on a dedicated `std::thread` so the
//! runtime is never touched during initialization (this function runs inside
//! `#[rocket::main]`).

use std::sync::Mutex;
use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::logs::{
    BatchConfigBuilder as LogBatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider,
};
use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{InitError, RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

use crate::config::TelemetryConfig;

static TELEMETRY_INITIALIZED: Mutex<bool> = Mutex::new(false);
const LOG_FILE_PREFIX: &str = "st0x-rest-api.log";
const MAX_LOG_FILES: usize = 14;

fn build_file_appender(log_dir: &str) -> Result<RollingFileAppender, InitError> {
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(log_dir)
}

const DEFAULT_ENV_FILTER: &str = "st0x_rest_api=info,rocket=warn,warn";

/// Instrumentation library name recorded on every span (distinct from the
/// service name; identifies the app's own tracer among any auto-instrumented
/// libraries).
const TRACER_NAME: &str = "st0x-rest-api-tracer";

/// Held for the lifetime of the process. Dropping it flushes the file appender
/// and force-flushes + shuts down the OTLP providers so buffered logs/spans are
/// exported at exit.
pub struct TelemetryGuard {
    _file_guard: WorkerGuard,
    providers: Option<OtelProviders>,
}

struct OtelProviders {
    tracer_provider: SdkTracerProvider,
    logger_provider: SdkLoggerProvider,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(providers) = self.providers.as_ref() {
            if let Err(error) = providers.tracer_provider.force_flush() {
                eprintln!("failed to flush telemetry spans: {error:?}");
            }
            if let Err(error) = providers.tracer_provider.shutdown() {
                eprintln!("failed to shut down tracer provider: {error:?}");
            }
            if let Err(error) = providers.logger_provider.force_flush() {
                eprintln!("failed to flush log records: {error:?}");
            }
            if let Err(error) = providers.logger_provider.shutdown() {
                eprintln!("failed to shut down logger provider: {error:?}");
            }
        }
    }
}

fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|e| {
        eprintln!("invalid RUST_LOG filter, using default: {e}");
        EnvFilter::new(DEFAULT_ENV_FILTER)
    })
}

/// Build the OTel [`Resource`] carrying `service.name` and
/// `deployment.environment` so signals from different environments are
/// distinguishable in VictoriaTraces/VictoriaLogs.
fn build_resource(service_name: &str, environment: &str) -> Resource {
    Resource::builder()
        .with_service_name(service_name.to_string())
        .with_attributes(vec![KeyValue::new(
            "deployment.environment",
            environment.to_string(),
        )])
        .build()
}

/// Build both OTLP providers. Returns an error (never panics) so callers can
/// fail open to console + file logging.
fn build_otel_providers(cfg: &TelemetryConfig) -> Result<OtelProviders, String> {
    // Build the blocking client off the tokio runtime — see module docs.
    let http_client =
        std::thread::spawn(|| otlp_reqwest::blocking::Client::builder().gzip(true).build())
            .join()
            .map_err(|_| "HTTP client builder thread panicked".to_string())?
            .map_err(|e| format!("failed to build OTLP HTTP client: {e}"))?;

    let resource = build_resource(&cfg.service_name, &cfg.environment);

    let traces_url = cfg
        .traces_endpoint
        .join("insert/opentelemetry/v1/traces")
        .map_err(|e| format!("invalid traces_endpoint: {e}"))?;
    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(http_client.clone())
        .with_endpoint(traces_url.as_str())
        .with_protocol(Protocol::HttpBinary)
        .build()
        .map_err(|e| format!("failed to build OTLP span exporter: {e}"))?;
    let tracer_provider = SdkTracerProvider::builder()
        .with_span_processor(
            BatchSpanProcessor::builder(span_exporter)
                .with_batch_config(
                    BatchConfigBuilder::default()
                        .with_max_export_batch_size(512)
                        .with_max_queue_size(2048)
                        .with_scheduled_delay(Duration::from_secs(3))
                        .build(),
                )
                .build(),
        )
        .with_resource(resource.clone())
        .build();

    let logs_url = cfg
        .logs_endpoint
        .join("insert/opentelemetry/v1/logs")
        .map_err(|e| format!("invalid logs_endpoint: {e}"))?;
    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_http_client(http_client)
        .with_endpoint(logs_url.as_str())
        .with_protocol(Protocol::HttpBinary)
        .build()
        .map_err(|e| format!("failed to build OTLP log exporter: {e}"))?;
    let logger_provider = SdkLoggerProvider::builder()
        .with_log_processor(
            BatchLogProcessor::builder(log_exporter)
                .with_batch_config(
                    LogBatchConfigBuilder::default()
                        .with_max_export_batch_size(512)
                        .with_max_queue_size(2048)
                        .with_scheduled_delay(Duration::from_secs(3))
                        .build(),
                )
                .build(),
        )
        .with_resource(resource)
        .build();

    Ok(OtelProviders {
        tracer_provider,
        logger_provider,
    })
}

/// Initialize logging (and, when configured, OTLP export). Returns a guard that
/// must be held for the process lifetime.
pub fn init(log_dir: &str, telemetry: Option<&TelemetryConfig>) -> Result<TelemetryGuard, String> {
    let mut initialized = TELEMETRY_INITIALIZED
        .lock()
        .map_err(|_| "telemetry initialization lock poisoned".to_string())?;
    if *initialized {
        return Err("telemetry::init() called more than once".to_string());
    }

    let file_appender = build_file_appender(log_dir)
        .map_err(|err| format!("failed to initialize rolling file appender: {err}"))?;
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

    // Build OTLP providers up front (fail-open): a build error degrades to
    // console + file logging rather than aborting the service.
    let providers = match telemetry {
        Some(cfg) => match build_otel_providers(cfg) {
            Ok(providers) => Some(providers),
            Err(error) => {
                eprintln!(
                    "telemetry: OTLP export disabled, continuing with console + file logging: {error}"
                );
                None
            }
        },
        None => None,
    };

    // Span-export + log-bridge layers, present only when OTLP is live.
    let (otel_trace_layer, otel_log_layer) = match providers.as_ref() {
        Some(providers) => {
            let tracer = providers.tracer_provider.tracer(TRACER_NAME);
            let trace_layer = tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(env_filter());
            let log_layer = OpenTelemetryTracingBridge::new(&providers.logger_provider)
                .with_filter(env_filter());
            (Some(trace_layer), Some(log_layer))
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(env_filter())
        .with(fmt::layer().json().with_current_span(false))
        .with(
            fmt::layer()
                .json()
                .with_current_span(false)
                .with_writer(file_writer),
        )
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .try_init()
        .map_err(|err| format!("failed to initialize tracing subscriber: {err}"))?;

    std::panic::set_hook(Box::new(|info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());

        if let Some(loc) = info.location() {
            tracing::error!(
                panic.message = %message,
                panic.file = loc.file(),
                panic.line = loc.line(),
                panic.column = loc.column(),
                "panic occurred"
            );
        } else {
            tracing::error!(panic.message = %message, "panic occurred");
        }
    }));

    *initialized = true;
    Ok(TelemetryGuard {
        _file_guard: file_guard,
        providers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};

    #[test]
    fn file_appender_retains_only_fourteen_daily_logs() {
        let dir = tempfile::tempdir().expect("temporary log directory");
        for day in 1..=15 {
            File::create(
                dir.path()
                    .join(format!("{LOG_FILE_PREFIX}.2000-01-{day:02}")),
            )
            .expect("seed daily log");
        }
        let unrelated = dir.path().join("unrelated.log");
        File::create(&unrelated).expect("seed unrelated file");

        let appender =
            build_file_appender(&dir.path().to_string_lossy()).expect("build file appender");
        drop(appender);

        let retained = fs::read_dir(dir.path())
            .expect("read log directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(LOG_FILE_PREFIX)
            })
            .count();

        assert_eq!(retained, MAX_LOG_FILES);
        assert!(unrelated.exists());
    }

    #[test]
    fn init_returns_file_appender_errors() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let invalid_log_dir = dir.path().join("not-a-directory");
        File::create(&invalid_log_dir).expect("seed file at log directory path");

        let error = match init(&invalid_log_dir.to_string_lossy(), None) {
            Ok(_) => panic!("telemetry initialization should fail"),
            Err(error) => error,
        };

        assert!(error.starts_with("failed to initialize rolling file appender:"));
    }
}
