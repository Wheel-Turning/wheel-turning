//! Domain types, ids, errors, typestate and configuration shared by every crate.

pub mod config;
pub mod ids;
pub mod telemetry;

pub use config::{Config, ConfigError, Env, Secret};
