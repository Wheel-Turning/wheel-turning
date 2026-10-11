//! Spec 04 end to end: a synthetic talk goes from `cleared` to `ready` through the job queue,
//! with fakes for recognition, diarization, conversion, the language model and the embedder,
//! a local server for the feed's audio host and a real database. Then every stage runs again
//! and no row changes. Every word is invented.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use serde_json::json;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_asr::{AsrHints, FakeTranscriber};
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_core::objects::{LocalObjects, ObjectStore};
use wt_core::scratch::{Converter, ScratchDir, ScratchError, WAV_NAME};
use wt_core::transcript::Word;
use wt_db::job::{self, JobKind};
use wt_db::test_support::TestDb;
use wt_extract::FakeLlm;
use wt_index::ChunkSettings;
use wt_ingest::audio::{self, AudioDeps, AudioOutcome};
use wt_ingest::http::Exclusions;
use wt_ingest::media::MediaFetcher;
use wt_ingest::textjobs::{self, PgHandList, TextOutcome, staging_key};
use wt_ml::FakeEmbedder;
use wt_registry::RegistryFile;
use wt_registry::glossary::{Glossary, GlossaryTerm, TermKind};
use wt_segment::{
    FakeDiarizer, FakeEditor, FixedLabeler, Label, QuestionEditor, SegmentSettings, Turn,
};

const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Mountain Zendo"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Mountain Zendo"
aliases = ["Ada"]
"#;

/// Invented sentences the teacher repeats.
const SENTENCES: [&str; 6] = [
    "This morning we sit together in the quiet hall.",
    "As doe gen wrote, to study the way is to study the self.",
    "To study the self is to forget the self.",
    "When the bell rings we return to the breath without hurry.",
    "Each of us brings a whole life to the cushion.",
    "Let the thinking come and go like weather over the hills.",
];

struct FakeConverter;

impl Converter for FakeConverter {
    fn to_wav(&self, dir: &ScratchDir, input: &Path) -> Result<PathBuf, ScratchError> {
        let path = dir.file(WAV_NAME)?;
        std::fs::write(&path, b"RIFF fixture audio").unwrap();
        std::fs::remove_file(input).unwrap();
        Ok(path)
    }
}

/// Five minutes of the teacher, a 20-second question from a student, then the answer.
fn timeline() -> (Vec<Word>, Vec<Turn>, i64) {
    let teacher: Vec<&str> = SENTENCES.iter().flat_map(|s| s.split(' ')).collect();
    let mut words = Vec::new();
    let mut n = 0usize;
    let mut add = |student: bool, from_s: i32, to_s: i32| {
        let mut at = from_s * 1000;
        while at + 320 <= to_s * 1000 {
            let w = if student {
                format!("s{n}")
            } else {
                teacher[n % teacher.len()].to_owned()
            };
            words.push(Word::new(w, at, at + 320, 0.9));
            n += 1;
            at += 400;
        }
    };
    add(false, 0, 300);
    add(true, 300, 320);
    add(false, 320, 600);
    let turn = |s: &str, a: i32, b: i32| Turn {
        speaker: s.into(),
        start_ms: a * 1000,
        end_ms: b * 1000,
    };
    (
        words,
        vec![
            turn("S1", 0, 300),
            turn("S2", 300, 320),
            turn("S1", 320, 600),
        ],
        600_000,
    )
}

fn glossary() -> Glossary {
    Glossary::new(&[GlossaryTerm {
        canonical: "Dogen".into(),
        kind: TermKind::Name,
        variants: vec!["doe gen".into()],
    }])
    .unwrap()
}

struct Fx {
    _db: TestDb,
    pool: PgPool,
    _server: MockServer,
    dir: PathBuf,
    talk: TalkId,
}

