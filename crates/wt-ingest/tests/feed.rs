//! An `audio_only` podcast feed against a real database: episodes match Vimeo talks by title,
//! date and length, the feed never creates a talk, and a matched talk's media route is the
//! episode's enclosure (Spec 03, Seattle Soto Zen sources). Everything here is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::types::chrono::{DateTime, Utc};
use wt_core::ids::TalkId;
use wt_db::feed::{self, MediaRoute};
use wt_db::job::{self, JobKind};
use wt_db::test_support::TestDb;
use wt_ingest::config::{ClassifyConfig, SourcesFile};
use wt_ingest::pipeline::{self, FeedReport};
use wt_ingest::{DiscoveredVideo, IngestError, SourceAdapter, SourcePlatform};
use wt_registry::RegistryFile;

const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Mountain Zendo"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Mountain Zendo"
aliases = ["Ada"]
"#;

const SOURCES: &str = r#"
[[source]]
name = "fixture-vimeo"
platform = "vimeo"
default_sangha = "Fixture Mountain Zendo"

[[source]]
name = "fixture-podcast"
platform = "rss"
default_sangha = "Fixture Mountain Zendo"
role = "audio_only"
feed_url = "https://example.invalid/feed.xml"
"#;

struct Fake(SourcePlatform, Mutex<Vec<DiscoveredVideo>>);

#[async_trait]
impl SourceAdapter for Fake {
    fn platform(&self) -> SourcePlatform {
        self.0
    }
    async fn discover(
        &self,
        _: Option<DateTime<Utc>>,
    ) -> Result<Vec<DiscoveredVideo>, IngestError> {
        Ok(self.1.lock().unwrap().clone())
    }
}

fn item(id: &str, title: &str, seconds: i32, at: &str, enclosure: Option<&str>) -> DiscoveredVideo {
    DiscoveredVideo {
        external_id: id.to_owned(),
        url: format!("https://example.invalid/{id}"),
        title: title.to_owned(),
        description: None,
        published_at: Some(at.parse().unwrap()),
        duration_s: Some(seconds),
        channel_ref: None,
        has_captions: false,
        raw: match enclosure {
            Some(url) => serde_json::json!({ "enclosure": { "url": url, "type": "audio/mpeg" } }),
            None => serde_json::json!({}),
        },
    }
}

struct Fx {
    _db: TestDb,
    pool: PgPool,
    cfg: ClassifyConfig,
    sources: SourcesFile,
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
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
    Fx {
        _db: db,
        pool,
        cfg: ClassifyConfig::load(&root.join("pipeline.toml"), &root.join("series.toml")).unwrap(),
        sources: SourcesFile::parse(SOURCES, "fixture").unwrap(),
    }
}

async fn talks(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from talk")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn talk_of(pool: &PgPool, vimeo_id: &str) -> TalkId {
    let id: sqlx::types::Uuid = sqlx::query_scalar(
        "select t.id from talk t join source_video sv on sv.id = t.source_video_id
         where sv.platform = 'vimeo' and sv.external_id = $1",
    )
    .bind(vimeo_id)
    .fetch_one(pool)
    .await
    .unwrap();
    TalkId::from(id)
}

