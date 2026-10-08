//! EngageWisdom sources (Spec 03): discovery from synthetic listing and talk pages on a local
//! server, then classification resolving each talk's speaker from its byline alone.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use sqlx::PgPool;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_db::job::{self, JobKind};
use wt_db::test_support::TestDb;
use wt_ingest::config::{ClassifyConfig, SourcesFile};
use wt_ingest::engagewisdom::EngageWisdomAdapter;
use wt_ingest::http::Exclusions;
use wt_ingest::pipeline::{self, Classified};
use wt_ingest::source::SourceAdapter;
use wt_registry::RegistryFile;

const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Zen Center"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Zen Center"
aliases = ["Ada"]
"#;

fn talk_page(serial: &str, title: &str, speakers: &[&str], transcript: &str) -> String {
    let byline: Vec<String> = speakers
        .iter()
        .map(|s| {
            format!(
                r#"<a href="/speaker/{}">{s}</a>"#,
                s.to_lowercase().replace(' ', "-")
            )
        })
        .collect();
    format!(
        r#"<html><body>
<div class="panel-pane pane-views pane-ew-talk-title-pane"><div class="pane-content"><div class="view"><div class="view-content"><div class="views-row"> <h3>{title}</h3> </div></div></div></div></div>
<div class="panel-pane pane-byline talk-head"  >  <div class="pane-content">   Given by {} at <a href="/location/fixture-hall">Fixture Hall</a> on <strong>March 3rd, 2019</strong>  </div> </div>
<div class="field field-name-field-serial-number field-type-text field-label-inline clearfix field-wrapper"><div  class="field-label">Serial:&nbsp;</div>{serial}</div>
<div class="field field-name-field-ai-summary"><div class="field-label">AI Summary:&nbsp;</div><p>Machine summary text.</p></div>
<div class="panel-pane talk-transcript"><div class="pane-content"><div class="field field-name-field-wisdom-transcript field-type-text-long"><div  class="field-label">Transcript:&nbsp;</div><p>[00:00]</p><p>{transcript}</p> </div>  </div>       </div>
<script>{{"audiofield":{{"wavesurfer-player-id":"https:\/\/media.example.invalid\/s3fs-public\/audio\/{serial}.mp4"}}}}</script>
</body></html>"#,
        byline.join(" and ")
    )
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "User-agent: *\nCrawl-delay: 10\nDisallow: /search/\nDisallow: /talks/withheld\n",
        ))
        .mount(&server)
        .await;
    let listing = |links: &[&str]| {
        let rows: String = links
            .iter()
            .map(|l| {
                format!(r#"<tr><td><h3 class="title"><a href="/talks/{l}">{l}</a></h3></td></tr>"#)
            })
            .collect();
        format!("<html><body><table>{rows}</table></body></html>")
    };
    Mock::given(method("GET"))
        .and(path("/fx"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(listing(&["joint-talk"])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/fx"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(listing(&[])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/fx"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(listing(&["remembering-ada", "the-pond"])),
        )
        .mount(&server)
        .await;
    for (slug, serial, title, speakers) in [
        (
            "remembering-ada",
            "FX-00001",
            "Remembering Ada Fixture",
            vec!["Bo Fixture"],
        ),
        ("the-pond", "FX-00002", "The Pond", vec!["Ada Fixture"]),
        (
            "joint-talk",
            "FX-00003",
            "A Joint Talk",
            vec!["Ada Fixture", "Bo Fixture"],
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/talks/{slug}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(talk_page(
                serial,
                title,
                &speakers,
                "The pond is still and clear this morning, and the moon rests on it.",
            )))
            .mount(&server)
            .await;
    }
    server
}

fn adapter(server: &MockServer, max: Option<usize>) -> EngageWisdomAdapter {
    EngageWisdomAdapter::new(&format!("{}/fx", server.uri()), max, Exclusions::default())
        .unwrap()
        .with_delay(Duration::ZERO)
}

#[tokio::test]
async fn discovery_reads_every_listing_page_and_each_talk_page() {
    let server = server().await;
    let videos = adapter(&server, None).discover(None).await.unwrap();
    let ids: Vec<&str> = videos.iter().map(|v| v.external_id.as_str()).collect();
    assert_eq!(ids, vec!["FX-00001", "FX-00002", "FX-00003"]);
    let first = &videos[0];
    assert_eq!(first.title, "Remembering Ada Fixture");
    assert_eq!(first.description.as_deref(), Some("Given by Bo Fixture"));
    assert_eq!(
        first.published_at.unwrap().date_naive().to_string(),
        "2019-03-03"
    );
    assert_eq!(first.raw["location"], "Fixture Hall");
    assert_eq!(
        first.raw["audio_url"],
        "https://media.example.invalid/s3fs-public/audio/FX-00001.mp4"
    );
    assert!(
        first.raw["transcript"]
            .as_str()
            .unwrap()
            .contains("the moon rests on it")
    );
    assert!(
        !first.raw.to_string().contains("Machine summary"),
        "the AI summary is never read"
    );
    assert_eq!(
        videos[2].raw["speakers"],
        serde_json::json!(["Ada Fixture", "Bo Fixture"])
    );

    // A sample stops at its limit and reads no further listing page.
    let sample = adapter(&server, Some(1)).discover(None).await.unwrap();
    assert_eq!(sample.len(), 1);
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| !r.url.path().starts_with("/search/"))
    );
}

