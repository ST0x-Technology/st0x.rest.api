use std::sync::Mutex;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{InitError, RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

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

pub fn init(log_dir: &str) -> Result<WorkerGuard, String> {
    let mut initialized = TELEMETRY_INITIALIZED
        .lock()
        .map_err(|_| "telemetry initialization lock poisoned".to_string())?;
    if *initialized {
        return Err("telemetry::init() called more than once".to_string());
    }

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|e| {
        eprintln!("invalid RUST_LOG filter, using default: {e}");
        EnvFilter::new("st0x_rest_api=info,rocket=warn,warn")
    });
    let file_appender = build_file_appender(log_dir)
        .map_err(|err| format!("failed to initialize rolling file appender: {err}"))?;
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().json().with_current_span(false))
        .with(
            fmt::layer()
                .json()
                .with_current_span(false)
                .with_writer(file_writer),
        )
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
    Ok(file_guard)
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

        let error = match init(&invalid_log_dir.to_string_lossy()) {
            Ok(_) => panic!("telemetry initialization should fail"),
            Err(error) => error,
        };

        assert!(error.starts_with("failed to initialize rolling file appender:"));
    }
}
