//! The EngageWisdom adapter (Spec 03, EngageWisdom sources). A collection's listing pages link
//! its talks fifty at a time; each talk page holds the title, speaker, date, location, serial,
//! the transcript and the audio address. Requests obey `robots.txt` and its crawl delay, one at
//! a time. The page's AI summary and keywords are never read.

use std::collections::BTreeSet;
use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use sqlx::types::chrono::{DateTime, NaiveDate, Utc};
use url::Url;

use crate::http::{self, Exclusions, parse_url};
use crate::source::{DiscoveredVideo, IngestError, SourceAdapter, SourcePlatform};

/// The crawl delay EngageWisdom's `robots.txt` asks for.
pub const CRAWL_DELAY: Duration = Duration::from_secs(10);

static TALK_LINK: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"href="(/talks/[^"?#]+)[^"]*""#).ok());
static TITLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?s)pane-ew-talk-title-pane.*?<h3[^>]*>(.*?)</h3>"#).ok());
static BYLINE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?s)pane-byline[^>]*>\s*<div class="pane-content">(.*?)</div>"#).ok()
});
static SPEAKER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"<a href="/speaker/[^"]*">([^<]+)</a>"#).ok());
static LOCATION: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"<a href="/location/[^"]*">([^<]+)</a>"#).ok());
static DATE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"<strong>\s*([A-Z][a-z]+ \d{1,2})(?:st|nd|rd|th)?, (\d{4})\s*</strong>").ok()
});
static SERIAL: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?s)field-name-field-serial-number.*?</div>\s*([A-Z]{1,4}-\d{3,6})"#).ok()
});
static TRANSCRIPT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?s)field-name-field-wisdom-transcript[^>]*>(.*?)</div>\s*</div>\s*</div>"#).ok()
});
static PARAGRAPH: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?s)<p[^>]*>(.*?)</p>").ok());
static TAG: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"<[^>]+>").ok());
static AUDIO: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#""wavesurfer-player-id":"(https?:[^"]+?\.(?:mp4|m4a|mp3))""#).ok()
});

fn re(r: &LazyLock<Option<Regex>>) -> Result<&Regex, IngestError> {
    r.as_ref().ok_or_else(|| IngestError::Parse {
        url: String::new(),
        message: "an EngageWisdom pattern failed to compile".into(),
    })
}

fn text(html: &str) -> String {
    let stripped = TAG.as_ref().map_or_else(
        || html.to_owned(),
        |t| t.replace_all(html, " ").into_owned(),
    );
    let decoded = stripped
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The talk links on a listing page, in order, without repeats.
pub fn talk_links(html: &str) -> Vec<String> {
    let Some(re) = TALK_LINK.as_ref() else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    re.captures_iter(html)
        .map(|c| c[1].to_owned())
        .filter(|l| seen.insert(l.clone()))
        .collect()
}

/// One talk page, as read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TalkPage {
    /// Such as `BZ-02832`.
    pub serial: String,
    /// The title.
    pub title: String,
    /// The speakers the byline names.
    pub speakers: Vec<String>,
    /// Where it was given.
    pub location: Option<String>,
    /// When, `YYYY-MM-DD`.
    pub date: Option<String>,
    /// The transcript: paragraphs separated by blank lines, `[mm:ss]` markers kept.
    pub transcript: String,
    /// The audio file.
    pub audio_url: Option<String>,
}

