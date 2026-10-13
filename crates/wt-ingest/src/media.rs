//! Media fetch for cleared talks (Spec 03, Seattle Soto Zen sources item 2; Spec 04, Media
//! handling). It takes a `Talk<Consented>`, so a talk that has not passed the gate cannot be
//! fetched, and it writes only into the talk's scratch directory.
//!
//! The fetch is a function here, not a method of each `SourceAdapter`, because a talk's media
//! can come from another source than the talk itself: an SSZ Vimeo talk's audio comes from the
//! podcast feed (decision of October 5, 2026).
//!
//! Route (b), a Vimeo talk's own file, is read through the Vimeo API with the token of the
//! talk's source (decision of October 13, 2026). The file links the API returns are signed and
//! work without the token, so neither a link nor the token is ever logged, stored or put in an
//! error: errors name the video id.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::Url;
use wt_core::Secret;
use wt_core::gate::{Consented, Talk};
use wt_core::scratch::ScratchDir;
use wt_db::feed::MediaRoute;

use crate::http::{self, Exclusions, parse_url};
use crate::source::IngestError;

/// What the Vimeo API is asked for about a video: its file links. `download` holds the links an
/// owner allows; `files` and `play` the files the token's owner may read on a Standard or
/// higher plan.
const FILE_FIELDS: &str = "download,files,play";

/// The `Accept` header the Vimeo API wants.
const VIMEO_ACCEPT: &str = "application/vnd.vimeo.*+json;version=3.4";

/// The file a Vimeo text track is saved to in scratch, as an accuracy reference only.
pub const CAPTION_REFERENCE: &str = "vimeo-captions.vtt";

/// Where a talk's media comes from, in the order Spec 03 tries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    /// A matched podcast episode's audio enclosure.
    Enclosure {
        /// Its address.
        url: String,
    },
    /// A Vimeo video's smallest file, read through the API with its source's token (Spec 03,
    /// item 2b).
    Vimeo {
        /// The source whose token reads it.
        source: String,
        /// The Vimeo video id.
        video_id: String,
    },
    /// A third-party download tool. Off unless `WT_ALLOW_THIRD_PARTY_FETCH` is set, and even
    /// then not built until counsel and SSZ agree (Spec 03, item 2d).
    ThirdParty {
        /// The page it would read.
        url: String,
    },
}

impl MediaSource {
    /// The source for a stored route, if there is one. Whether a Vimeo route can be fetched
    /// depends on its source's token: see [`MediaFetcher::source_for`].
    pub fn from_route(route: &MediaRoute) -> Option<Self> {
        match route {
            MediaRoute::FeedEnclosure { url, .. } | MediaRoute::Direct { url } => {
                Some(MediaSource::Enclosure { url: url.clone() })
            }
            MediaRoute::Vimeo { source, video_id } => Some(MediaSource::Vimeo {
                source: source.clone(),
                video_id: video_id.clone(),
            }),
            MediaRoute::None => None,
        }
    }
}

/// The Vimeo API and each Vimeo source's token, for route (b).
#[derive(Debug, Clone)]
pub struct VimeoMedia {
    /// The API base, `https://api.vimeo.com`; tests point it at a local server.
    pub api: Url,
    /// Each Vimeo source's token by source name, read from the variable its `token_env` names.
    pub tokens: BTreeMap<String, Secret>,
}

/// What a fetch needs besides the talk.
#[derive(Debug, Clone)]
pub struct MediaFetcher {
    client: reqwest::Client,
    exclusions: Exclusions,
    allow_third_party: bool,
    vimeo: Option<VimeoMedia>,
}

impl MediaFetcher {
    /// A fetcher that never requests an excluded address. `allow_third_party` is
    /// `WT_ALLOW_THIRD_PARTY_FETCH`.
    pub fn new(exclusions: Exclusions, allow_third_party: bool) -> Result<Self, IngestError> {
        Ok(Self {
            client: http::client_with_timeout(std::time::Duration::from_secs(60 * 60))?,
            exclusions,
            allow_third_party,
            vimeo: None,
        })
    }

