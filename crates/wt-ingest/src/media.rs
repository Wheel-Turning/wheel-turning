//! Media fetch for cleared talks (Spec 03, Seattle Soto Zen sources item 2; Spec 04, Media
//! handling). It takes a `Talk<Consented>`, so a talk that has not passed the gate cannot be
//! fetched, and it writes only into the talk's scratch directory.
//!
//! The fetch is a function here, not a method of each `SourceAdapter`, because a talk's media
//! can come from another source than the talk itself: an SSZ Vimeo talk's audio comes from the
//! podcast feed (decision of October 5, 2026).

use std::io::Write;
use std::path::{Path, PathBuf};

use wt_core::gate::{Consented, Talk};
use wt_core::scratch::ScratchDir;
use wt_db::feed::MediaRoute;

use crate::http::{self, Exclusions, parse_url};
use crate::source::IngestError;

/// Where a talk's media comes from, in the order Spec 03 tries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    /// A matched podcast episode's audio enclosure.
    Enclosure {
        /// Its address.
        url: String,
    },
    /// A third-party download tool. Off unless `WT_ALLOW_THIRD_PARTY_FETCH` is set, and even
    /// then not built until counsel and SSZ agree (Spec 03, item 2d).
    ThirdParty {
        /// The page it would read.
        url: String,
    },
}

impl MediaSource {
    /// The source for a stored route, if there is one.
    pub fn from_route(route: &MediaRoute) -> Option<Self> {
        match route {
            MediaRoute::FeedEnclosure { url, .. } => {
                Some(MediaSource::Enclosure { url: url.clone() })
            }
            MediaRoute::None => None,
        }
    }
}

/// What a fetch needs besides the talk.
#[derive(Debug, Clone)]
pub struct MediaFetcher {
    client: reqwest::Client,
    exclusions: Exclusions,
    allow_third_party: bool,
}

impl MediaFetcher {
    /// A fetcher that never requests an excluded address. `allow_third_party` is
    /// `WT_ALLOW_THIRD_PARTY_FETCH`.
    pub fn new(exclusions: Exclusions, allow_third_party: bool) -> Result<Self, IngestError> {
        Ok(Self {
            client: http::client_with_timeout(std::time::Duration::from_secs(60 * 60))?,
            exclusions,
            allow_third_party,
        })
    }

    /// Fetches the talk's media into `scratch` and returns the file. Refuses a third-party
    /// source before anything starts while fetching from third parties is off.
    pub async fn fetch(
        &self,
        talk: &Talk<Consented>,
        source: &MediaSource,
        scratch: &ScratchDir,
    ) -> Result<PathBuf, IngestError> {
        match source {
            MediaSource::ThirdParty { .. } if !self.allow_third_party => {
                Err(IngestError::ThirdPartyFetchDisabled)
            }
            MediaSource::ThirdParty { .. } => Err(IngestError::MediaNotAvailable),
            MediaSource::Enclosure { url } => self.download(talk, url, scratch).await,
        }
    }

    async fn download(
        &self,
        talk: &Talk<Consented>,
        url: &str,
        scratch: &ScratchDir,
    ) -> Result<PathBuf, IngestError> {
        let target = self.exclusions.clear(&parse_url(url)?)?;
        http::check_robots(&self.client, &target).await?;
        let mut response = http::get_ok(&self.client, &target).await?;
        if let Some(length) = response.content_length() {
            check_space(scratch.path(), length)?;
        }
        let path = scratch
            .file("source.media")
            .map_err(|e| scratch_error(url, e))?;
        let mut file = scratch
            .create_file("source.media")
            .map_err(|e| scratch_error(url, e))?;
        while let Some(chunk) = response.chunk().await.map_err(|e| IngestError::Http {
            url: url.to_owned(),
            message: e.without_url().to_string(),
        })? {
            file.write_all(&chunk).map_err(|e| IngestError::Http {
                url: url.to_owned(),
                message: format!("writing to scratch: {e}"),
            })?;
        }
        file.flush().map_err(|e| IngestError::Http {
            url: url.to_owned(),
            message: format!("writing to scratch: {e}"),
        })?;
        tracing::info!(talk = %talk.id, bytes = path.metadata().map(|m| m.len()).unwrap_or(0),
                       "media fetched into scratch");
        Ok(path)
    }
}

fn scratch_error(url: &str, e: wt_core::scratch::ScratchError) -> IngestError {
    IngestError::Http {
        url: url.to_owned(),
        message: e.to_string(),
    }
}

/// Refuses a download unless free space is at least twice its size (Spec 04, Media handling,
/// rule 3): the source file and its WAV exist together for a moment.
fn check_space(dir: &Path, length: u64) -> Result<(), IngestError> {
    let free = fs4::available_space(dir).map_err(|e| IngestError::Http {
        url: dir.display().to_string(),
        message: format!("reading free disk space: {e}"),
    })?;
    if free < length.saturating_mul(2) {
        return Err(IngestError::NoDiskSpace {
            needed: length.saturating_mul(2),
            free,
        });
    }
    Ok(())
}
