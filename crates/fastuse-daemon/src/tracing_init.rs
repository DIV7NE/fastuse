//! tracing_subscriber init: JSON to stderr + rolling file in
//! `%LOCALAPPDATA%\fastuse\logs\` (D-09).

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Registry};

use fastuse_core::local_app_data;

/// Initialize tracing. Returns a guard that must be kept alive for the
/// lifetime of the process so that the non-blocking file appender flushes.
pub fn init() -> std::io::Result<WorkerGuard> {
    let log_dir = local_app_data()?.join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = rolling::Builder::new()
        .filename_prefix("daemon")
        .filename_suffix("log")
        .max_log_files(5)
        .rotation(rolling::Rotation::DAILY)
        .build(&log_dir)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("rolling: {e}")))?;
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let stderr_layer = fmt::layer()
        .json()
        .with_writer(std::io::stderr);
    let file_layer = fmt::layer()
        .json()
        .with_writer(file_writer);

    Registry::default()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    Ok(guard)
}