    /// Adds route (b): Vimeo talks of a source with a token fetch their file through the API.
    pub fn with_vimeo(mut self, vimeo: VimeoMedia) -> Self {
        self.vimeo = Some(vimeo);
        self
    }

    /// The source for a stored route that this fetcher can try. A Vimeo talk whose source has
    /// no token has none, so the `audio` job holds it as `no_media` without fetching.
    pub fn source_for(&self, route: &MediaRoute) -> Option<MediaSource> {
        MediaSource::from_route(route).filter(|source| match source {
            MediaSource::Vimeo { source, .. } => self.vimeo_token(source).is_some(),
            _ => true,
        })
    }

    fn vimeo_token(&self, source: &str) -> Option<(&Url, &Secret)> {
        let vimeo = self.vimeo.as_ref()?;
        Some((&vimeo.api, vimeo.tokens.get(source)?))
    }

    /// Fetches the talk's media into `scratch` and returns the file. Refuses a third-party
    /// source before anything starts while fetching from third parties is off. A source that
    /// offers no file for the talk is `MediaNotAvailable`.
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
            MediaSource::Enclosure { url } => {
                let target = self.exclusions.clear(&parse_url(url)?)?;
                http::check_robots(&self.client, &target).await?;
                let response = http::get_ok(&self.client, &target).await?;
                self.download(talk, url, response, scratch).await
            }
            MediaSource::Vimeo { source, video_id } => {
                self.fetch_vimeo(talk, source, video_id, scratch).await
            }
        }
    }

    /// Route (b). Reads the video's file links with its source's token, streams the smallest
    /// file into scratch, and saves a text track there as an accuracy reference when the video
    /// has one (Spec 03, items 2b and 5). No file, from a plan below Standard or downloads
    /// off, is `MediaNotAvailable`. No robots check is made: these are the account owner's API
    /// and signed links, not pages a crawler reads (decision of October 13, 2026). The
    /// exclusions still apply to the talk's page, the API address and the file link.
    async fn fetch_vimeo(
        &self,
        talk: &Talk<Consented>,
        source: &str,
        video_id: &str,
        scratch: &ScratchDir,
    ) -> Result<PathBuf, IngestError> {
        let (api, token) = self
            .vimeo_token(source)
            .ok_or(IngestError::MediaNotAvailable)?;
        if video_id.is_empty() || !video_id.chars().all(|c| c.is_ascii_digit()) {
            return Err(IngestError::Parse {
                url: format!("vimeo video {video_id:?}"),
                message: "not a numeric Vimeo id".to_owned(),
            });
        }
        let label = format!("vimeo video {video_id}");
        self.exclusions
            .clear(&parse_url(&format!("https://vimeo.com/{video_id}"))?)?;
        let video: VideoFiles = self
            .api_json(
                api,
                token,
                &format!("/videos/{video_id}?fields={FILE_FIELDS}"),
            )
            .await?
            .ok_or(IngestError::MediaNotAvailable)?;
        let Some(link) = smallest_file(&video) else {
            tracing::info!(talk = %talk.id, vimeo_id = %video_id,
                           "the Vimeo API returned no file for this video");
            return Err(IngestError::MediaNotAvailable);
        };
        let response = self.get_signed(&link, &label).await?;
        let path = self.download(talk, &label, response, scratch).await?;
        self.save_captions(talk, api, token, video_id, scratch)
            .await;
        Ok(path)
    }

    /// GETs a signed link, checked against the exclusions, with `label` in place of the link in
    /// any error.
    async fn get_signed(&self, link: &str, label: &str) -> Result<reqwest::Response, IngestError> {
        let url = Url::parse(link).map_err(|_| IngestError::Parse {
            url: label.to_owned(),
            message: "the link is not an address".to_owned(),
        })?;
        let target = self.exclusions.clear(&url).map_err(|e| redact(e, label))?;
        let response = http::get(&self.client, &target)
            .await
            .map_err(|e| redact(e, label))?;
        http::ok(target.url(), response).map_err(|e| redact(e, label))
    }

    /// Saves the video's first text track that a person wrote, if any, into scratch as a
    /// reference for measuring accuracy. It is never the transcript, auto-generated tracks are
    /// skipped (Spec 04, Transcription), and a failure here only logs: the audio is what the
    /// job needs.
    async fn save_captions(
        &self,
        talk: &Talk<Consented>,
        api: &Url,
        token: &Secret,
        video_id: &str,
        scratch: &ScratchDir,
    ) {
        let label = format!("vimeo video {video_id} text track");
        let result: Result<bool, IngestError> = async {
            let Some(tracks) = self
                .api_json::<TextTracks>(api, token, &format!("/videos/{video_id}/texttracks"))
                .await?
            else {
                return Ok(false);
            };
            let Some(link) = tracks
                .data
                .into_iter()
                .filter(|t| !t.is_auto_generated())
                .find_map(|t| t.link)
            else {
                return Ok(false);
            };
            let response = self.get_signed(&link, &label).await?;
            let text = response.text().await.map_err(|e| IngestError::Http {
                url: label.clone(),
                message: e.without_url().to_string(),
            })?;
            let mut file = scratch
                .create_file(CAPTION_REFERENCE)
                .map_err(|e| scratch_error(&label, e))?;
            file.write_all(text.as_bytes())
                .map_err(|e| IngestError::Http {
                    url: label.clone(),
                    message: format!("writing to scratch: {e}"),
                })?;
            Ok(true)
        }
        .await;
        match result {
            Ok(true) => tracing::info!(talk = %talk.id, vimeo_id = %video_id,
                                       "Vimeo text track saved in scratch as a reference"),
            Ok(false) => {}
            Err(e) => tracing::warn!(talk = %talk.id, vimeo_id = %video_id, error = %e,
                                     "could not read the Vimeo text track; carrying on"),
        }
    }

    /// GETs a Vimeo API path with the token. `None` for 403 and 404, which mean this token may
    /// not read the video; any other failure is an error, so a bad token shows up instead of
    /// holding every talk.
    async fn api_json<T: for<'de> Deserialize<'de>>(
        &self,
        api: &Url,
        token: &Secret,
        path_and_query: &str,
    ) -> Result<Option<T>, IngestError> {
        let url = api.join(path_and_query).map_err(|e| IngestError::Parse {
            url: path_and_query.to_owned(),
            message: e.to_string(),
        })?;
        let target = self.exclusions.clear(&url)?;
        let response = http::get_authorized(&self.client, &target, token, VIMEO_ACCEPT).await?;
        if matches!(response.status().as_u16(), 403 | 404) {
            return Ok(None);
        }
        let response = http::ok(&url, response)?;
        response
            .json()
            .await
            .map(Some)
            .map_err(|e| IngestError::Parse {
                url: url.to_string(),
                message: e.without_url().to_string(),
            })
    }

    /// Streams a response into the talk's scratch directory. `url` names the download in
    /// errors: the address for an enclosure, the video id for a signed Vimeo link.
    async fn download(
        &self,
        talk: &Talk<Consented>,
        url: &str,
        mut response: reqwest::Response,
        scratch: &ScratchDir,
    ) -> Result<PathBuf, IngestError> {
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

/// Replaces the address in a request error with `label`, so a signed link never reaches a log
/// line or the job's stored error.
fn redact(e: IngestError, label: &str) -> IngestError {
    match e {
        IngestError::Http { message, .. } => IngestError::Http {
            url: label.to_owned(),
            message,
        },
        IngestError::Status { status, .. } => IngestError::Status {
            url: label.to_owned(),
            status,
        },
        IngestError::Excluded {
            exclusion, reason, ..
        } => IngestError::Excluded {
            url: label.to_owned(),
            exclusion,
            reason,
        },
        IngestError::Parse { message, .. } => IngestError::Parse {
            url: label.to_owned(),
            message,
        },
        other => other,
    }
}

/// What the Vimeo API says about a video's files.
#[derive(Debug, Default, Deserialize)]
struct VideoFiles {
    #[serde(default)]
    download: Vec<VimeoFile>,
    #[serde(default)]
    files: Vec<VimeoFile>,
    #[serde(default)]
    play: Option<Play>,
}

#[derive(Debug, Default, Deserialize)]
struct Play {
    #[serde(default)]
    progressive: Vec<VimeoFile>,
}

#[derive(Debug, Deserialize)]
struct VimeoFile {
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    quality: Option<String>,
    #[serde(default, rename = "type")]
    mime: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

impl VimeoFile {
    /// A whole file, video with its sound, rather than a streaming manifest.
    fn is_whole_file(&self) -> bool {
        !matches!(self.quality.as_deref(), Some("hls" | "dash"))
            && !self
                .mime
                .as_deref()
                .is_some_and(|m| m.contains("mpegurl") || m.contains("dash"))
    }
}

/// The link of the smallest whole file among those the API lists. Every progressive file
/// carries the sound, and the smallest is quickest to fetch; a file of unknown size comes last.
fn smallest_file(video: &VideoFiles) -> Option<String> {
    let progressive = video.play.iter().flat_map(|p| p.progressive.iter());
    video
        .download
        .iter()
        .chain(video.files.iter())
        .chain(progressive)
        .filter(|f| f.is_whole_file())
        .filter_map(|f| Some((f.size.unwrap_or(u64::MAX), f.link.clone()?)))
        .min_by_key(|(size, _)| *size)
        .map(|(_, link)| link)
}

/// A video's text tracks.
#[derive(Debug, Deserialize)]
struct TextTracks {
    #[serde(default)]
    data: Vec<TextTrack>,
}

#[derive(Debug, Deserialize)]
struct TextTrack {
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl TextTrack {
    /// Vimeo marks its machine captions with an `-x-autogen` language and names them so.
    fn is_auto_generated(&self) -> bool {
        self.language
            .as_deref()
            .is_some_and(|l| l.to_ascii_lowercase().contains("autogen"))
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.to_ascii_lowercase().contains("auto-generated"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_smallest_whole_file_wins_and_manifests_are_skipped() {
        let video: VideoFiles = serde_json::from_value(serde_json::json!({
            "download": [
                { "quality": "source", "type": "source", "size": 900, "link": "https://f/source" },
                { "quality": "hd", "type": "video/mp4", "size": 500, "link": "https://f/hd" }
            ],
            "files": [
                { "quality": "hls", "type": "video/mp4", "size": 1, "link": "https://f/hls" },
                { "quality": "sd", "type": "video/mp4", "size": 200, "link": "https://f/sd" },
                { "quality": "sd", "type": "video/mp4", "link": "https://f/unknown" }
            ],
            "play": { "progressive": [
                { "type": "video/mp4", "size": 150, "link": "https://f/240p" }
            ] }
        }))
        .unwrap();
        assert_eq!(smallest_file(&video).as_deref(), Some("https://f/240p"));
        assert_eq!(smallest_file(&VideoFiles::default()), None);
    }

    #[test]
    fn machine_text_tracks_are_recognized() {
        let track = |language: &str, name: &str| TextTrack {
            link: None,
            language: Some(language.into()),
            name: Some(name.into()),
        };
        assert!(track("en-x-autogen", "English").is_auto_generated());
        assert!(track("en", "English (auto-generated)").is_auto_generated());
        assert!(!track("en", "English").is_auto_generated());
    }

    #[test]
    fn a_redacted_error_names_the_label_not_the_link() {
        let e = redact(
            IngestError::Status {
                url: "https://cdn.example/file.mp4?signature=abc".into(),
                status: 410,
            },
            "vimeo video 123",
        );
        let text = e.to_string();
        assert!(!text.contains("signature"), "{text}");
        assert!(text.contains("vimeo video 123"), "{text}");
    }
}
