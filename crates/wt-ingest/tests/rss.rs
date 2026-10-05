//! The feed adapter and the Apple lookup against recorded, synthetic responses served by a
//! local `wiremock` server. No request leaves the machine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use serde_json::json;
use wiremock::matchers::{header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_ingest::http::Exclusions;
use wt_ingest::podcast::resolve_feed;
use wt_ingest::rss::RssAdapter;
use wt_ingest::{IngestError, SourceAdapter};

/// A feed in the shape podcast hosts publish, with invented episodes.
const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
  <channel>
    <title>Fixture Zendo Dharma Talks</title>
    <link>https://example.invalid/</link>
    <description>Synthetic fixture feed.</description>
    <item>
      <title>Dharma Talk on Fixtures, by Ada Fixture on May 12, 2024</title>
      <guid isPermaLink="false">fixture-episode-2</guid>
      <pubDate>Tue, 21 May 2024 18:00:00 +0000</pubDate>
      <itunes:duration>40:12</itunes:duration>
      <description>Talk notes.</description>
      <enclosure url="https://example.invalid/audio/2.mp3" type="audio/mpeg" length="38000000"/>
    </item>
    <item>
      <title>A Long Talk</title>
      <guid>fixture-episode-1</guid>
      <pubDate>Sun, 05 May 2024 18:00:00 +0000</pubDate>
      <itunes:duration>1:02:14</itunes:duration>
      <enclosure url="https://example.invalid/audio/1.m4a" type="audio/x-m4a" length="60000000"/>
    </item>
    <item>
      <title>An announcement with no audio</title>
      <guid>fixture-post</guid>
      <pubDate>Sat, 04 May 2024 18:00:00 +0000</pubDate>
    </item>
  </channel>
</rss>"#;

async fn mount_robots(server: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_owned()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn the_feed_reads_episodes_identifies_itself_and_waits_between_requests() {
    let server = MockServer::start().await;
    mount_robots(&server, "User-agent: *\nDisallow: /private/\n").await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .and(header_regex(
            "user-agent",
            r"^WheelTurning/\S+ \(\+https://demo\.wheelturning\.org\)$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(FEED))
        .expect(1)
        .mount(&server)
        .await;

    let adapter =
        RssAdapter::new(&format!("{}/feed.xml", server.uri()), Exclusions::default()).unwrap();
    let started = Instant::now();
    let episodes = adapter.discover(None).await.unwrap();
    assert!(
        started.elapsed().as_millis() >= 1000,
        "a second between robots.txt and the feed"
    );

    assert_eq!(
        episodes.len(),
        2,
        "the item without an enclosure is not an episode"
    );
    let first = &episodes[0];
    assert_eq!(first.external_id, "fixture-episode-2");
    assert_eq!(first.duration_s, Some(2412));
    assert_eq!(
        first.published_at.unwrap().to_rfc3339(),
        "2024-05-21T18:00:00+00:00"
    );
    assert_eq!(first.description.as_deref(), Some("Talk notes."));
    assert_eq!(
        first.raw["enclosure"]["url"],
        "https://example.invalid/audio/2.mp3"
    );
    assert_eq!(first.raw["enclosure"]["type"], "audio/mpeg");
    assert_eq!(episodes[1].duration_s, Some(3734));
    assert_eq!(
        first.url, "https://example.invalid/audio/2.mp3",
        "no link: the enclosure"
    );
}

#[tokio::test]
async fn a_feed_path_robots_txt_disallows_is_never_requested() {
    let server = MockServer::start().await;
    mount_robots(&server, "User-agent: *\nDisallow: /feed\n").await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FEED))
        .expect(0)
        .mount(&server)
        .await;
    let adapter =
        RssAdapter::new(&format!("{}/feed.xml", server.uri()), Exclusions::default()).unwrap();
    assert!(matches!(
        adapter.discover(None).await.unwrap_err(),
        IngestError::RobotsDisallowed(_)
    ));
}

#[tokio::test]
async fn the_apple_lookup_gives_the_feed_address() {
    let server = MockServer::start().await;
    // Apple's real rules: these bar nested lookups, not the API at the root.
    mount_robots(
        &server,
        "User-agent: *\nDisallow: /search*\nDisallow: /*/lookup?\n",
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/lookup"))
        .and(query_param("id", "1000000001"))
        .and(query_param("entity", "podcast"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resultCount": 1,
            "results": [{
                "wrapperType": "track", "kind": "podcast",
                "collectionName": "Fixture Zendo Dharma Talks",
                "feedUrl": "https://example.invalid/feed.xml",
            }],
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/lookup"))
        .and(query_param("id", "1000000002"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "resultCount": 0, "results": [] })),
        )
        .mount(&server)
        .await;

    let lookup = format!("{}/lookup", server.uri());
    let found = resolve_feed(&lookup, 1_000_000_001, &Exclusions::default())
        .await
        .unwrap();
    assert_eq!(found.feed_url, "https://example.invalid/feed.xml");
    assert_eq!(found.name.as_deref(), Some("Fixture Zendo Dharma Talks"));
    assert!(
        resolve_feed(&lookup, 1_000_000_002, &Exclusions::default())
            .await
            .is_err(),
        "no such podcast"
    );
}