#[tokio::test]
async fn the_crawl_delay_is_kept_between_requests() {
    let server = server().await;
    let started = std::time::Instant::now();
    adapter(&server, Some(2))
        .with_delay(Duration::from_millis(100))
        .discover(None)
        .await
        .unwrap();
    // A listing page and two talk pages, 100 ms apart.
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "{:?}",
        started.elapsed()
    );
}

struct Fx {
    _db: TestDb,
    pool: PgPool,
    cfg: ClassifyConfig,
    sources: SourcesFile,
}

async fn start(server: &MockServer) -> Fx {
    let db = TestDb::start().await.expect("docker must be running");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    wt_db::registry::import(
        &pool,
        &RegistryFile::parse(REGISTRY, "fixture").unwrap(),
        "test",
    )
    .await
    .unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let cfg = ClassifyConfig::load(&root.join("pipeline.toml"), &root.join("series.toml")).unwrap();
    let sources = SourcesFile::parse(
        &format!(
            r#"[[source]]
name = "fixture-ew"
platform = "engagewisdom"
collection_url = "{}/fx"
default_sangha = "Fixture Zen Center"
link_style = "engagewisdom"
default_talk_type = "dharma_talk"
"#,
            server.uri()
        ),
        "fixture",
    )
    .unwrap();
    Fx {
        _db: db,
        pool,
        cfg,
        sources,
    }
}

#[tokio::test]
async fn speakers_become_teachers_and_resolve_from_the_byline_alone() {
    let server = server().await;
    let fx = start(&server).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let source = fx.sources.get("fixture-ew").unwrap();
    let report = pipeline::discover(&mut conn, source, &adapter(&server, None), None)
        .await
        .unwrap();
    assert_eq!(report.new, 3);
    let teachers: Vec<String> =
        sqlx::query_scalar("select display_name from teacher order by display_name")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        teachers,
        vec!["Ada Fixture", "Bo Fixture"],
        "a new speaker became a teacher"
    );

    let mut outcomes = BTreeMap::new();
    while let Some(j) = job::claim(&mut conn, &[JobKind::Classify], "w")
        .await
        .unwrap()
    {
        let outcome = pipeline::run_classify_job(&mut conn, &j, &fx.cfg, &fx.sources, "test")
            .await
            .unwrap();
        let (serial, teacher, reason): (String, Option<String>, Option<String>) = sqlx::query_as(
            "select sv.external_id, te.display_name, t.state_reason
             from talk t join source_video sv on sv.id = t.source_video_id
             left join teacher te on te.id = t.teacher_id where t.id = $1",
        )
        .bind(j.talk_id.unwrap().as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        outcomes.insert(serial, (outcome, teacher, reason));
    }
    // The title names Ada, but the byline says Bo, and the byline decides.
    assert_eq!(outcomes["FX-00001"].1.as_deref(), Some("Bo Fixture"));
    assert_eq!(outcomes["FX-00002"].1.as_deref(), Some("Ada Fixture"));
    assert!(
        matches!(outcomes["FX-00003"].0, Classified::NeedsReview(_)),
        "{:?}",
        outcomes["FX-00003"]
    );
    assert_eq!(
        outcomes["FX-00003"].2.as_deref(),
        Some("speaker_unresolved")
    );
}
