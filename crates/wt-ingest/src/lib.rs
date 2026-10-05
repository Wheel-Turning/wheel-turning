//! The `SourceAdapter` trait, the Vimeo adapter, deduplication and talk classification
//! (Spec 03). Discovery records metadata only; nothing here fetches media.

pub mod classify;
pub mod config;
pub mod pipeline;
pub mod robots;
pub mod source;
pub mod vimeo;

pub use source::{DiscoveredVideo, IngestError, SourceAdapter, SourcePlatform};
