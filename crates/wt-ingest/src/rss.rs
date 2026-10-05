//! The podcast feed adapter (Spec 03, Licensed sources item 5, and Seattle Soto Zen sources
//! item 2). It reads each episode's title, publish date, duration, description and audio
//! enclosure. It checks `robots.txt` before reading the feed, identifies itself, and waits at
//! least a second between requests to the feed's host.

use std::io::Cursor;

use async_trait::async_trait;
use sqlx::types::chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

use crate::http::{self, Exclusions, MIN_DELAY, parse_url};
use crate::source::{DiscoveredVideo, IngestError, SourceAdapter, SourcePlatform};

/// Reads one podcast feed.
pub struct RssAdapter {
    client: reqwest::Client,
    feed: Url,
    exclusions: Exclusions,
    last_request: Mutex<Option<Instant>>,
}

impl RssAdapter {
    /// An adapter for the feed at `feed_url`, which never requests what `exclusions` covers.
    pub fn new(feed_url: &str, exclusions: Exclusions) -> Result<Self, IngestError> {
        Ok(Self {
            client: http::client()?,
            feed: parse_url(feed_url)?,
            exclusions,
            last_request: Mutex::new(None),
        })
    }

    /// Waits until at least [`MIN_DELAY`] has passed since the previous request.
    async fn throttle(&self) {
        let mut last = self.last_request.lock().await;
        if let Some(at) = *last {
            let since = at.elapsed();
            if since < MIN_DELAY {
                tokio::time::sleep(MIN_DELAY - since).await;
            }
        }
        *last = Some(Instant::now());
    }

    /// Fetches and parses the feed.
    pub async fn episodes(&self) -> Result<Vec<DiscoveredVideo>, IngestError> {
        let target = self.exclusions.clear(&self.feed)?;
        self.throttle().await;
        http::check_robots(&self.client, &target).await?;
        self.throttle().await;
        let body = http::text(&self.feed, http::get_ok(&self.client, &target).await?).await?;
        parse_feed(&self.feed, &body)
    }
}

#[async_trait]
impl SourceAdapter for RssAdapter {
    fn platform(&self) -> SourcePlatform {
        SourcePlatform::Rss
    }

    async fn discover(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<DiscoveredVideo>, IngestError> {
        Ok(self
            .episodes()
            .await?
            .into_iter()
            .filter(|e| match (since, e.published_at) {
                (Some(since), Some(at)) => at >= since,
                _ => true,
            })
            .collect())
    }
}

/// `itunes:duration` as seconds: `HH:MM:SS`, `MM:SS` or plain seconds.
pub fn parse_duration(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    text.split(':').try_fold(0i32, |acc, part| {
        Some(acc.checked_mul(60)? + part.trim().parse::<i32>().ok()?)
    })
}

/// Parses an RSS document into episodes. Items with no enclosure carry no audio and are
/// skipped; an episode's id is its `guid`, or else its enclosure address.
pub fn parse_feed(feed: &Url, body: &str) -> Result<Vec<DiscoveredVideo>, IngestError> {
    let channel = ::rss::Channel::read_from(Cursor::new(body.as_bytes())).map_err(|e| {
        IngestError::Parse {
            url: feed.to_string(),
            message: e.to_string(),
        }
    })?;
    Ok(channel
        .items()
        .iter()
        .filter_map(|item| {
            let enclosure = item.enclosure()?;
            let id = item
                .guid()
                .map(|g| g.value().to_owned())
                .filter(|g| !g.trim().is_empty())
                .unwrap_or_else(|| enclosure.url().to_owned());
            let itunes = item.itunes_ext();
            Some(DiscoveredVideo {
                external_id: id.clone(),
                url: item.link().unwrap_or(enclosure.url()).to_owned(),
                title: item.title().unwrap_or_default().trim().to_owned(),
                description: item
                    .description()
                    .or_else(|| itunes.and_then(|i| i.summary()))
                    .map(str::to_owned)
                    .filter(|d| !d.trim().is_empty()),
                published_at: item
                    .pub_date()
                    .and_then(|d| DateTime::parse_from_rfc2822(d.trim()).ok())
                    .map(|d| d.with_timezone(&Utc)),
                duration_s: itunes.and_then(|i| i.duration()).and_then(parse_duration),
                channel_ref: Some(feed.to_string()),
                has_captions: false,
                raw: serde_json::json!({
                    "guid": id,
                    "link": item.link(),
                    "enclosure": {
                        "url": enclosure.url(),
                        "type": enclosure.mime_type(),
                        "length": enclosure.length(),
                    },
                }),
            })
        })
        .filter(|e| !e.title.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn itunes_durations_read_in_all_three_forms() {
        assert_eq!(parse_duration("1:02:14"), Some(3734));
        assert_eq!(parse_duration("41:09"), Some(2469));
        assert_eq!(parse_duration("2469"), Some(2469));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("about an hour"), None);
    }
}
