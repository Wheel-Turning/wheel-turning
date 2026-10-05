//! The Vimeo adapter against recorded, synthetic responses served by a local `wiremock` server:
//! the API across pages, the talks page with oEmbed, and `robots.txt`. No request leaves the
//! machine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_core::Secret;
use wt_ingest::vimeo::{VimeoAdapter, VimeoEndpoints};
use wt_ingest::{IngestError, SourceAdapter};

const TOKEN: &str = "fixture-token";

fn endpoints(server: &MockServer) -> VimeoEndpoints {
    let base = Url::parse(&server.uri()).unwrap();
    VimeoEndpoints {
        api: base.clone(),
        oembed: base,
    }
}

fn api_item(id: u64, name: &str, duration: i32, created: &str, tracks: i64) -> serde_json::Value {
    json!({
        "uri": format!("/videos/{id}"),
        "name": name,
        "description": "Synthetic fixture description.",
        "duration": duration,
        "created_time": created,
        "link": format!("https://vimeo.com/{id}"),
        "privacy": { "view": "anybody" },
        "license": null,
        "metadata": { "connections": { "texttracks": { "total": tracks } } },
    })
}

fn oembed(id: u64, title: &str, duration: i32, uploaded: &str) -> serde_json::Value {
    json!({
        "type": "video",
        "title": title,
        "duration": duration,
        "upload_date": uploaded,
        "video_id": id,
        "author_url": "https://vimeo.com/fixturesangha",
        "description": "",
    })
}

