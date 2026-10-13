//! The Vimeo adapter (Spec 03, Seattle Soto Zen sources). With an access token it lists the
//! account's videos through the Vimeo API, page by page. Without one it reads the Vimeo links
//! embedded in the sangha's public talks page, after checking `robots.txt`, and takes each
//! video's title, duration and upload date from Vimeo's public oEmbed endpoint.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use sqlx::types::chrono::{DateTime, NaiveDateTime, Utc};
use url::Url;
use wt_core::Secret;

use crate::http::{self, Exclusions, parse_url};
use crate::source::{DiscoveredVideo, IngestError, SourceAdapter, SourcePlatform};

const PAGE_SIZE: u32 = 100;
const API_FIELDS: &str = "uri,name,description,duration,created_time,link,privacy,license,\
                          metadata.connections.texttracks.total";

/// A Vimeo id as it appears in a page: a link or embed (`vimeo.com/123456789`,
/// `player.vimeo.com/video/123456789`), or the `data-video_id` attribute that WordPress video
/// playlist plugins such as Vimeotheque put on each playlist item.
static VIMEO_LINK: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?:(?:player\.)?vimeo\.com/(?:video/)?|data-video_id=["'])(\d{6,12})\b"#).ok()
});

/// Where the adapter sends requests. Tests point these at a local mock server.
#[derive(Debug, Clone)]
pub struct VimeoEndpoints {
    /// The API base, `https://api.vimeo.com`.
    pub api: Url,
    /// The oEmbed base, `https://vimeo.com`.
    pub oembed: Url,
}

impl VimeoEndpoints {
    /// The real Vimeo addresses.
    pub fn vimeo() -> Result<Self, IngestError> {
        Ok(Self {
            api: parse_url("https://api.vimeo.com")?,
            oembed: parse_url("https://vimeo.com")?,
        })
    }
}

/// Lists a Vimeo account's videos.
pub struct VimeoAdapter {
    source: String,
    client: reqwest::Client,
    token: Option<Secret>,
    talks_page: Option<Url>,
    endpoints: VimeoEndpoints,
    exclusions: Exclusions,
}

impl VimeoAdapter {
    /// An adapter for `source`. With `token` it uses the API, otherwise `talks_page`. It never
    /// requests an address `exclusions` covers, and skips a video whose page one covers.
    pub fn new(
        source: &str,
        token: Option<Secret>,
        talks_page: Option<&str>,
        endpoints: VimeoEndpoints,
        exclusions: Exclusions,
    ) -> Result<Self, IngestError> {
        let client = http::client()?;
        Ok(Self {
            source: source.to_owned(),
            client,
            token,
            talks_page: talks_page.map(parse_url).transpose()?,
            endpoints,
            exclusions,
        })
    }

    async fn get(
        &self,
        url: &Url,
        token: Option<&Secret>,
    ) -> Result<reqwest::Response, IngestError> {
        let target = self.exclusions.clear(url)?;
        match token {
            Some(token) => {
                http::get_authorized(
                    &self.client,
                    &target,
                    token,
                    "application/vnd.vimeo.*+json;version=3.4",
                )
                .await
            }
            None => http::get(&self.client, &target).await,
        }
    }

    async fn get_ok(
        &self,
        url: &Url,
        token: Option<&Secret>,
    ) -> Result<reqwest::Response, IngestError> {
        http::ok(url, self.get(url, token).await?)
    }

    async fn json<T: for<'de> Deserialize<'de>>(
        url: &Url,
        response: reqwest::Response,
    ) -> Result<T, IngestError> {
        response.json().await.map_err(|e| IngestError::Parse {
            url: url.to_string(),
            message: e.without_url().to_string(),
        })
    }

    async fn discover_api(&self, token: &Secret) -> Result<Vec<DiscoveredVideo>, IngestError> {
        let mut next = Some(self.api_url(&format!(
            "/me/videos?per_page={PAGE_SIZE}&fields={API_FIELDS}"
        ))?);
        let mut videos = Vec::new();
        while let Some(url) = next.take() {
            let response = self.get_ok(&url, Some(token)).await?;
            let page: ApiPage = Self::json(&url, response).await?;
            for item in page.data {
                videos.push(api_video(&url, item)?);
            }
            next = page
                .paging
                .and_then(|p| p.next)
                .map(|n| self.api_url(&n))
                .transpose()?;
        }
        Ok(videos)
    }

