use std::sync::Once;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

static TELEMETRY_INIT: Once = Once::new();

pub fn init() -> Result<WorkerGuard, String> {
    let mut guard_slot: Option<WorkerGuard> = None;

    TELEMETRY_INIT.call_once(|| {
        let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|e| {
            eprintln!("invalid RUST_LOG filter, using default: {e}");
            EnvFilter::new("st0x_rest_api=info,rocket=warn,warn")
        });

        let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "./logs".to_string());
        let file_appender = tracing_appender::rolling::daily(&log_dir, "st0x-rest-api.log");
        let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

        let init_result = tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().json().with_current_span(false))
            .with(
                fmt::layer()
                    .json()
                    .with_current_span(false)
                    .with_writer(file_writer),
            )
            .try_init();

        if let Err(err) = init_result {
            eprintln!("failed to initialize tracing subscriber: {err}");
            std::process::exit(1);
        }

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

        guard_slot = Some(file_guard);
    });

    guard_slot.ok_or_else(|| "telemetry::init() called more than once".to_string())
}