async fn start() -> Fx {
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
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/AudioFiles/talk.mp3"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ID3 fixture media".to_vec()))
        .mount(&server)
        .await;
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('vimeo', '900000001', 'https://vimeo.com/900000001', 'Ada: Fixture Talk')
         returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'Ada: Fixture Talk', id, sangha_id, 'Ada Fixture', 'awaiting_consent'
         from teacher where display_name = 'Ada Fixture' returning id",
    )
    .bind(video)
    .fetch_one(&pool)
    .await
    .unwrap();
    wt_db::consent::set_consent(
        &pool,
        "Ada",
        ConsentStatus::Granted,
        Some("fixture"),
        None,
        "test",
    )
    .await
    .unwrap();
    sqlx::query(
        "insert into source_video (platform, external_id, url, title, raw)
         values ('rss', 'ep-1', 'https://feed.example/ep-1', 'Fixture Talk (Ada)', $1)",
    )
    .bind(json!({
        "item": { "enclosure": { "url": format!("{}/AudioFiles/talk.mp3", server.uri()),
                                 "type": "audio/mpeg" } },
        "match": { "vimeo_id": "900000001", "how": "title" }
    }))
    .execute(&pool)
    .await
    .unwrap();
    // The hand list holds the passage the teacher quotes.
    let work: Uuid =
        sqlx::query_scalar("insert into work (title) values ('Genjokoan') returning id")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "insert into work_locator (work_id, locator, label, aliases)
         values ($1, 'genjokoan:5', 'Genjokoan, paragraph 5', '{To study the self is to forget the self}')",
    )
    .bind(work)
    .execute(&pool)
    .await
    .unwrap();
    Fx {
        _db: db,
        pool,
        _server: server,
        dir: std::env::temp_dir().join(format!("wt-pipeline-{}", Uuid::new_v4())),
        talk: TalkId::from(talk),
    }
}

fn extraction_reply() -> serde_json::Value {
    json!({
        "practice_period": null,
        "citations": [
            {"text": "to study the self is to forget the self", "work_hint": "Genjokoan",
             "locator_hint": null, "kind": "quotation"}
        ]
    })
}

/// Runs one claimed or direct stage for the fixture talk.
async fn run_stage(fx: &Fx, kind: JobKind, llm: &FakeLlm) -> String {
    run_stage_with(fx, kind, llm, None).await
}

/// Runs a stage with a student-question editor for the `audio` job.
async fn run_stage_with(
    fx: &Fx,
    kind: JobKind,
    llm: &FakeLlm,
    editor: Option<&dyn QuestionEditor>,
) -> String {
    let (words, turns, audio_ms) = timeline();
    let transcriber = FakeTranscriber::new(words, audio_ms);
    let diarizer = FakeDiarizer::new(turns);
    let fetcher = MediaFetcher::new(Exclusions::default(), false).unwrap();
    let objects = LocalObjects::new(fx.dir.join("objects"));
    let segment = SegmentSettings {
        diarize_scope: "full".into(),
        ..SegmentSettings::default()
    };
    let hints = AsrHints {
        glossary_prompt: "Dogen".into(),
        language: "en".into(),
    };
    let scratch = fx.dir.join("scratch");
    let g = glossary();
    let mut conn = fx.pool.acquire().await.unwrap();
    match kind {
        JobKind::Audio => {
            let deps = AudioDeps {
                transcriber: &transcriber,
                diarizer: &diarizer,
                labeler: &FixedLabeler(Label::Talk),
                converter: &FakeConverter,
                fetcher: &fetcher,
                objects: &objects,
                scratch_root: &scratch,
                segment: &segment,
                hints: &hints,
                actor: "test",
                editor,
            };
            format!(
                "{:?}",
                audio::run(&mut conn, fx.talk, &deps, false).await.unwrap()
            )
        }
        JobKind::Correct => format!(
            "{:?}",
            textjobs::run_correct(&mut conn, fx.talk, &objects, &g, &|_| false)
                .await
                .unwrap()
        ),
        JobKind::Extract => format!(
            "{:?}",
            textjobs::run_extract(&mut conn, fx.talk, &objects, &g, llm)
                .await
                .unwrap()
        ),
        JobKind::Resolve => format!(
            "{:?}",
            textjobs::run_resolve(
                &mut conn,
                fx.talk,
                &objects,
                &[],
                &PgHandList::new(fx.pool.clone())
            )
            .await
            .unwrap()
        ),
        JobKind::Index => format!(
            "{:?}",
            textjobs::run_index(
                &mut conn,
                fx.talk,
                &objects,
                &g,
                &ChunkSettings::default(),
                &FakeEmbedder::new("fake-bge-m3"),
            )
            .await
            .unwrap()
            .unwrap()
        ),
        other => panic!("no stage {other}"),
    }
}

const STAGES: [JobKind; 5] = [
    JobKind::Audio,
    JobKind::Correct,
    JobKind::Extract,
    JobKind::Resolve,
    JobKind::Index,
];