    fn api_url(&self, path_and_query: &str) -> Result<Url, IngestError> {
        self.endpoints
            .api
            .join(path_and_query)
            .map_err(|e| IngestError::Parse {
                url: path_and_query.to_owned(),
                message: e.to_string(),
            })
    }

    /// The Vimeo ids the talks page links or embeds, in page order, after checking
    /// `robots.txt`. A disallowed page is never requested.
    pub async fn talks_page_ids(&self) -> Result<Vec<String>, IngestError> {
        let page = self
            .talks_page
            .as_ref()
            .ok_or_else(|| IngestError::Misconfigured {
                source_name: self.source.clone(),
                what: "a talks_page",
            })?;
        let target = self.exclusions.clear(page)?;
        http::check_robots(&self.client, &target).await?;
        let html = http::text(page, http::get_ok(&self.client, &target).await?).await?;
        Ok(page_video_ids(&html))
    }

    /// The account's videos from the API, or `None` without an access token.
    pub async fn account_videos(&self) -> Result<Option<Vec<DiscoveredVideo>>, IngestError> {
        match &self.token {
            Some(token) => Ok(Some(self.discover_api(token).await?)),
            None => Ok(None),
        }
    }

    async fn discover_page(&self, page: &Url) -> Result<Vec<DiscoveredVideo>, IngestError> {
        let ids = self.talks_page_ids().await?;
        let mut videos = Vec::new();
        for id in ids {
            let video_url = format!("https://vimeo.com/{id}");
            if let Err(excluded) = parse_url(&video_url).and_then(|u| self.exclusions.clear(&u)) {
                tracing::warn!(source = %self.source, vimeo_id = %id, %excluded,
                               "skipping an excluded video from the talks page");
                continue;
            }
            let mut oembed = self
                .endpoints
                .oembed
                .join("/api/oembed.json")
                .map_err(|e| IngestError::Parse {
                    url: "/api/oembed.json".to_owned(),
                    message: e.to_string(),
                })?;
            oembed.query_pairs_mut().append_pair("url", &video_url);
            let response = self.get(&oembed, None).await?;
            if !response.status().is_success() {
                // A video the page embeds but Vimeo will not describe publicly. The
                // completeness report lists these; discovery carries on with the rest.
                tracing::warn!(source = %self.source, vimeo_id = %id, status = response.status().as_u16(),
                               "oEmbed refused a video from the talks page");
                continue;
            }
            let raw: serde_json::Value = Self::json(&oembed, response).await?;
            let item: OEmbed =
                serde_json::from_value(raw.clone()).map_err(|e| IngestError::Parse {
                    url: oembed.to_string(),
                    message: e.to_string(),
                })?;
            // A video restricted to embedding on its owner's domain answers 200 with
            // `domain_status_code` 403 and no title or duration. Treat it like a refusal.
            let Some(title) = item.title.filter(|t| !t.trim().is_empty()) else {
                tracing::warn!(source = %self.source, vimeo_id = %id,
                               domain_status = ?item.domain_status_code,
                               "oEmbed gave no details for a video from the talks page");
                continue;
            };
            videos.push(DiscoveredVideo {
                external_id: id.clone(),
                url: video_url,
                title,
                description: item.description.filter(|d| !d.trim().is_empty()),
                published_at: item.upload_date.as_deref().and_then(parse_upload_date),
                duration_s: item.duration,
                channel_ref: item.author_url,
                has_captions: false,
                raw: serde_json::json!({ "oembed": raw, "talks_page": page.as_str() }),
            });
        }
        Ok(videos)
    }
}

#[async_trait]
impl SourceAdapter for VimeoAdapter {
    fn platform(&self) -> SourcePlatform {
        SourcePlatform::Vimeo
    }

