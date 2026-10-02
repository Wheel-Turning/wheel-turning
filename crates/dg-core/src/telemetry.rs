//! Structured logging setup shared by every binary.
//!
//! Logs go to stdout through `tracing`. Outside dev they are one JSON object per line, which is
//! what CloudWatch Logs ingests. The filter comes from `DG_LOG`.

use crate::config::Env;
use tracing_subscriber::EnvFilter;

/// Why logging could not be initialised.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// `DG_LOG` is not a valid filter.
    #[error("DG_LOG is not a valid log filter: {0}")]
    Filter(String),
    /// A global subscriber was already installed.
    #[error("logging was already initialised")]
    AlreadyInitialised,
}

/// Installs the global log subscriber: JSON outside dev, compact text in dev.
pub fn init_logging(env: Env, filter: &str) -> Result<(), TelemetryError> {
    let filter = EnvFilter::try_new(filter).map_err(|e| TelemetryError::Filter(e.to_string()))?;
    // Color codes only help a person at a terminal; piped output stays plain text.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(ansi)
        .with_writer(std::io::stdout);
    let result = if env.is_dev() {
        builder.compact().try_init()
    } else {
        builder
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .try_init()
    };
    result.map_err(|_| TelemetryError::AlreadyInitialised)
}
