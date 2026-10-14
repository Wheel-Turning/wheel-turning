//! Domain types, ids, errors, typestate and configuration shared by every crate.

// Private items carry docs too, as the public ones do through `missing_docs`.
#![warn(clippy::missing_docs_in_private_items)]

pub mod answer;
pub mod config;
pub mod excerpt;
pub mod gate;
pub mod ids;
pub mod licenses;
pub mod objects;
pub mod records;
pub mod scratch;
pub mod stores;
pub mod telemetry;
pub mod transcript;

pub use config::{Config, ConfigError, Env, Secret};