/// Reads a talk page. The AI summary and keywords are not read.
pub fn parse_talk(html: &str, url: &str) -> Result<TalkPage, IngestError> {
    let parse = |what: &str| IngestError::Parse {
        url: url.to_owned(),
        message: format!("no {what} on the talk page"),
    };
    let title = re(&TITLE)?
        .captures(html)
        .map(|c| text(&c[1]))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| parse("title"))?;
    let byline = re(&BYLINE)?
        .captures(html)
        .map(|c| c[1].to_owned())
        .unwrap_or_default();
    let speakers: Vec<String> = re(&SPEAKER)?
        .captures_iter(&byline)
        .map(|c| text(&c[1]))
        .collect();
    let location = re(&LOCATION)?.captures(&byline).map(|c| text(&c[1]));
    let date = re(&DATE)?.captures(&byline).and_then(|c| {
        NaiveDate::parse_from_str(&format!("{} {}", &c[1], &c[2]), "%B %d %Y")
            .ok()
            .map(|d| d.format("%Y-%m-%d").to_string())
    });
    // A few pages carry no serial; their address is their identity then.
    let serial = match re(&SERIAL)?.captures(html) {
        Some(c) => c[1].to_owned(),
        None => {
            let slug = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| parse("serial or address"))?;
            format!("page:{slug}")
        }
    };
    let body = re(&TRANSCRIPT)?
        .captures(html)
        .map(|c| c[1].to_owned())
        .unwrap_or_default();
    let paragraphs: Vec<String> = re(&PARAGRAPH)?
        .captures_iter(&body)
        .map(|c| text(&c[1]))
        .filter(|p| !p.is_empty())
        .collect();
    // The address sits in a JSON string, so `\/` and `'` are escapes. Left in, a URL
    // parser reads the backslash as a slash and the file is not found.
    let audio_url = re(&AUDIO)?.captures(html).map(|c| {
        serde_json::from_str::<String>(&format!("\"{}\"", &c[1]))
            .unwrap_or_else(|_| c[1].replace("\\/", "/"))
    });
    Ok(TalkPage {
        serial,
        title,
        speakers,
        location,
        date,
        transcript: paragraphs.join("\n\n"),
        audio_url,
    })
}

/// Lists one EngageWisdom collection.
pub struct EngageWisdomAdapter {
    client: reqwest::Client,
    collection: Url,
    exclusions: Exclusions,
    max_talks: Option<usize>,
    delay: Duration,
}

impl EngageWisdomAdapter {
    /// An adapter for the collection at `collection_url`, reading at most `max_talks` talks.
    pub fn new(
        collection_url: &str,
        max_talks: Option<usize>,
        exclusions: Exclusions,
    ) -> Result<Self, IngestError> {
        Ok(Self {
            client: http::client()?,
            collection: parse_url(collection_url)?,
            exclusions,
            max_talks,
            delay: CRAWL_DELAY,
        })
    }

    /// The same with another delay between requests: tests.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    async fn page(&self, url: &Url, first: &mut bool) -> Result<String, IngestError> {
        if !*first {
            tokio::time::sleep(self.delay).await;
        }
        *first = false;
        let target = self.exclusions.clear(url)?;
        let response = http::get_ok(&self.client, &target).await?;
        http::text(url, response).await
    }
}

#[async_trait]
impl SourceAdapter for EngageWisdomAdapter {
    fn platform(&self) -> SourcePlatform {
        SourcePlatform::EngageWisdom
    }

