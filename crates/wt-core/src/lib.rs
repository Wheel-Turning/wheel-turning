//! Domain types, ids, errors, typestate and configuration shared by every crate.

pub mod answer;
pub mod config;
pub mod gate;
pub mod ids;
pub mod objects;
pub mod records;
pub mod scratch;
pub mod stores;
pub mod telemetry;
pub mod transcript;

pub use config::{Config, ConfigError, Env, Secret};