/// A digest of every row the pipeline writes.
async fn snapshot(pool: &PgPool) -> Vec<(String, i64, String)> {
    let mut out = Vec::new();
    for table in [
        "talk",
        "transcript",
        "segment",
        "passage",
        "citation",
        "job",
        "llm_usage",
        "student_question",
    ] {
        let row: (i64, Option<String>) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            // The table names are the constants above.
            "select count(*), md5(string_agg(x::text, '|' order by x::text)) from {table} x"
        )))
        .fetch_one(pool)
        .await
        .unwrap();
        out.push((table.to_owned(), row.0, row.1.unwrap_or_default()));
    }
    out
}

#[tokio::test]
async fn a_synthetic_talk_goes_from_cleared_to_ready_and_rerunning_every_stage_changes_nothing() {
    let fx = start().await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let queued = audio::advance_cleared(&mut conn).await.unwrap();
    assert_eq!(queued, vec![fx.talk]);
    assert!(
        audio::advance_cleared(&mut conn).await.unwrap().is_empty(),
        "advancing twice queues nothing new"
    );

    // The worker loop: claim, run, succeed, until the queue is empty.
    let llm = FakeLlm::new(vec![Ok(extraction_reply())]);
    let mut ran = Vec::new();
    while let Some(claimed) = job::claim(&mut conn, &STAGES, "test").await.unwrap() {
        let outcome = run_stage(&fx, claimed.kind, &llm).await;
        job::succeed(&mut conn, &claimed).await.unwrap();
        ran.push((claimed.kind, outcome));
    }
    let kinds: Vec<JobKind> = ran.iter().map(|(k, _)| *k).collect();
    assert_eq!(kinds, STAGES.to_vec(), "{ran:#?}");
    assert_eq!(llm.seen.lock().unwrap().len(), 1);

    let state: String = sqlx::query_scalar("select state::text from talk where id = $1")
        .bind(fx.talk.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(state, "ready");
    let served: Vec<(String, String)> = sqlx::query_as(
        "select text, segment_kind::text from servable_passage where talk_id = $1 order by ordinal",
    )
    .bind(fx.talk.as_uuid())
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert!(served.len() >= 4, "{} passages", served.len());
    assert!(served.iter().any(|(_, k)| k == "qa_teacher"));
    assert!(served.iter().all(|(t, _)| !t.contains("doe gen")));
    assert!(served.iter().any(|(t, _)| t.contains("As Dogen wrote")));
    assert!(
        served
            .iter()
            .all(|(t, _)| !t.split(' ').any(|w| w.starts_with('s')
                && w[1..].chars().all(|c| c.is_ascii_digit())
                && w.len() > 1)),
        "no student word is served"
    );
    let (confidence, method, span): (String, String, String) = sqlx::query_as(
        "select c.confidence::text, c.method,
                substr(p.text, c.span_start + 1, c.span_end - c.span_start)
         from citation c join passage p on p.id = c.passage_id
         where c.talk_id = $1 order by p.ordinal limit 1",
    )
    .bind(fx.talk.as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        (confidence.as_str(), method.as_str()),
        ("resolved", "hand_list")
    );
    assert_eq!(span, "To study the self is to forget the self.");
    assert!(
        wt_core::scratch::leftovers(&fx.dir.join("scratch"))
            .unwrap()
            .is_empty()
    );

    // Every stage again, directly: nothing changes, nothing is queued, no model is called.
    let objects = LocalObjects::new(fx.dir.join("objects"));
    let staged = objects.get(&staging_key(fx.talk)).unwrap();
    let before = snapshot(&fx.pool).await;
    let silent = FakeLlm::new(vec![]);
    let outcomes: Vec<String> = {
        let mut v = Vec::new();
        for kind in STAGES {
            v.push(run_stage(&fx, kind, &silent).await);
        }
        v
    };
    assert_eq!(outcomes[0], format!("{:?}", AudioOutcome::Current));
    assert!(outcomes[4].contains("written: false"), "{}", outcomes[4]);
    assert_eq!(
        outcomes[1],
        format!("{:?}", TextOutcome::Done(Some(JobKind::Index)))
    );
    assert!(silent.seen.lock().unwrap().is_empty());
    assert!(audio::advance_cleared(&mut conn).await.unwrap().is_empty());
    let leftover = job::claim(&mut conn, &STAGES, "test").await.unwrap();
    assert!(leftover.is_none(), "{leftover:?}");
    assert_eq!(snapshot(&fx.pool).await, before);
    assert_eq!(objects.get(&staging_key(fx.talk)).unwrap(), staged);
}

/// The fixture student says `s750` to `s799`; the editor keeps eight of the words.
fn editor() -> FakeEditor<impl Fn(&str) -> String + Send + Sync> {
    FakeEditor::new(|s: &str| {
        let w: Vec<&str> = s.split_whitespace().take(8).collect();
        format!("Teacher, about {}?", w.join(" "))
    })
}

fn the_edit() -> String {
    let w: Vec<String> = (750..758).map(|i| format!("s{i}")).collect();
    format!("Teacher, about {}?", w.join(" "))
}

/// Spec 04, Student questions: the student's question is stored only as its edit, with its
/// embedding, beside the teacher's answer; rerunning every stage changes nothing; the owner's
/// export shows it with the label; and a talk takedown leaves no question behind.
#[tokio::test]
async fn a_students_question_is_stored_only_as_its_edit_and_goes_with_its_talk() {
    let fx = start().await;
    let mut conn = fx.pool.acquire().await.unwrap();
    audio::advance_cleared(&mut conn).await.unwrap();
    let llm = FakeLlm::new(vec![Ok(extraction_reply())]);
    let editor = editor();
    while let Some(claimed) = job::claim(&mut conn, &STAGES, "test").await.unwrap() {
        run_stage_with(&fx, claimed.kind, &llm, Some(&editor)).await;
        job::succeed(&mut conn, &claimed).await.unwrap();
    }
    let rows: Vec<(String, i32, i32, i32, String, String)> = sqlx::query_as(
        "select text, start_ms, end_ms, answer_start_ms, edit_model, embedding_model
         from student_question where talk_id = $1",
    )
    .bind(fx.talk.as_uuid())
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (text, start, _, answer, model, embed) = &rows[0];
    assert_eq!(text, &the_edit());
    assert_eq!((*start, *answer), (300_000, 320_000));
    assert_eq!(
        (model.as_str(), embed.as_str()),
        ("fake-editor", "fake-bge-m3")
    );
    let served: i64 =
        sqlx::query_scalar("select count(*) from servable_student_question where talk_id = $1")
            .bind(fx.talk.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(served, 1);

    // The student's other words are nowhere: not in the transcript, segments, passages or the
    // staged edit.
    let objects = LocalObjects::new(fx.dir.join("objects"));
    let transcript = objects
        .get(&audio::transcript_key(fx.talk, 1))
        .unwrap()
        .unwrap();
    let staged = objects
        .get(&audio::questions_key(fx.talk))
        .unwrap()
        .unwrap();
    for bytes in [&transcript, &staged] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("s760") && !text.contains("s799"), "{text}");
    }
    let stored: i64 = sqlx::query_scalar(
        "select (select count(*) from segment where text ~ 's7[5-9][0-9]')
              + (select count(*) from passage where text ~ 's7[5-9][0-9]')",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(stored, 0);
    assert!(
        wt_core::scratch::leftovers(&fx.dir.join("scratch"))
            .unwrap()
            .is_empty()
    );

    // Every stage again: nothing changes and the editor is not asked again.
    let before = snapshot(&fx.pool).await;
    let silent = FakeLlm::new(vec![]);
    for kind in STAGES {
        run_stage_with(&fx, kind, &silent, Some(&editor)).await;
    }
    assert_eq!(snapshot(&fx.pool).await, before);
    assert_eq!(editor.calls.load(std::sync::atomic::Ordering::Relaxed), 1);

    // The owner's export shows the question with the label, in place of the bare gap.
    let out = fx.dir.join("owner-export");
    let report = wt_ingest::owner_export::export_teacher(
        &mut conn,
        &objects,
        &glossary(),
        &ChunkSettings::default(),
        "Ada",
        &out,
    )
    .await
    .unwrap();
    let md = std::fs::read_to_string(report.dir.join(&report.files[0])).unwrap();
    assert!(
        md.contains(&format!("**Student:** {} [0:05:00]", the_edit())),
        "{md}"
    );
    assert!(
        md.contains("_Question lightly edited by machine; names and identifying details removed._")
    );
    assert!(!md.contains("A student asked a question"), "{md}");

    // A talk takedown leaves no question, and its staged edit goes with the transcript.
    wt_db::takedown::run(&fx.pool, &wt_db::takedown::Scope::Talk(fx.talk), "test")
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("select count(*) from student_question")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    audio::delete_talk_objects(&objects, &[fx.talk]).unwrap();
    assert!(
        objects
            .get(&audio::questions_key(fx.talk))
            .unwrap()
            .is_none()
    );
}

/// Spec 04, Owner exports, and Spec 03, display level e: the teacher's talk is written for their
/// own use with the removed stretches marked and no student words, and then the takedown leaves
/// nothing in the database while the export stays.
#[tokio::test]
async fn the_owner_export_writes_the_teachers_words_then_level_e_deletes_the_rest() {
    let fx = start().await;
    let mut conn = fx.pool.acquire().await.unwrap();
    audio::advance_cleared(&mut conn).await.unwrap();
    let llm = FakeLlm::new(vec![Ok(extraction_reply())]);
    while let Some(claimed) = job::claim(&mut conn, &STAGES, "test").await.unwrap() {
        run_stage(&fx, claimed.kind, &llm).await;
        job::succeed(&mut conn, &claimed).await.unwrap();
    }

    let objects = LocalObjects::new(fx.dir.join("objects"));
    let out = fx.dir.join("owner-export");
    let report = wt_ingest::owner_export::export_teacher(
        &mut conn,
        &objects,
        &glossary(),
        &ChunkSettings::default(),
        "Ada",
        &out,
    )
    .await
    .unwrap();
    assert_eq!(report.files.len(), 1, "{report:?}");
    assert!(report.skipped.is_empty());
    let text = std::fs::read_to_string(report.dir.join(&report.files[0])).unwrap();
    assert!(text.starts_with("# Ada: Fixture Talk\n"), "{text}");
    assert!(text.contains("Source: https://vimeo.com/900000001"));
    assert!(
        text.contains("As Dogen wrote"),
        "the glossary's corrections"
    );
    assert!(
        text.contains("_[A student asked a question, 0:05:20]_"),
        "{text}"
    );
    assert!(
        !text.split_whitespace().any(|w| w.len() > 1
            && w.starts_with('s')
            && w[1..].chars().all(|c| c.is_ascii_digit())),
        "no student word is exported"
    );
    let index = std::fs::read_to_string(report.dir.join("README.md")).unwrap();
    assert!(index.contains(&report.files[0]));

    // Level e: recorded, then the opt-out takedown with exclusions.
    let change = wt_db::display::mark_takedown_level(
        &mut conn,
        "Ada",
        wt_db::display::DisplayLevel::ExportThenDelete,
        "owner",
        "asked for a copy and removal",
    )
    .await
    .unwrap();
    assert!(change.changed);
    let takedown = wt_db::rights::opt_out(&fx.pool, "Ada", "asked", "owner", false)
        .await
        .unwrap();
    assert!(takedown.takedown.is_some());
    for table in ["passage", "segment", "transcript", "citation"] {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            // The table names are the constants above.
            "select count(*) from {table}"
        )))
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        assert_eq!(n, 0, "{table} rows remain");
    }
    let level: String =
        sqlx::query_scalar("select display_level from teacher where display_name = 'Ada Fixture'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(level, "export_then_delete");
    // Step 3: the stored transcript and citations go too; the export stays.
    let talks = takedown.takedown.as_ref().unwrap().talks.clone();
    let key = format!("transcripts/{}/v1.json", fx.talk.as_uuid());
    assert!(objects.get(&key).unwrap().is_some());
    let deleted = audio::delete_talk_objects(&objects, &talks).unwrap();
    assert!(deleted >= 2, "transcript and citations: {deleted}");
    assert!(objects.get(&key).unwrap().is_none());
    assert!(objects.get(&staging_key(fx.talk)).unwrap().is_none());
    assert_eq!(audio::delete_talk_objects(&objects, &talks).unwrap(), 0);
    assert!(
        report.dir.join(&report.files[0]).is_file(),
        "the export stays"
    );
}

