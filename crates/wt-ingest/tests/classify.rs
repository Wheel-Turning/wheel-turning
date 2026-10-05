//! Discovery and classification against a real database, with a synthetic corpus served by a
//! fake adapter: planted duplicates, an 11-second clip, a title-variant duplicate, a differently
//! dated near-duplicate, series parts, review holds and speakers (Spec 03, M3 acceptance).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::types::chrono::{DateTime, Utc};
use wt_db::job::{self, JobKind};
use wt_db::test_support::TestDb;
use wt_ingest::config::{ClassifyConfig, SourcesFile};
use wt_ingest::pipeline::{self, Classified, DiscoveryReport};
use wt_ingest::{DiscoveredVideo, IngestError, SourceAdapter, SourcePlatform};
use wt_registry::RegistryFile;

const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Mountain Zendo"
donation_url = "https://example.invalid/give"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Mountain Zendo"
aliases = ["Ada"]

[[teacher]]
display_name = "Bo Sample"
sangha = "Fixture Mountain Zendo"
aliases = ["Bo"]
"#;

const SOURCES: &str = r#"
[[source]]
name = "fixture-vimeo"
platform = "vimeo"
default_sangha = "Fixture Mountain Zendo"
"#;

const ACTOR: &str = "test";

/// Serves a fixed list, which a test can change between runs.
struct FakeAdapter(Mutex<Vec<DiscoveredVideo>>);

#[async_trait]
impl SourceAdapter for FakeAdapter {
    fn platform(&self) -> SourcePlatform {
        SourcePlatform::Vimeo
    }
    async fn discover(
        &self,
        _: Option<DateTime<Utc>>,
    ) -> Result<Vec<DiscoveredVideo>, IngestError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

fn video(id: &str, title: &str, duration: i32, at: &str, captions: bool) -> DiscoveredVideo {
    DiscoveredVideo {
        external_id: id.to_owned(),
        url: format!("https://vimeo.com/{id}"),
        title: title.to_owned(),
        description: None,
        published_at: Some(at.parse().unwrap()),
        duration_s: Some(duration),
        channel_ref: None,
        has_captions: captions,
        raw: serde_json::json!({ "fixture": true }),
    }
}

/// The synthetic corpus: (external id, title, seconds, published, captions).
fn corpus() -> Vec<DiscoveredVideo> {
    [
        (
            "1001",
            "Ada: Uji, Part 4",
            2400,
            "2021-03-07T18:00:00Z",
            false,
        ),
        (
            "1002",
            "Uji Part 4 (Ada) – re-upload",
            2410,
            "2021-03-08T09:00:00Z",
            false,
        ),
        ("1003", "Buddhist Quote", 11, "2021-03-09T18:00:00Z", false),
        (
            "1004",
            "Morning remarks",
            120,
            "2021-03-10T18:00:00Z",
            false,
        ),
        (
            "1005",
            "Ada — Facing the Jewel Mirror",
            2700,
            "2026-01-18T18:00:00Z",
            false,
        ),
        (
            "1006",
            "Ada: Facing the Jewel Mirror",
            2650,
            "2026-01-18T20:00:00Z",
            true,
        ),
        (
            "1007",
            "Ada: Facing the Jewel Mirror, continued",
            2690,
            "2026-01-25T18:00:00Z",
            false,
        ),
        (
            "1008",
            "Ada: Genjokoan Part 3",
            2400,
            "2022-02-06T18:00:00Z",
            false,
        ),
        (
            "1009",
            "Ada: Genjokoan Part 4",
            2410,
            "2022-02-06T20:00:00Z",
            false,
        ),
        ("1010", "zoom_0", 2700, "2020-05-31T18:00:00Z", false),
        ("1011", "Q&A with Ada", 2000, "2020-06-14T18:00:00Z", false),
        (
            "1012",
            "A talk on sitting",
            2400,
            "2020-07-05T18:00:00Z",
            false,
        ),
        (
            "1013",
            "Ada and Bo in dialogue",
            2400,
            "2020-07-12T18:00:00Z",
            false,
        ),
        (
            "1014",
            "Antiracist 12 Steps",
            5400,
            "2021-09-01T18:00:00Z",
            false,
        ),
    ]
    .into_iter()
    .map(|(id, t, d, at, c)| video(id, t, d, at, c))
    .collect()
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
    let registry = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &registry, ACTOR)
        .await
        .unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let cfg = ClassifyConfig::load(&root.join("pipeline.toml"), &root.join("series.toml")).unwrap();
    Fx {
        _db: db,
        pool,
        cfg,
        sources: SourcesFile::parse(SOURCES, "fixture").unwrap(),
    }
}

