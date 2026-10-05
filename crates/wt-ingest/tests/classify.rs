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
use wt_ingest::report;
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
            "A talk on sitting by Cy Example",
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
    .map(|(id, t, d, at, c)| {
        let mut v = video(id, t, d, at, c);
        if id == "1001" {
            v.description = Some(
                "Recorded at the zendo. Support the sangha: https://example.invalid/donate.                  Schedule: https://example.invalid/calendar"
                    .to_owned(),
            );
        }
        v
    })
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
    classify_all_with(fx, &fx.cfg).await
}

async fn classify_all_with(fx: &Fx, cfg: &ClassifyConfig) -> BTreeMap<String, Classified> {
    let mut conn = fx.pool.acquire().await.unwrap();
    let mut out = BTreeMap::new();
    while let Some(j) = job::claim(&mut conn, &[JobKind::Classify], "w")
        .await
        .unwrap()
    {
        let outcome = pipeline::run_classify_job(&mut conn, &j, cfg, &fx.sources, ACTOR)
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

#[tokio::test]
async fn the_report_renders_every_section_from_the_fixture_corpus() {
    let fx = start().await;
    let adapter = FakeAdapter(Mutex::new(corpus()));
    discover(&fx, &adapter).await;
    classify_all(&fx).await;

    let mut conn = fx.pool.acquire().await.unwrap();
    let rows = wt_db::ingest::report_rows(&mut conn).await.unwrap();
    let report = report::build(
        &rows,
        &fx.cfg.series,
        "2026-10-05T12:00:00Z".parse().unwrap(),
    );

    assert_eq!(report.totals.talks, 14);
    assert_eq!(report.totals.excluded, 5);

    // Duplicate clusters, kept record first.
    let kept: Vec<_> = report
        .duplicate_clusters
        .iter()
        .map(|c| c.kept.source.as_str())
        .collect();
    assert_eq!(kept, ["vimeo 1001", "vimeo 1006"]);
    assert_eq!(
        report.duplicate_clusters[1].duplicates[0].source,
        "vimeo 1005"
    );
    assert!(report.duplicate_clusters[1].kept.captions);

    // Exclusions other than duplicates, with reasons.
    let excluded: Vec<_> = report.excluded.iter().map(|i| i.status.as_str()).collect();
    assert_eq!(
        excluded,
        [
            "excluded: non_talk",
            "excluded: too_short",
            "excluded: excluded_session"
        ]
    );

    // Review holds with reasons and the speaker candidates.
    let review: BTreeMap<_, _> = report
        .needs_review
        .iter()
        .map(|i| (i.source.as_str(), (i.status.as_str(), i.speaker.as_deref())))
        .collect();
    assert_eq!(review["vimeo 1010"].0, "needs_review: untitled");
    assert_eq!(review["vimeo 1011"].0, "needs_review: student_voices");
    assert_eq!(
        review["vimeo 1013"],
        ("needs_review: speaker_unresolved", Some("[Ada, Bo]"))
    );

    // Series coverage shows the missing parts.
    let genjokoan = report
        .series
        .iter()
        .find(|s| s.name == "Genjokoan")
        .unwrap();
    assert_eq!(genjokoan.parts.keys().copied().collect::<Vec<_>>(), [3, 4]);
    assert_eq!(genjokoan.missing, [1, 2]);
    let uji = report.series.iter().find(|s| s.name == "Uji").unwrap();
    assert_eq!(
        (uji.parts[&4].len(), uji.missing.clone()),
        (1, vec![1, 2, 3]),
        "the duplicate is not counted"
    );
    let bendowa = report.series.iter().find(|s| s.name == "Bendowa").unwrap();
    assert!(
        bendowa.parts.is_empty() && bendowa.missing.is_empty(),
        "a series with no talks yet"
    );

    // Month by month, with the gaps between uploads.
    assert_eq!(report.months.first().unwrap().month, "2020-05");
    assert_eq!(report.months.last().unwrap().month, "2026-01");
    assert_eq!(report.months.len(), 69, "every month from first to last");
    let march_2021 = report.months.iter().find(|m| m.month == "2021-03").unwrap();
    assert_eq!(
        (march_2021.videos, march_2021.talks),
        (4, 1),
        "two clips and a duplicate drop out"
    );
    assert!(
        report
            .month_gaps
            .contains(&("2022-03".to_owned(), "2025-12".to_owned()))
    );

    // Donation-looking links, listed for a person, and only those.
    let links: Vec<_> = report
        .donation_links
        .iter()
        .map(|l| l.url.as_str())
        .collect();
    assert_eq!(links, ["https://example.invalid/donate"]);

    let html = report::render_html(&report);
    for id in [
        "duplicates",
        "excluded",
        "review",
        "series",
        "months",
        "donation-links",
    ] {
        assert!(html.contains(&format!("id=\"{id}\"")), "section {id}");
    }
    assert!(html.contains("<tr class=\"kept\">"));
    assert!(html.contains("2022-03 to 2025-12"));
    let lower = html.to_lowercase();
    assert!(
        !lower.contains("<script") && !lower.contains(" src=") && !lower.contains("<link"),
        "the page loads nothing from anywhere"
    );

    // Building the report changed nothing.
    let again = wt_db::ingest::report_rows(&mut conn).await.unwrap();
    assert_eq!(again, rows);
}

#[tokio::test]
async fn the_database_normalizes_titles_exactly_as_rule_1_does() {
    let fx = start().await;
    let titles = [
        "2021-11-03_Fixture_Sunday_Shobogenzo-Uji-Being-Time_Part-3.mp4",
        "Q&A: Zazen — “Quoted” words!",
        "Genjōkōan  Paragraph	4 (5-09-2021)",
        "ﬁrst Talk, again",
    ];
    let adapter = FakeAdapter(Mutex::new(
        titles
            .iter()
            .enumerate()
            .map(|(i, t)| video(&format!("77{i}"), t, 2400, "2024-01-01T00:00:00Z", false))
            .collect(),
    ));
    discover(&fx, &adapter).await;
    let probe = video("7799", "probe", 2400, "2024-01-01T00:00:00Z", false);
    adapter.0.lock().unwrap().push(probe);
    discover(&fx, &adapter).await;
    let probe_id: sqlx::types::Uuid = sqlx::query_scalar(
        "select t.id from talk t join source_video sv on sv.id = t.source_video_id
         where sv.external_id = '7799'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();

    let mut conn = fx.pool.acquire().await.unwrap();
    let candidates = wt_db::ingest::duplicate_candidates(
        &mut conn,
        wt_core::ids::TalkId::from(probe_id),
        "probe",
        2400,
        10,
    )
    .await
    .unwrap();
    let from_sql: std::collections::BTreeSet<String> =
        candidates.into_iter().map(|c| c.normalized_title).collect();
    let from_rust: std::collections::BTreeSet<String> = titles
        .iter()
        .map(|t| wt_ingest::classify::normalize_title(t))
        .collect();
    assert_eq!(from_sql, from_rust);
}

fn config_text(name: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config");
    std::fs::read_to_string(root.join(name)).unwrap()
}

async fn reclassify(fx: &Fx, pipeline_toml: &str, dry_run: bool) -> pipeline::ReclassifyReport {
    let mut conn = fx.pool.acquire().await.unwrap();
    let aliases = wt_db::ingest::alias_list(&mut conn).await.unwrap();
    let version = pipeline::rules_version(pipeline_toml, &config_text("series.toml"), &aliases);
    pipeline::reclassify(&mut conn, &version, None, dry_run)
        .await
        .unwrap()
}

#[tokio::test]
async fn reclassify_applies_changed_rules_once_and_leaves_withdrawn_talks_alone() {
    let fx = start().await;
    let adapter = FakeAdapter(Mutex::new(corpus()));
    discover(&fx, &adapter).await;

    // First pass under a mistaken rule that drops every "jewel" talk as a non-talk.
    let good = config_text("pipeline.toml");
    let wrong = good.replace(
        r"exclude_title_patterns = ['\bquote\b'",
        r"exclude_title_patterns = ['\bjewel\b', '\bquote\b'",
    );
    assert_ne!(wrong, good, "the mistaken rule was planted");
    let wrong_cfg =
        ClassifyConfig::parse(&wrong, "wrong", &config_text("series.toml"), "series").unwrap();
    classify_all_with(&fx, &wrong_cfg).await;
    let before = rows(&fx.pool).await;
    for id in ["1005", "1006", "1007"] {
        assert_eq!(
            before[id].reason.as_deref(),
            Some("non_talk"),
            "{id} wrongly excluded"
        );
    }
    assert_eq!(before["1012"].reason.as_deref(), Some("speaker_unresolved"));
    sqlx::query(
        "update talk set state = 'withdrawn', state_reason = 'takedown'
         where source_video_id = (select id from source_video where external_id = '1009')",
    )
    .execute(&fx.pool)
    .await
    .unwrap();

    // The rule is corrected and the registry learns a new teacher.
    let more = RegistryFile::parse(
        r#"
[[sangha]]
name = "Fixture Mountain Zendo"

[[teacher]]
display_name = "Cy Example"
sangha = "Fixture Mountain Zendo"
aliases = ["Cy"]
"#,
        "more",
    )
    .unwrap();
    wt_db::registry::import(&fx.pool, &more, ACTOR)
        .await
        .unwrap();

    let dry = reclassify(&fx, &good, true).await;
    let queued: i64 = sqlx::query_scalar("select count(*) from job where state = 'queued'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(queued, 0, "a dry run queues nothing");

    let report = reclassify(&fx, &good, false).await;
    assert_eq!(report.queued, dry.queued);
    // Every talk but the withdrawn one.
    assert_eq!(
        (report.eligible, report.queued, report.already_done),
        (13, 13, 0)
    );
    classify_all(&fx).await;

    let after = rows(&fx.pool).await;
    let state = |id: &str| (after[id].state.as_str(), after[id].reason.as_deref());
    assert_eq!(
        state("1005"),
        ("excluded", Some("duplicate")),
        "the planted pair merges again"
    );
    assert_eq!(after["1005"].duplicate_of.as_deref(), Some("1006"));
    assert_eq!(state("1006"), ("awaiting_consent", Some("consent_missing")));
    assert_eq!(state("1007"), ("awaiting_consent", Some("consent_missing")));
    assert_eq!(
        after["1012"].teacher.as_deref(),
        Some("Cy Example"),
        "the new alias resolves"
    );
    assert_eq!(
        state("1003"),
        ("excluded", Some("non_talk")),
        "still a non-talk"
    );
    assert_eq!(
        state("1002"),
        ("excluded", Some("duplicate")),
        "still a duplicate"
    );
    assert_eq!(
        state("1010"),
        ("needs_review", Some("untitled")),
        "a person's hold stays"
    );
    assert_eq!(
        state("1009"),
        ("withdrawn", Some("takedown")),
        "never touched"
    );
    let reopened: i64 = sqlx::query_scalar(
        "select count(*) from correction_log where reason = 'reopened for reclassification'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        reopened, 7,
        "each classifier exclusion was reopened and logged"
    );

    let again = reclassify(&fx, &good, false).await;
    assert_eq!(
        (again.queued, again.already_done),
        (0, 13),
        "unchanged rules queue nothing"
    );
}

#[tokio::test]
async fn recording_dates_in_titles_set_talk_dates_and_catch_late_re_uploads() {
    let fx = start().await;
    let adapter = FakeAdapter(Mutex::new(
        [
            // One recording uploaded twice, 13 days apart, the second with a typo.
            (
                "2001",
                "2021-11-07_Ada_Sunday_Shobogenzo-Uji_Part-9.mp4",
                2718,
                "2021-11-13T18:00:00Z",
            ),
            (
                "2002",
                "2021-11-07_Sunday_Ada_Sbobogenzo_Uji_Part-9",
                2711,
                "2021-11-26T18:00:00Z",
            ),
            // One title, two talks a week apart, each uploaded later.
            (
                "2003",
                "Ada: Facing the Jewel Mirror, on January 18, 2026",
                2025,
                "2026-02-01T18:00:00Z",
            ),
            (
                "2004",
                "Ada: Facing the Jewel Mirror, on January 25, 2026",
                2062,
                "2026-02-06T18:00:00Z",
            ),
            // A title date after the upload is a typo: the upload date stands.
            (
                "2005",
                "Ada: Endings, on May 30, 2023",
                2400,
                "2023-05-10T18:00:00Z",
            ),
            // No date in the title: weekly talks with one generic title stay apart.
            (
                "2006",
                "Sunday Dharma Talk: Ada - Bendowa Series",
                1778,
                "2021-02-25T18:00:00Z",
            ),
            (
                "2007",
                "Sunday Dharma Talk: Ada - Bendowa Series",
                1751,
                "2021-03-04T18:00:00Z",
            ),
        ]
        .into_iter()
        .map(|(id, t, d, at)| video(id, t, d, at, false))
        .collect(),
    ));
    discover(&fx, &adapter).await;
    classify_all(&fx).await;

    let dates: BTreeMap<String, (Option<String>, String, Option<String>)> = sqlx::query_as(
        "select sv.external_id, t.talk_date::text, t.state::text, dsv.external_id
         from talk t join source_video sv on sv.id = t.source_video_id
         left join talk d on d.id = t.duplicate_of
         left join source_video dsv on dsv.id = d.source_video_id",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap()
    .into_iter()
    .map(
        |(id, date, state, dup): (String, Option<String>, String, Option<String>)| {
            (id, (date, state, dup))
        },
    )
    .collect();
    let date = |id: &str| dates[id].0.as_deref();

    assert_eq!(
        date("2001"),
        Some("2021-11-07"),
        "the recording date, not the upload"
    );
    assert_eq!(dates["2002"].1, "excluded");
    assert_eq!(
        dates["2002"].2.as_deref(),
        Some("2001"),
        "the late re-upload merges into the first"
    );
    assert_eq!(
        (date("2003"), date("2004")),
        (Some("2026-01-18"), Some("2026-01-25"))
    );
    assert_eq!(
        (dates["2003"].2.as_deref(), dates["2004"].2.as_deref()),
        (None, None)
    );
    assert_eq!(
        date("2005"),
        Some("2023-05-10"),
        "a future title date is ignored"
    );
    assert_eq!(
        (dates["2006"].2.as_deref(), dates["2007"].2.as_deref()),
        (None, None)
    );
    assert_eq!(date("2006"), Some("2021-02-25"));
}

#[tokio::test]
async fn talks_from_a_mandated_source_clear_in_classification_without_consent() {
    let mut fx = start().await;
    fx.sources = SourcesFile::parse(
        &format!("{SOURCES}rights_source = \"fixture-zendo-talks\"\n"),
        "fixture",
    )
    .unwrap();
    let adapter = FakeAdapter(Mutex::new(vec![
        video(
            "800000001",
            "Ada: Sitting with the Breath",
            2400,
            "2024-05-05T18:00:00Z",
            false,
        ),
        video(
            "800000002",
            "Sunday Dharma Talk: A Guest",
            2400,
            "2024-05-12T18:00:00Z",
            false,
        ),
    ]));

    // The source must exist in the rights ledger as a talks source.
    let mut conn = fx.pool.acquire().await.unwrap();
    let refused = pipeline::discover(
        &mut conn,
        fx.sources.get("fixture-vimeo").unwrap(),
        &adapter,
        None,
    )
    .await;
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("not a talks source"),
        "an unknown rights_source is refused before anything is recorded"
    );
    drop(conn);
    let decision = wt_core::records::StoredRecord {
        key: "records/fixture".into(),
        sha256: "fixture".into(),
    };
    wt_db::rights::add_source(
        &fx.pool,
        &wt_db::rights::NewSource {
            key: "fixture-zendo-talks",
            publisher: "Fixture Mountain Zendo",
            prefixes: &["https://vimeo.com/fixturezendo".to_owned()],
            kind: "talks",
            basis: wt_db::rights::NewBasis::Mandate {
                by: "Owner Fixture",
                rationale: "fixture",
                display_notice: "Talk by {teacher}, {sangha}.",
                decision: &decision,
            },
        },
    )
    .await
    .unwrap();
    wt_db::rights::set_talks_basis(&fx.pool, "mandate", "Owner Fixture")
        .await
        .unwrap();

    discover(&fx, &adapter).await;
    // The rights holder is known for Ada's talk; a talk still needs one under a mandate.
    sqlx::query(
        "update talk set rights_holder = 'Ada Fixture' where source_video_id =
           (select id from source_video where external_id = '800000001')",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    let outcomes = classify_all(&fx).await;
    assert_eq!(outcomes["800000001"], Classified::Cleared);
    assert!(
        matches!(&outcomes["800000002"], Classified::NeedsReview(_)),
        "an unresolved speaker still waits for a person: {:?}",
        outcomes["800000002"]
    );
    let (basis, source, consent_rows): (String, String, i64) = sqlx::query_as(
        "select t.rights_basis::text, t.source_key,
                (select count(*) from speaker_consent)
         from talk t join source_video sv on sv.id = t.source_video_id
         where sv.external_id = '800000001'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        (basis.as_str(), source.as_str(), consent_rows),
        ("mandate", "fixture-zendo-talks", 0)
    );
}
