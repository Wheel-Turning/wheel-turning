//! Structured logging setup shared by every binary.
//!
//! Logs go through `tracing`, to stdout for services and to stderr for the `wt` command line,
//! whose stdout carries its reports. Outside dev they are one JSON object per line, which is
//! what CloudWatch Logs ingests from either stream. The filter comes from `WT_LOG`.

use crate::config::Env;
use tracing_subscriber::EnvFilter;

/// Why logging could not be initialised.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// `WT_LOG` is not a valid filter.
    #[error("WT_LOG is not a valid log filter: {0}")]
    Filter(String),
    /// A global subscriber was already installed.
    #[error("logging was already initialised")]
    AlreadyInitialised,
}

/// Where log lines go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogOutput {
    /// Services, whose stdout is only logs.
    Stdout,
    /// The command line, whose stdout is a report another program may parse.
    Stderr,
}

/// Installs the global log subscriber: JSON outside dev, compact text in dev.
pub fn init_logging(env: Env, filter: &str, output: LogOutput) -> Result<(), TelemetryError> {
    let filter = EnvFilter::try_new(filter).map_err(|e| TelemetryError::Filter(e.to_string()))?;
    // Color codes only help a person at a terminal; piped output stays plain text.
    let ansi = match output {
        LogOutput::Stdout => std::io::IsTerminal::is_terminal(&std::io::stdout()),
        LogOutput::Stderr => std::io::IsTerminal::is_terminal(&std::io::stderr()),
    };
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(ansi)
        .with_writer(move || -> Box<dyn std::io::Write> {
            match output {
                LogOutput::Stdout => Box::new(std::io::stdout()),
                LogOutput::Stderr => Box::new(std::io::stderr()),
            }
        });
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