    async fn discover(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<DiscoveredVideo>, IngestError> {
        let videos = match (&self.token, &self.talks_page) {
            (Some(token), _) => self.discover_api(token).await?,
            (None, Some(page)) => self.discover_page(page).await?,
            (None, None) => {
                return Err(IngestError::Misconfigured {
                    source_name: self.source.clone(),
                    what: "a token (its token_env) or a talks_page",
                });
            }
        };
        Ok(videos
            .into_iter()
            .filter(|v| match (since, v.published_at) {
                (Some(since), Some(at)) => at >= since,
                _ => true,
            })
            .collect())
    }
}

/// Every distinct Vimeo id linked or embedded in a page, in page order.
pub fn page_video_ids(html: &str) -> Vec<String> {
    let Some(re) = VIMEO_LINK.as_ref() else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    re.captures_iter(html)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_owned()))
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// oEmbed's `upload_date`, `2020-05-31 12:00:00`, read as UTC.
fn parse_upload_date(text: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| n.and_utc())
}

#[derive(Debug, Deserialize)]
struct ApiPage {
    data: Vec<serde_json::Value>,
    paging: Option<Paging>,
}

#[derive(Debug, Deserialize)]
struct Paging {
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiItem {
    uri: String,
    name: String,
    description: Option<String>,
    duration: Option<i32>,
    created_time: Option<String>,
    link: Option<String>,
    metadata: Option<ApiMetadata>,
}

#[derive(Debug, Deserialize)]
struct ApiMetadata {
    connections: Option<ApiConnections>,
}

#[derive(Debug, Deserialize)]
struct ApiConnections {
    texttracks: Option<ApiTotal>,
}

#[derive(Debug, Deserialize)]
struct ApiTotal {
    total: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct OEmbed {
    title: Option<String>,
    domain_status_code: Option<u16>,
    description: Option<String>,
    duration: Option<i32>,
    upload_date: Option<String>,
    author_url: Option<String>,
}

fn api_video(page: &Url, raw: serde_json::Value) -> Result<DiscoveredVideo, IngestError> {
    let item: ApiItem = serde_json::from_value(raw.clone()).map_err(|e| IngestError::Parse {
        url: page.to_string(),
        message: e.to_string(),
    })?;
    let id = item
        .uri
        .rsplit('/')
        .next()
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
        .ok_or_else(|| IngestError::Parse {
            url: page.to_string(),
            message: format!("video uri {:?} has no numeric id", item.uri),
        })?
        .to_owned();
    let captions = item
        .metadata
        .and_then(|m| m.connections)
        .and_then(|c| c.texttracks)
        .and_then(|t| t.total)
        .is_some_and(|n| n > 0);
    Ok(DiscoveredVideo {
        url: item
            .link
            .unwrap_or_else(|| format!("https://vimeo.com/{id}")),
        external_id: id,
        title: item.name,
        description: item.description.filter(|d| !d.trim().is_empty()),
        published_at: item
            .created_time
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc)),
        duration_s: item.duration,
        channel_ref: None,
        has_captions: captions,
        raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_ids_come_from_links_and_embeds_once_each_in_order() {
        let html = r#"
            <iframe src="https://player.vimeo.com/video/802312541?h=abc"></iframe>
            <a href="https://vimeo.com/672990540">Discrimination and Suffering</a>
            <a href="https://vimeo.com/802312541">again</a>
            <a href="https://vimeo.com/channels/staffpicks">not a video</a>
            <div class="cvm-playlist-item" data-video_id="672974907"></div>
            <div class="cvm-playlist-item" data-video_id='672990540'></div>
        "#;
        assert_eq!(
            page_video_ids(html),
            ["802312541", "672990540", "672974907"]
        );
    }

    #[test]
    fn oembed_dates_read_as_utc() {
        let at = parse_upload_date("2020-05-31 12:00:00").unwrap();
        assert_eq!(at.to_rfc3339(), "2020-05-31T12:00:00+00:00");
        assert_eq!(parse_upload_date("yesterday"), None);
    }
}