async fn run_feed(fx: &Fx, feed: &Fake) -> FeedReport {
    let mut conn = fx.pool.acquire().await.unwrap();
    pipeline::discover_feed(
        &mut conn,
        &fx.cfg,
        fx.sources.get("fixture-podcast").unwrap(),
        feed,
        &["fixture-vimeo".to_owned()],
        "test",
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn feed_episodes_attach_audio_to_vimeo_talks_and_create_none() {
    let fx = start().await;
    let vimeo = Fake(
        SourcePlatform::Vimeo,
        Mutex::new(vec![
            item(
                "700000001",
                "Dharma Talk on Fixtures, by Ada on May 12, 2024",
                2420,
                "2024-05-14T18:00:00Z",
                None,
            ),
            item(
                "700000002",
                "Ada: A Long Talk",
                3700,
                "2024-05-06T18:00:00Z",
                None,
            ),
            item(
                "700000003",
                "Ada: A Talk Not in the Feed",
                2000,
                "2024-04-01T18:00:00Z",
                None,
            ),
            item(
                "700000004",
                "01-17-21 Sunday Dharma Talk: Ada",
                2500,
                "2021-01-20T18:00:00Z",
                None,
            ),
        ]),
    );
    {
        let mut conn = fx.pool.acquire().await.unwrap();
        pipeline::discover(
            &mut conn,
            fx.sources.get("fixture-vimeo").unwrap(),
            &vimeo,
            None,
        )
        .await
        .unwrap();
        while let Some(j) = job::claim(&mut conn, &[JobKind::Classify], "w")
            .await
            .unwrap()
        {
            pipeline::run_classify_job(&mut conn, &j, &fx.cfg, &fx.sources, "test")
                .await
                .unwrap();
        }
    }
    assert_eq!(talks(&fx.pool).await, 4);

    let podcast = Fake(
        SourcePlatform::Rss,
        Mutex::new(vec![
            // Different title words, but the same recording date and nearly the same length.
            item(
                "ep-1",
                "Fixtures (May 12, 2024)",
                2412,
                "2024-05-21T18:00:00Z",
                Some("https://example.invalid/1.mp3"),
            ),
            // The same title, published a day before the Vimeo upload, slightly longer.
            item(
                "ep-2",
                "A Long Talk",
                3734,
                "2024-05-05T18:00:00Z",
                Some("https://example.invalid/2.mp3"),
            ),
            // Nothing on Vimeo is this.
            item(
                "ep-3",
                "Some Other Recording",
                1500,
                "2024-03-01T18:00:00Z",
                Some("https://example.invalid/3.mp3"),
            ),
            // No length and a generic title, as in SSZ's feed: the audio file's name dates it.
            {
                let mut e = item(
                    "ep-4",
                    "Sunday Morning Dharma Talk (Ada)",
                    0,
                    "2021-01-18T18:00:00Z",
                    Some(
                        "https://example.invalid/AudioFiles/2021-01-17-Ada-Sunday-Talk.mp3?sig=2020-12-31",
                    ),
                );
                e.duration_s = None;
                e
            },
        ]),
    );
    let report = run_feed(&fx, &podcast).await;
    assert_eq!(
        report,
        FeedReport {
            listed: 4,
            new: 4,
            matched: 3,
            unmatched: 1,
            duplicate_uploads: 0
        }
    );
    assert_eq!(
        talks(&fx.pool).await,
        4,
        "an audio-only feed never creates a talk"
    );

    let mut conn = fx.pool.acquire().await.unwrap();
    let episodes = feed::episodes(&mut conn, "fixture-podcast").await.unwrap();
    let matched = |id: &str| {
        episodes
            .iter()
            .find(|e| e.external_id == id)
            .unwrap()
            .matched_vimeo_id
            .clone()
    };
    assert_eq!(matched("ep-1").as_deref(), Some("700000001"));
    assert_eq!(matched("ep-2").as_deref(), Some("700000002"));
    assert_eq!(matched("ep-3"), None);
    assert_eq!(
        matched("ep-4").as_deref(),
        Some("700000004"),
        "dated by its audio file's name"
    );

    // A matched talk's audio comes from the enclosure; nothing comes from Vimeo.
    match feed::media_route(&mut conn, talk_of(&fx.pool, "700000001").await)
        .await
        .unwrap()
    {
        MediaRoute::FeedEnclosure { url, mime_type, .. } => {
            assert_eq!(url, "https://example.invalid/1.mp3");
            assert_eq!(mime_type.as_deref(), Some("audio/mpeg"));
        }
        MediaRoute::None => panic!("expected the enclosure"),
    }
    assert_eq!(
        feed::media_route(&mut conn, talk_of(&fx.pool, "700000003").await)
            .await
            .unwrap(),
        MediaRoute::None
    );
    drop(conn);

    // Running again changes nothing, and rediscovering Vimeo keeps the matches.
    assert_eq!(
        run_feed(&fx, &podcast).await,
        FeedReport {
            listed: 4,
            new: 0,
            matched: 0,
            unmatched: 1,
            duplicate_uploads: 0
        }
    );
    vimeo.1.lock().unwrap()[0].title =
        "Dharma Talk on Fixtures, by Ada on May 12, 2024 (remastered)".into();
    let mut conn = fx.pool.acquire().await.unwrap();
    pipeline::discover(
        &mut conn,
        fx.sources.get("fixture-vimeo").unwrap(),
        &vimeo,
        None,
    )
    .await
    .unwrap();
    let episodes = feed::episodes(&mut conn, "fixture-podcast").await.unwrap();
    assert_eq!(
        episodes
            .iter()
            .filter(|e| e.matched_vimeo_id.is_some())
            .count(),
        3
    );

    let logged: i64 = sqlx::query_scalar(
        "select count(*) from correction_log where entity = 'source_video' and field = 'match'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(logged, 3, "each match is logged once");
}

fn no_length(id: &str, title: &str, at: &str, file: &str) -> DiscoveredVideo {
    let mut e = item(
        id,
        title,
        0,
        at,
        Some(&format!("https://example.invalid/AudioFiles/{file}?sig=1")),
    );
    e.duration_s = None;
    e
}

async fn state_of(pool: &PgPool, vimeo_id: &str) -> (String, Option<String>, Option<String>) {
    sqlx::query_as(
        "select t.state::text, t.state_reason, d_sv.external_id
         from talk t join source_video sv on sv.id = t.source_video_id
         left join talk d on d.id = t.duplicate_of
         left join source_video d_sv on d_sv.id = d.source_video_id
         where sv.platform = 'vimeo' and sv.external_id = $1",
    )
    .bind(vimeo_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn an_episode_reveals_a_re_upload_under_a_new_title_and_matches_exact_titles() {
    let fx = start().await;
    let vimeo = Fake(
        SourcePlatform::Vimeo,
        Mutex::new(vec![
            // Dated uploads, then the same talks re-uploaded weeks later under real titles.
            item(
                "710000001",
                "Ada 04-12-20",
                1753,
                "2020-04-15T18:00:00Z",
                None,
            ),
            item(
                "710000002",
                "Ada 2020-05-10",
                2874,
                "2020-05-12T18:00:00Z",
                None,
            ),
            item(
                "710000003",
                "Who Is Ill?",
                1753,
                "2020-05-17T18:00:00Z",
                None,
            ),
            // A re-upload trimmed by ten minutes: length cannot tie it to its dated copy.
            item(
                "710000004",
                "Mother's Day Fixture",
                2280,
                "2020-05-17T19:00:00Z",
                None,
            ),
            // Titled only, no other copy.
            item(
                "710000005",
                "What is the Point?",
                2382,
                "2020-07-08T18:00:00Z",
                None,
            ),
            // One generic title on two different talks.
            item(
                "710000006",
                "Sunday Talk",
                2400,
                "2020-08-02T18:00:00Z",
                None,
            ),
            item(
                "710000007",
                "Sunday Talk",
                2600,
                "2020-08-09T18:00:00Z",
                None,
            ),
            // A full recording and an excerpt of one day: dated alike, never proven one upload.
            item(
                "710000008",
                "2020-06-14-Ada-Full",
                2782,
                "2020-06-14T18:00:00Z",
                None,
            ),
            item(
                "710000009",
                "2020-06-14-Ada-Zen-is-Fixture",
                2469,
                "2020-06-14T19:00:00Z",
                None,
            ),
        ]),
    );
    {
        let mut conn = fx.pool.acquire().await.unwrap();
        pipeline::discover(
            &mut conn,
            fx.sources.get("fixture-vimeo").unwrap(),
            &vimeo,
            None,
        )
        .await
        .unwrap();
        while let Some(j) = job::claim(&mut conn, &[JobKind::Classify], "w")
            .await
            .unwrap()
        {
            pipeline::run_classify_job(&mut conn, &j, &fx.cfg, &fx.sources, "test")
                .await
                .unwrap();
        }
    }
    let podcast = Fake(
        SourcePlatform::Rss,
        Mutex::new(vec![
            no_length(
                "ep-a",
                "Who Is Ill? (Ada Fixture)",
                "2020-04-12T18:00:00Z",
                "2020_04_12_Ada.mp3",
            ),
            no_length(
                "ep-b",
                "Mother's Day Fixture (Ada Fixture)",
                "2020-05-10T18:00:00Z",
                "2020_05_10_Ada.mp3",
            ),
            no_length(
                "ep-c",
                "What is the Point? (Ada Fixture)",
                "2020-07-05T18:00:00Z",
                "point.mp3",
            ),
            no_length(
                "ep-d",
                "Sunday Talk (Ada Fixture)",
                "2020-08-02T18:00:00Z",
                "sunday.mp3",
            ),
            no_length(
                "ep-e",
                "Zen is Fixture (Ada Fixture)",
                "2020-06-14T18:00:00Z",
                "2020_06_14_Ada.mp3",
            ),
        ]),
    );

    let report = run_feed(&fx, &podcast).await;
    assert_eq!(
        report,
        FeedReport {
            listed: 5,
            new: 5,
            matched: 4,
            unmatched: 1,
            duplicate_uploads: 2
        }
    );
    // The dated copies were uploaded first, so rule 3's ranking keeps them.
    assert_eq!(
        state_of(&fx.pool, "710000003").await,
        (
            "excluded".into(),
            Some("duplicate_upload".into()),
            Some("710000001".into())
        )
    );
    assert_eq!(
        state_of(&fx.pool, "710000004").await,
        (
            "excluded".into(),
            Some("duplicate_upload".into()),
            Some("710000002".into())
        ),
        "the trimmed re-upload too"
    );
    let mut conn = fx.pool.acquire().await.unwrap();
    let episodes = feed::episodes(&mut conn, "fixture-podcast").await.unwrap();
    let matched = |id: &str| {
        episodes
            .iter()
            .find(|e| e.external_id == id)
            .unwrap()
            .matched_vimeo_id
            .clone()
    };
    assert_eq!(matched("ep-a").as_deref(), Some("710000001"));
    assert_eq!(matched("ep-b").as_deref(), Some("710000002"));
    assert_eq!(matched("ep-c").as_deref(), Some("710000005"), "exact title");
    assert_eq!(
        matched("ep-d"),
        None,
        "a title two talks share matches neither"
    );
    assert_eq!(matched("ep-e").as_deref(), Some("710000009"));
    for id in [
        "710000005",
        "710000006",
        "710000007",
        "710000008",
        "710000009",
    ] {
        assert_ne!(
            state_of(&fx.pool, id).await.1.as_deref(),
            Some("duplicate_upload"),
            "{id}"
        );
    }

    // Reclassify never reopens what the feed proved, and the feed does not merge twice.
    let report = pipeline::reclassify(&mut conn, "next-rules", None, false)
        .await
        .unwrap();
    assert_eq!(
        report.eligible, 7,
        "the two feed-proven duplicates are not eligible"
    );
    while let Some(j) = job::claim(&mut conn, &[JobKind::Classify], "w")
        .await
        .unwrap()
    {
        pipeline::run_classify_job(&mut conn, &j, &fx.cfg, &fx.sources, "test")
            .await
            .unwrap();
    }
    drop(conn);
    assert_eq!(
        state_of(&fx.pool, "710000003").await.1.as_deref(),
        Some("duplicate_upload")
    );
    assert_eq!(
        run_feed(&fx, &podcast).await,
        FeedReport {
            listed: 5,
            new: 0,
            matched: 0,
            unmatched: 1,
            duplicate_uploads: 0
        }
    );
}