#[tokio::test]
async fn api_discovery_reads_every_page_with_the_token() {
    let server = MockServer::start().await;
    let page1: Vec<_> = (0..100)
        .map(|i| {
            api_item(
                100_000_000 + i,
                &format!("Fixture talk {i}"),
                2400,
                "2024-01-07T18:00:00+00:00",
                0,
            )
        })
        .collect();
    let page2 = vec![
        api_item(
            200_000_001,
            "Fixture talk with captions",
            1800,
            "2024-02-04T18:00:00+00:00",
            1,
        ),
        api_item(
            200_000_002,
            "Fixture clip",
            11,
            "2024-02-05T18:00:00+00:00",
            0,
        ),
    ];
    Mock::given(method("GET"))
        .and(path("/me/videos"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(wiremock::matchers::query_param_is_missing("page"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 102, "page": 1, "per_page": 100,
            "paging": { "next": "/me/videos?page=2&per_page=100", "previous": null },
            "data": page1,
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/me/videos"))
        .and(query_param("page", "2"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 102, "page": 2, "per_page": 100,
            "paging": { "next": null, "previous": "/me/videos?page=1&per_page=100" },
            "data": page2,
        })))
        .expect(2)
        .mount(&server)
        .await;

    let adapter = VimeoAdapter::new(
        "fixture-vimeo",
        Some(Secret::new(TOKEN)),
        None,
        endpoints(&server),
    )
    .unwrap();
    let videos = adapter.discover(None).await.unwrap();

    assert_eq!(videos.len(), 102, "both pages");
    let captioned = videos
        .iter()
        .find(|v| v.external_id == "200000001")
        .unwrap();
    assert_eq!(captioned.url, "https://vimeo.com/200000001");
    assert_eq!(captioned.duration_s, Some(1800));
    assert!(captioned.has_captions);
    assert_eq!(
        captioned.published_at.unwrap().to_rfc3339(),
        "2024-02-04T18:00:00+00:00"
    );
    assert_eq!(
        captioned.raw["uri"], "/videos/200000001",
        "the API item is kept as raw"
    );
    assert!(!videos[0].has_captions);

    // A second run, limited by date, reads both pages again.
    let since = "2024-02-01T00:00:00Z".parse().unwrap();
    assert_eq!(adapter.discover(Some(since)).await.unwrap().len(), 2);
}

async fn mount_site(server: &MockServer, robots: Option<&str>) {
    let robots = match robots {
        Some(body) => ResponseTemplate::new(200).set_body_string(body),
        None => ResponseTemplate::new(404),
    };
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(robots)
        .mount(server)
        .await;
}

#[tokio::test]
async fn talks_page_discovery_uses_oembed_for_each_embedded_video() {
    let server = MockServer::start().await;
    mount_site(&server, Some("User-agent: *\nDisallow: /wp-admin/\n")).await;
    Mock::given(method("GET"))
        .and(path("/talks-page/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<html><body>
                 <iframe src="https://player.vimeo.com/video/300000001?h=x"></iframe>
                 <a href="https://vimeo.com/300000002">Second</a>
                 <a href="https://vimeo.com/300000001">First again</a>
                 <iframe src="https://player.vimeo.com/video/300000003"></iframe>
                 <div class="cvm-playlist-item" data-video_id="300000004"></div>
               </body></html>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    for (id, title, duration, uploaded) in [
        (
            300_000_001u64,
            "Fixture talk one",
            2400,
            "2020-05-31 12:00:00",
        ),
        (300_000_002, "Fixture talk two", 2243, "2020-06-14 12:00:00"),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/oembed.json"))
            .and(query_param(
                "url",
                format!("https://vimeo.com/{id}").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(oembed(id, title, duration, uploaded)),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    // A video the site embeds but Vimeo will not describe publicly.
    Mock::given(method("GET"))
        .and(path("/api/oembed.json"))
        .and(query_param("url", "https://vimeo.com/300000003"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    // A video restricted to its owner's domain: 200, but no title or duration.
    Mock::given(method("GET"))
        .and(path("/api/oembed.json"))
        .and(query_param("url", "https://vimeo.com/300000004"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type": "video", "version": "1.0", "provider_name": "Vimeo",
            "domain_status_code": 403, "video_id": 300_000_004u64, "uri": "/videos/300000004",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = format!("{}/talks-page/", server.uri());
    let adapter =
        VimeoAdapter::new("fixture-vimeo", None, Some(&page), endpoints(&server)).unwrap();
    let videos = adapter.discover(None).await.unwrap();

    let ids: Vec<_> = videos.iter().map(|v| v.external_id.as_str()).collect();
    assert_eq!(
        ids,
        ["300000001", "300000002"],
        "page order, each once, refused and domain-restricted ones skipped"
    );
    assert_eq!(videos[0].title, "Fixture talk one");
    assert_eq!(videos[0].duration_s, Some(2400));
    assert_eq!(videos[0].url, "https://vimeo.com/300000001");
    assert_eq!(videos[0].description, None, "an empty description is none");
    assert_eq!(
        videos[1].published_at.unwrap().to_rfc3339(),
        "2020-06-14T12:00:00+00:00"
    );
    assert_eq!(videos[1].raw["talks_page"], page);
}

#[tokio::test]
async fn a_disallowed_talks_page_is_never_requested() {
    let server = MockServer::start().await;
    mount_site(&server, Some("User-agent: *\nDisallow: /talks-page/\n")).await;
    Mock::given(method("GET"))
        .and(path("/talks-page/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
        .expect(0)
        .mount(&server)
        .await;

    let page = format!("{}/talks-page/", server.uri());
    let adapter =
        VimeoAdapter::new("fixture-vimeo", None, Some(&page), endpoints(&server)).unwrap();
    let err = adapter.discover(None).await.unwrap_err();
    assert!(matches!(err, IngestError::RobotsDisallowed(_)), "{err}");
    // MockServer verifies `.expect(0)` when it drops.
}

#[tokio::test]
async fn a_site_without_robots_txt_is_read_and_a_source_with_nothing_configured_is_refused() {
    let server = MockServer::start().await;
    mount_site(&server, None).await;
    Mock::given(method("GET"))
        .and(path("/talks-page/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<p>No talks yet.</p>"))
        .expect(1)
        .mount(&server)
        .await;
    let page = format!("{}/talks-page/", server.uri());
    let adapter =
        VimeoAdapter::new("fixture-vimeo", None, Some(&page), endpoints(&server)).unwrap();
    assert!(adapter.discover(None).await.unwrap().is_empty());

    let bare = VimeoAdapter::new("fixture-vimeo", None, None, endpoints(&server)).unwrap();
    assert!(matches!(
        bare.discover(None).await.unwrap_err(),
        IngestError::Misconfigured { .. }
    ));
}

#[tokio::test]
async fn the_completeness_check_reads_page_ids_and_needs_a_token_for_the_account_list() {
    let server = MockServer::start().await;
    mount_site(&server, Some("User-agent: *\nDisallow: /wp-admin/\n")).await;
    Mock::given(method("GET"))
        .and(path("/talks-page/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<div data-video_id="400000002"></div><a href="https://vimeo.com/400000001">x</a>
               <div data-video_id="400000002"></div>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;
    // No oEmbed request is made: listing the page's ids describes nothing.
    Mock::given(method("GET"))
        .and(path("/api/oembed.json"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let page = format!("{}/talks-page/", server.uri());
    let adapter =
        VimeoAdapter::new("fixture-vimeo", None, Some(&page), endpoints(&server)).unwrap();
    assert_eq!(
        adapter.talks_page_ids().await.unwrap(),
        ["400000002", "400000001"]
    );
    assert_eq!(
        adapter.account_videos().await.unwrap(),
        None,
        "no token, no account list"
    );
}
