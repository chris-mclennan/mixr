use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub struct AppLog;

impl AppLog {
    pub fn init() -> Result<WorkerGuard> {
        let log_dir = dirs::home_dir().unwrap_or_default().join(".mixr");
        std::fs::create_dir_all(&log_dir)?;

        let file_appender = tracing_appender::rolling::never(&log_dir, "mixr.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        tracing_subscriber::registry()
            .with(EnvFilter::new("info"))
            .with(
                fmt::layer()
                    .with_writer(non_blocking)
                    .with_ansi(false)
                    .with_target(false),
            )
            .init();

        tracing::info!("mixr-rs started");
        Ok(guard)
    }
}
