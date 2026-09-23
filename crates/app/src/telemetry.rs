use std::io::IsTerminal;

use anyhow::anyhow;
use tracing_subscriber::EnvFilter;
use uwuu_core::config::{LogFormat, TelemetryConfig};

/// Installs the global `tracing` subscriber. `RUST_LOG` overrides the
/// configured filter.
pub fn init(config: &TelemetryConfig) -> anyhow::Result<()> {
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::try_new(&config.log_filter)
            .map_err(|e| anyhow!("invalid telemetry.log_filter: {e}"))?,
    };
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal());
    match config.log_format {
        LogFormat::Text => builder.try_init(),
        LogFormat::Json => builder.json().flatten_event(true).try_init(),
    }
    .map_err(|e| anyhow!(e))
}
