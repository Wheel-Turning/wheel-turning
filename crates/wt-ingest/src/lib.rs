//! The `SourceAdapter` trait, the Vimeo adapter, deduplication and talk classification
//! (Spec 03). Discovery records metadata only; nothing here fetches media.

pub mod audio;
pub mod classify;
pub mod completeness;
pub mod config;
pub mod dates;
pub mod feedmatch;
pub mod http;
pub mod media;
pub mod owner_export;
pub mod pipeline;
pub mod podcast;
pub mod report;
pub mod review;
pub mod robots;
pub mod rss;
pub mod source;
pub mod textjobs;
pub mod texts;
pub mod vimeo;

pub use source::{DiscoveredVideo, IngestError, SourceAdapter, SourcePlatform};