    async fn discover(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<DiscoveredVideo>, IngestError> {
        let target = self.exclusions.clear(&self.collection)?;
        http::check_robots(&self.client, &target).await?;
        let mut first = false;
        let mut links: Vec<Url> = Vec::new();
        let mut seen = BTreeSet::new();
        let limit = self.max_talks.unwrap_or(usize::MAX);
        for n in 0.. {
            let mut page_url = self.collection.clone();
            if n > 0 {
                page_url
                    .query_pairs_mut()
                    .append_pair("page", &n.to_string());
            }
            let html = self.page(&page_url, &mut first).await?;
            let found: Vec<String> = talk_links(&html)
                .into_iter()
                .filter(|l| seen.insert(l.clone()))
                .collect();
            if found.is_empty() {
                break;
            }
            for l in found {
                if links.len() < limit {
                    links.push(self.collection.join(&l).map_err(|e| IngestError::Parse {
                        url: l.clone(),
                        message: e.to_string(),
                    })?);
                }
            }
            if links.len() >= limit {
                break;
            }
        }
        let mut out = Vec::new();
        for url in links {
            let target = self.exclusions.clear(&url)?;
            http::check_robots(&self.client, &target).await?;
            let html = self.page(&url, &mut first).await?;
            let talk = parse_talk(&html, url.as_str())?;
            let published_at = talk
                .date
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .and_then(|d| d.and_hms_opt(12, 0, 0))
                .map(|d| d.and_utc());
            if let (Some(since), Some(at)) = (since, published_at)
                && at < since
            {
                continue;
            }
            let speaker = talk.speakers.join(" and ");
            out.push(DiscoveredVideo {
                external_id: talk.serial.clone(),
                url: url.to_string(),
                title: talk.title.clone(),
                // The speaker the byline names, which classification resolves alone.
                description: (!speaker.is_empty()).then(|| format!("Given by {speaker}")),
                published_at,
                duration_s: None,
                channel_ref: Some(self.collection.to_string()),
                has_captions: !talk.transcript.is_empty(),
                raw: serde_json::to_value(&talk).map_err(|e| IngestError::Parse {
                    url: url.to_string(),
                    message: e.to_string(),
                })?,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic talk page in EngageWisdom's markup.
    pub const PAGE: &str = r#"<html><head><title>Fixture Archive</title></head><body>
<div class="panel-pane pane-views pane-ew-talk-title-pane"><div class="pane-content"><div class="view"><div class="view-content"><div class="views-row"> <h3>The Fixture Pond</h3> </div></div></div></div></div>
<div class="panel-pane pane-byline talk-head"  >  <div class="pane-content">   Given by <a href="/speaker/fixture-teacher">Fixture Teacher</a> at <a href="/location/fixture-hall">Fixture Hall</a> on <strong>April 24th, 2021</strong>  </div> </div>
<div class="field field-name-field-serial-number field-type-text field-label-inline clearfix field-wrapper"><div  class="field-label">Serial:&nbsp;</div>FX-00042</div>
<div class="field field-name-field-ai-summary"><div class="field-label">AI Summary:&nbsp;</div><p>A machine summary that must never be read.</p></div>
<div class="panel-pane pane-entity-field pane-node-field-wisdom-transcript talk-transcript"  ><div class="pane-content"><div class="field field-name-field-wisdom-transcript field-type-text-long field-label-above field-wrapper"><div  class="field-label">Transcript:&nbsp;</div><p>[00:00]</p><p>Good morning. The pond is still &amp; clear.</p><p>[01:02]</p><p>Student: Is it always still?</p><p>No, the wind moves it.</p> </div>  </div>       </div>
<script>jQuery.extend(Drupal.settings, {"audiofield":{"wavesurfer-player-id":"https:\/\/media.example.invalid\/s3fs-public\/audio\/FX-00042.mp4"}});</script>
</body></html>"#;

    #[test]
    fn reads_a_talk_page_without_the_ai_summary() {
        let t = parse_talk(PAGE, "https://fx.example.invalid/talks/fixture-pond").unwrap();
        assert_eq!(t.serial, "FX-00042");
        assert_eq!(t.title, "The Fixture Pond");
        assert_eq!(t.speakers, vec!["Fixture Teacher"]);
        assert_eq!(t.location.as_deref(), Some("Fixture Hall"));
        assert_eq!(t.date.as_deref(), Some("2021-04-24"));
        assert_eq!(
            t.audio_url.as_deref(),
            Some("https://media.example.invalid/s3fs-public/audio/FX-00042.mp4")
        );
        assert!(
            t.transcript
                .starts_with("[00:00]\n\nGood morning. The pond is still & clear.")
        );
        assert!(
            !serde_json::to_string(&t)
                .unwrap()
                .contains("machine summary")
        );
    }

    #[test]
    fn a_page_without_a_serial_is_known_by_its_address() {
        let page = PAGE.replace("FX-00042", "");
        let t = parse_talk(&page, "https://fx.example.invalid/talks/sunday-talk-18").unwrap();
        assert_eq!(t.serial, "page:sunday-talk-18");
    }

    #[test]
    fn json_escapes_in_the_audio_address_are_decoded() {
        let page = PAGE.replace("FX-00042.mp4", r"What-I-Don't-Know.mp4");
        let t = parse_talk(&page, "https://fx.example.invalid/talks/fixture-pond").unwrap();
        assert_eq!(
            t.audio_url.as_deref(),
            Some("https://media.example.invalid/s3fs-public/audio/What-I-Don't-Know.mp4")
        );
    }

    #[test]
    fn lists_talk_links_once_in_order() {
        let html =
            r#"<a href="/talks/b">B</a><a href="/talks/a?x=1">A</a><a href="/talks/b">B</a>"#;
        assert_eq!(talk_links(html), vec!["/talks/b", "/talks/a"]);
    }
}
