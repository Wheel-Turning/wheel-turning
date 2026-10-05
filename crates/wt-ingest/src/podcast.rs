//! Finding a podcast's feed address from its Apple Podcasts id (Spec 03, Seattle Soto Zen
//! sources, item 2a). Apple's public lookup API answers with the feed the publisher registered.
//! The caller prints it and saves it only after a person confirms it.

use serde::Deserialize;
use url::Url;

use crate::http::{self, parse_url};
use crate::source::IngestError;

/// Apple's lookup API.
pub const APPLE_LOOKUP: &str = "https://itunes.apple.com/lookup";

#[derive(Debug, Deserialize)]
struct Lookup {
    results: Vec<LookupResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LookupResult {
    feed_url: Option<String>,
    collection_name: Option<String>,
}

/// What the lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundFeed {
    /// The feed address.
    pub feed_url: String,
    /// The podcast's name, to show the person confirming it.
    pub name: Option<String>,
}

/// Looks up the feed for Apple Podcasts id `apple_id` at `lookup` (normally [`APPLE_LOOKUP`]),
/// after checking `robots.txt`.
pub async fn resolve_feed(
    lookup: &str,
    apple_id: u64,
    exclusions: &http::Exclusions,
) -> Result<FoundFeed, IngestError> {
    let client = http::client()?;
    let mut url: Url = parse_url(lookup)?;
    url.query_pairs_mut()
        .append_pair("id", &apple_id.to_string())
        .append_pair("entity", "podcast");
    let target = exclusions.clear(&url)?;
    http::check_robots(&client, &target).await?;
    tokio::time::sleep(http::MIN_DELAY).await;
    let response = http::get_ok(&client, &target).await?;
    let found: Lookup = response.json().await.map_err(|e| IngestError::Parse {
        url: url.to_string(),
        message: e.without_url().to_string(),
    })?;
    found
        .results
        .into_iter()
        .find_map(|r| {
            Some(FoundFeed {
                feed_url: r.feed_url.filter(|f| !f.trim().is_empty())?,
                name: r.collection_name,
            })
        })
        .ok_or_else(|| IngestError::Parse {
            url: url.to_string(),
            message: format!("no podcast with a feed address has Apple id {apple_id}"),
        })
}