async fn discover(fx: &Fx, adapter: &FakeAdapter) -> DiscoveryReport {
    let mut conn = fx.pool.acquire().await.unwrap();
    pipeline::discover(
        &mut conn,
        fx.sources.get("fixture-vimeo").unwrap(),
        adapter,
        None,
    )
    .await
    .unwrap()
}

/// Runs every queued classify job, as a worker would. Returns the outcome per external id.
async fn classify_all(fx: &Fx) -> BTreeMap<String, Classified> {
    let mut conn = fx.pool.acquire().await.unwrap();
    let mut out = BTreeMap::new();
    while let Some(j) = job::claim(&mut conn, &[JobKind::Classify], "w")
        .await
        .unwrap()
    {
        let outcome = pipeline::run_classify_job(&mut conn, &j, &fx.cfg, &fx.sources, ACTOR)
            .await
            .unwrap();
        let id: String = sqlx::query_scalar(
            "select sv.external_id from talk t join source_video sv on sv.id = t.source_video_id
             where t.id = $1",
        )
        .bind(j.talk_id.unwrap().as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        out.insert(id, outcome);
    }
    out
}

#[derive(Debug, PartialEq)]
struct Row {
    state: String,
    reason: Option<String>,
    talk_type: String,
    duplicate_of: Option<String>,
    series: Option<String>,
    part: Option<i32>,
    teacher: Option<String>,
}

/// external id, state, reason, type, duplicate of, series, part, teacher.
type RowTuple = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<i32>,
    Option<String>,
);

