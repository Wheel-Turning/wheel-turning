//! The `SourceAdapter` trait and what discovery returns (Spec 03, Source adapters).
//!
//! Discovery returns metadata only. The fetch methods, which take a `Talk<Consented>`, arrive
//! with the transcript pipeline in M4, together with the scratch directory they write into.

use std::fmt;

use async_trait::async_trait;
use serde::Deserialize;
use sqlx::types::chrono::{DateTime, Utc};

/// A source platform, matching the `source_platform` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourcePlatform {
    /// YouTube Data API.
    Youtube,
    /// Files in a bucket or directory.
    Storage,
    /// A podcast feed.
    Rss,
    /// Vimeo, SSZ's canonical list.
    Vimeo,
}

impl SourcePlatform {
    /// The value stored in `source_video.platform`.
    pub fn as_str(self) -> &'static str {
        match self {
            SourcePlatform::Youtube => "youtube",
            SourcePlatform::Storage => "storage",
            SourcePlatform::Rss => "rss",
            SourcePlatform::Vimeo => "vimeo",
        }
    }
}

impl fmt::Display for SourcePlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One video as a source lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredVideo {
    /// The platform's id for the video.
    pub external_id: String,
    /// The canonical page address.
    pub url: String,
    /// The title as published.
    pub title: String,
    /// The description, if any.
    pub description: Option<String>,
    /// When it was published or uploaded.
    pub published_at: Option<DateTime<Utc>>,
    /// Length in seconds.
    pub duration_s: Option<i32>,
    /// The account or channel it came from.
    pub channel_ref: Option<String>,
    /// Whether the source holds a captions or text track, which rule 3 prefers to keep.
    pub has_captions: bool,
    /// The platform's own record, stored in `source_video.raw`.
    pub raw: serde_json::Value,
}

/// Errors from a source. None carries a token or credential.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// The request could not be sent or its body read.
    #[error("request to {url} failed: {message}")]
    Http {
        /// The address, never with a token in it.
        url: String,
        /// What went wrong.
        message: String,
    },
    /// The server answered with an error status.
    #[error("{url} answered {status}")]
    Status {
        /// The address.
        url: String,
        /// The HTTP status.
        status: u16,
    },
    /// An active exclusion covers the address, so it was not requested (Spec 08).
    #[error("{url} is excluded by exclusion {exclusion} ({reason}), so it was not requested")]
    Excluded {
        /// The address.
        url: String,
        /// The exclusion's id.
        exclusion: String,
        /// Why it exists.
        reason: String,
    },
    /// `robots.txt` disallows the path, so it was not requested.
    #[error("robots.txt disallows {0}, so it was not requested")]
    RobotsDisallowed(String),
    /// A response did not have the expected shape.
    #[error("unexpected response from {url}: {message}")]
    Parse {
        /// The address.
        url: String,
        /// What was wrong.
        message: String,
    },
    /// The source's configuration lacks something this adapter needs.
    #[error("source {source_name} needs {what}")]
    Misconfigured {
        /// The source.
        source_name: String,
        /// The missing setting.
        what: &'static str,
    },
    /// Third-party fetching is off (`WT_ALLOW_THIRD_PARTY_FETCH`).
    #[error("third-party fetching is disabled")]
    ThirdPartyFetchDisabled,
    /// The source offers no media download for this talk.
    #[error("the source offers no media for this talk")]
    MediaNotAvailable,
    /// Free disk space is under twice the download's size.
    #[error("not enough free disk: {needed} bytes needed, {free} free")]
    NoDiskSpace {
        /// Bytes needed.
        needed: u64,
        /// Bytes free.
        free: u64,
    },
}

/// A source of talks. Every adapter implements it.
#[async_trait]
pub trait SourceAdapter: Send + Sync {
    /// The platform.
    fn platform(&self) -> SourcePlatform;

    /// Lists the source's videos, metadata only. `since` limits the list where the source
    /// can filter by date.
    async fn discover(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<DiscoveredVideo>, IngestError>;
}