/// Spec 04, Transcript review: a review file round-trips into a reviewed version that keeps
/// word times and citations, a stale file is refused, and no automated pass replaces it.
#[tokio::test]
async fn a_review_round_trips_into_a_reviewed_version_no_pass_overwrites() {
    use wt_ingest::review::{self, Corrections, Edit};
    let fx = start().await;
    let mut conn = fx.pool.acquire().await.unwrap();
    audio::advance_cleared(&mut conn).await.unwrap();
    let llm = FakeLlm::new(vec![Ok(extraction_reply())]);
    while let Some(claimed) = job::claim(&mut conn, &STAGES, "test").await.unwrap() {
        run_stage(&fx, claimed.kind, &llm).await;
        job::succeed(&mut conn, &claimed).await.unwrap();
    }
    let objects = LocalObjects::new(fx.dir.join("objects"));
    let (path, file) = review::export_review(
        &mut conn,
        &objects,
        &glossary(),
        &ChunkSettings::default(),
        fx.talk,
        &fx.dir.join("review"),
    )
    .await
    .unwrap();
    let html = std::fs::read_to_string(&path).unwrap();
    assert!(html.contains("Listen at 0:00:00"));
    assert!(
        html.contains("As Dogen wrote"),
        "the reviewer sees the corrected text"
    );
    assert!(!html.contains("src=\"http"));

    // Edit the first paragraph: one word changed, two added.
    let p = &file.paragraphs[0];
    assert!(p.text.contains("in the quiet hall"), "{}", p.text);
    let edited = p
        .text
        .replacen("in the quiet hall", "in the quiet zendo hall together", 1);
    let corrections = Corrections {
        schema: review::SCHEMA,
        talk_id: file.talk_id.clone(),
        transcript_hash: file.transcript_hash.clone(),
        edits: vec![Edit {
            w0: p.w0,
            w1: p.w1,
            text: edited.clone(),
        }],
    };
    let report = review::import_corrections(
        &mut conn,
        &objects,
        &glossary(),
        &corrections,
        "Priest Fixture",
    )
    .await
    .unwrap();
    assert_eq!((report.version, report.edits), (2, 1));
    // The reviewed version keeps the corrected text: each "doe gen" became one "Dogen", and
    // the reviewer added two words.
    let v1: serde_json::Value = serde_json::from_slice(
        &objects
            .get(&format!("transcripts/{}/v1.json", fx.talk.as_uuid()))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let merges = v1["words"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["w"] == "doe")
        .count();
    assert!(merges > 0);
    assert_eq!(report.words.1, report.words.0 - merges + 2);
    assert_eq!(report.citations_moved, 1);

    // The reviewed version: origin, reviewer, and times that stay in order.
    let origin: String =
        sqlx::query_scalar("select origin from transcript where talk_id = $1 and is_current")
            .bind(fx.talk.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(origin, "reviewed");
    let doc: serde_json::Value = serde_json::from_slice(
        &objects
            .get(&format!("transcripts/{}/v2.json", fx.talk.as_uuid()))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(doc["reviewed_by"], "Priest Fixture");
    let starts: Vec<i64> = doc["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["s"].as_i64().unwrap())
        .collect();
    assert!(
        starts.windows(2).all(|p| p[0] <= p[1]),
        "word times stay in order"
    );
    let logged: (String, String) = sqlx::query_as(
        "select actor, new_value #>> '{}' from correction_log where entity = 'transcript'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(logged, ("Priest Fixture".to_owned(), edited));

    // The queued correct and index jobs rebuild the passages; the citation still lands.
    while let Some(claimed) = job::claim(&mut conn, &STAGES, "test").await.unwrap() {
        run_stage(&fx, claimed.kind, &FakeLlm::new(vec![])).await;
        job::succeed(&mut conn, &claimed).await.unwrap();
    }
    let served: String = sqlx::query_scalar(
        "select string_agg(text, ' ' order by ordinal) from servable_passage where talk_id = $1",
    )
    .bind(fx.talk.as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert!(
        served.contains("in the quiet zendo hall together"),
        "{served}"
    );
    let span: String = sqlx::query_scalar(
        "select substr(p.text, c.span_start + 1, c.span_end - c.span_start)
         from citation c join passage p on p.id = c.passage_id where c.talk_id = $1 limit 1",
    )
    .bind(fx.talk.as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(span, "To study the self is to forget the self.");

    // The same file again is stale: the transcript changed.
    let stale = review::import_corrections(
        &mut conn,
        &objects,
        &glossary(),
        &corrections,
        "Priest Fixture",
    )
    .await
    .unwrap_err();
    assert!(matches!(stale, review::ReviewError::Stale(_)), "{stale}");

    // No automated pass replaces it: requeue, a retranscribing audio job, a glossary change.
    let requeued = audio::requeue_for_model(&mut conn, &objects, "whisper-new", false)
        .await
        .unwrap();
    assert!(requeued.is_empty());
    assert_eq!(
        run_stage(&fx, JobKind::Audio, &FakeLlm::new(vec![])).await,
        "Current"
    );
    let wider = Glossary::new(&[GlossaryTerm {
        canonical: "Zendo Hall".into(),
        kind: TermKind::Term,
        variants: vec!["zendo hall".into()],
    }])
    .unwrap();
    let affected = textjobs::requeue_after_glossary_change(&mut conn, &objects, &wider)
        .await
        .unwrap();
    assert!(
        affected.is_empty(),
        "the glossary leaves reviewed words alone"
    );
}