async fn rows(pool: &PgPool) -> BTreeMap<String, Row> {
    let all: Vec<RowTuple> = sqlx::query_as(
        "select sv.external_id, t.state::text, t.state_reason, t.talk_type::text,
                    dsv.external_id, t.series, t.part, te.display_name
             from talk t
             join source_video sv on sv.id = t.source_video_id
             left join talk d on d.id = t.duplicate_of
             left join source_video dsv on dsv.id = d.source_video_id
             left join teacher te on te.id = t.teacher_id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    all.into_iter()
        .map(
            |(id, state, reason, talk_type, duplicate_of, series, part, teacher)| {
                (
                    id,
                    Row {
                        state,
                        reason,
                        talk_type,
                        duplicate_of,
                        series,
                        part,
                        teacher,
                    },
                )
            },
        )
        .collect()
}

#[tokio::test]
async fn the_synthetic_corpus_classifies_with_the_expected_exclusions_and_no_false_merges() {
    let fx = start().await;
    let adapter = FakeAdapter(Mutex::new(corpus()));
    let report = discover(&fx, &adapter).await;
    assert_eq!(
        report,
        DiscoveryReport {
            listed: 14,
            new: 14,
            ..DiscoveryReport::default()
        }
    );
    let outcomes = classify_all(&fx).await;
    assert_eq!(outcomes.len(), 14, "one classify job per talk");

    let rows = rows(&fx.pool).await;
    let get = |id: &str| &rows[id];
    let state = |id: &str| (get(id).state.as_str(), get(id).reason.as_deref());

    // Rule 2: the title pattern, the duration and the excluded session each give their reason.
    assert_eq!(state("1003"), ("excluded", Some("non_talk")));
    assert_eq!(get("1003").talk_type, "non_talk");
    assert_eq!(state("1004"), ("excluded", Some("too_short")));
    assert_eq!(get("1004").talk_type, "non_talk");
    assert_eq!(state("1014"), ("excluded", Some("excluded_session")));

    // Rule 3: a title-variant re-upload a day later merges into the earlier upload.
    assert_eq!(state("1002"), ("excluded", Some("duplicate")));
    assert_eq!(get("1002").duplicate_of.as_deref(), Some("1001"));
    assert_eq!(state("1001"), ("awaiting_consent", Some("consent_missing")));
    // The planted duplicate pair keeps the one with captions, though it was published later.
    assert_eq!(state("1005"), ("excluded", Some("duplicate")));
    assert_eq!(get("1005").duplicate_of.as_deref(), Some("1006"));
    assert_eq!(state("1006"), ("awaiting_consent", Some("consent_missing")));
    // No false merges: a week later with a different title, and two parts recorded the same day.
    assert_eq!(state("1007"), ("awaiting_consent", Some("consent_missing")));
    assert_eq!(state("1008"), ("awaiting_consent", Some("consent_missing")));
    assert_eq!(state("1009"), ("awaiting_consent", Some("consent_missing")));
    let merged: Vec<_> = rows
        .iter()
        .filter(|(_, r)| r.duplicate_of.is_some())
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(merged, ["1002", "1005"], "exactly the planted duplicates");

    // Rule 4: series and part.
    assert_eq!(
        (get("1001").series.as_deref(), get("1001").part),
        (Some("Uji"), Some(4))
    );
    assert_eq!(
        (get("1008").series.as_deref(), get("1008").part),
        (Some("Genjokoan"), Some(3))
    );
    assert_eq!(
        (get("1009").series.as_deref(), get("1009").part),
        (Some("Genjokoan"), Some(4))
    );

    // Rule 5: untitled and Q&A recordings are held whatever their length.
    assert_eq!(state("1010"), ("needs_review", Some("untitled")));
    assert_eq!(state("1011"), ("needs_review", Some("student_voices")));
    assert_eq!(get("1001").talk_type, "dharma_talk");

    // Rule 6: one alias resolves; none or two hold the talk.
    assert_eq!(get("1001").teacher.as_deref(), Some("Ada Fixture"));
    assert_eq!(state("1012"), ("needs_review", Some("speaker_unresolved")));
    assert_eq!(state("1013"), ("needs_review", Some("speaker_unresolved")));
    assert_eq!(
        outcomes["1013"],
        Classified::NeedsReview("speaker_unresolved".into())
    );

    // Every merge and exclusion is logged.
    let logged: i64 = sqlx::query_scalar(
        "select count(*) from correction_log where entity = 'talk' and new_value->>'state' = 'excluded'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(logged, 5);
}

#[tokio::test]
async fn rediscovery_is_a_no_op_and_changed_metadata_requeues_only_that_talk() {
    let fx = start().await;
    let adapter = FakeAdapter(Mutex::new(corpus()));
    discover(&fx, &adapter).await;
    classify_all(&fx).await;

    let again = discover(&fx, &adapter).await;
    assert_eq!(
        again,
        DiscoveryReport {
            listed: 14,
            unchanged: 14,
            ..DiscoveryReport::default()
        }
    );
    assert!(
        classify_all(&fx).await.is_empty(),
        "no job queued for unchanged input"
    );

    // The unresolved talk gets a speaker in its title upstream.
    adapter.0.lock().unwrap()[11].title = "Ada: A talk on sitting".to_owned();
    let changed = discover(&fx, &adapter).await;
    assert_eq!((changed.changed, changed.unchanged), (1, 13));
    let outcomes = classify_all(&fx).await;
    assert_eq!(outcomes.keys().collect::<Vec<_>>(), ["1012"]);
    assert_eq!(outcomes["1012"], Classified::AwaitingConsent);
}

#[tokio::test]
async fn a_withdrawn_talk_is_never_requeued() {
    let fx = start().await;
    let adapter = FakeAdapter(Mutex::new(corpus()));
    discover(&fx, &adapter).await;
    classify_all(&fx).await;
    sqlx::query(
        "update talk set state = 'withdrawn', state_reason = 'takedown'
         where source_video_id = (select id from source_video where external_id = '1001')",
    )
    .execute(&fx.pool)
    .await
    .unwrap();

    adapter.0.lock().unwrap()[0].title = "Ada: Uji, Part 4 (remastered)".to_owned();
    let report = discover(&fx, &adapter).await;
    assert_eq!(report.withdrawn, 1);
    assert!(classify_all(&fx).await.is_empty());
    assert_eq!(rows(&fx.pool).await["1001"].state, "withdrawn");
}
