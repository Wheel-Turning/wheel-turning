//! Spec 04, the `audio` job, with fakes for recognition, diarization and conversion, a local
//! server for the feed's audio host and a real database. Every word and file is synthetic: the
//! teacher says `t<n>`, a student says `s<n>`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

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
use wt_db::test_support::TestDb;
use wt_ingest::IngestError;
use wt_ingest::audio::{self, AudioDeps, AudioOutcome};
use wt_ingest::http::Exclusions;
use wt_ingest::media::{MediaFetcher, MediaSource};
use wt_registry::RegistryFile;
use wt_segment::{FakeDiarizer, FixedLabeler, Label, SegmentSettings, Turn};

const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Mountain Zendo"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Mountain Zendo"
aliases = ["Ada"]
"#;

/// Writes a WAV of silence instead of running ffmpeg, and deletes the fetched file as ffmpeg's
/// converter does.
struct FakeConverter;

impl Converter for FakeConverter {
    fn to_wav(&self, dir: &ScratchDir, input: &Path) -> Result<PathBuf, ScratchError> {
        let path = dir.file(WAV_NAME)?;
        std::fs::write(&path, b"RIFF fixture audio").unwrap();
        std::fs::remove_file(input).unwrap();
        Ok(path)
    }
}

struct Fx {
    _db: TestDb,
    pool: PgPool,
    server: MockServer,
    dir: PathBuf,
}

impl Fx {
    fn scratch(&self) -> PathBuf {
        self.dir.join("scratch")
    }

    fn objects(&self) -> LocalObjects {
        LocalObjects::new(self.dir.join("objects"))
    }

    fn scratch_is_empty(&self) -> bool {
        wt_core::scratch::leftovers(&self.scratch())
            .unwrap()
            .is_empty()
    }
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
    let dir = std::env::temp_dir().join(format!("wt-audio-{}", Uuid::new_v4()));
    Fx {
        _db: db,
        pool,
        server,
        dir,
    }
}

/// A cleared talk by Ada, and when `with_episode`, a feed episode matched to it.
async fn cleared_talk(fx: &Fx, with_episode: bool) -> TalkId {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('vimeo', '900000001', 'https://vimeo.com/900000001', 'Ada: Fixture Talk')
         returning id",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'Ada: Fixture Talk', id, sangha_id, 'Ada Fixture', 'awaiting_consent'
         from teacher where display_name = 'Ada Fixture' returning id",
    )
    .bind(video)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    wt_db::consent::set_consent(
        &fx.pool,
        "Ada",
        ConsentStatus::Granted,
        Some("fixture"),
        None,
        "test",
    )
    .await
    .unwrap();
    if with_episode {
        sqlx::query(
            "insert into source_video (platform, external_id, url, title, raw)
             values ('rss', 'ep-1', 'https://feed.example/ep-1', 'Fixture Talk (Ada)', $1)",
        )
        .bind(serde_json::json!({
            "item": { "enclosure": { "url": format!("{}/AudioFiles/talk.mp3", fx.server.uri()),
                                     "type": "audio/mpeg" } },
            "match": { "vimeo_id": "900000001", "how": "title" }
        }))
        .execute(&fx.pool)
        .await
        .unwrap();
    }
    TalkId::from(talk)
}

/// Ten minutes of talk by S1, a 20-second question from S2 at 5:00, then the answer.
fn timeline() -> (Vec<Word>, Vec<Turn>) {
    let mut words = Vec::new();
    let mut add = |speaker: char, from_s: i32, to_s: i32| {
        let mut at = from_s * 1000;
        while at + 320 <= to_s * 1000 {
            words.push(Word::new(
                format!("{speaker}{}", words.len()),
                at,
                at + 320,
                0.9,
            ));
            at += 400;
        }
    };
    add('t', 0, 300);
    add('s', 300, 320);
    add('t', 320, 600);
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
    )
}

async fn run(
    fx: &Fx,
    talk: TalkId,
    transcriber: &FakeTranscriber,
    turns: Vec<Turn>,
) -> Result<AudioOutcome, audio::AudioError> {
    let diarizer = FakeDiarizer::new(turns);
    let fetcher = MediaFetcher::new(Exclusions::default(), false).unwrap();
    let objects = fx.objects();
    let segment = SegmentSettings::default();
    let hints = AsrHints {
        glossary_prompt: "Dogen".into(),
        language: "en".into(),
    };
    let scratch = fx.scratch();
    let deps = AudioDeps {
        transcriber,
        diarizer: &diarizer,
        labeler: &FixedLabeler(Label::Talk),
        converter: &FakeConverter,
        fetcher: &fetcher,
        objects: &objects,
        scratch_root: &scratch,
        segment: &segment,
        hints: &hints,
        actor: "test",
    };
    let mut conn = fx.pool.acquire().await.unwrap();
    audio::run(&mut conn, talk, &deps, false).await
}

async fn state(pool: &PgPool, talk: TalkId) -> (String, Option<String>) {
    sqlx::query_as("select state::text, state_reason from talk where id = $1")
        .bind(talk.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn only_the_teachers_words_are_stored_and_scratch_is_gone() {
    let fx = start().await;
    let talk = cleared_talk(&fx, true).await;
    let (words, turns) = timeline();
    let transcriber = FakeTranscriber::new(words, 600_000);

    let outcome = run(&fx, talk, &transcriber, turns).await.unwrap();
    let AudioOutcome::Segmented {
        version, segments, ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!((version, segments), (1, 2));
    assert!(fx.scratch_is_empty(), "no audio or raw recognition left");

    // The stored JSON holds no student word and no student span.
    let key = audio::transcript_key(talk, 1);
    let json: serde_json::Value =
        serde_json::from_slice(&fx.objects().get(&key).unwrap().unwrap()).unwrap();
    let stored: Vec<&str> = json["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["w"].as_str().unwrap())
        .collect();
    assert!(!stored.is_empty());
    assert!(
        stored.iter().all(|w| w.starts_with('t')),
        "a student word was stored"
    );
    assert!(
        json["spans"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["kind"] != "qa_student")
    );
    assert_eq!(json["schema"], 1);
    assert_eq!(json["glossary_version"], serde_json::Value::Null);

    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "select kind::text, text, question_note from segment where talk_id = $1 order by ordinal",
    )
    .bind(talk.as_uuid())
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["talk", "qa_teacher"]
    );
    assert!(
        rows.iter().all(|r| !r.1.contains('s') && r.2.is_none()),
        "teacher text, null note"
    );
    let (hash, current): (String, bool) =
        sqlx::query_as("select content_hash, is_current from transcript where talk_id = $1")
            .bind(talk.as_uuid())
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert!(current);
    let queued: i64 = sqlx::query_scalar(
        "select count(*) from job where kind = 'correct' and talk_id = $1 and idempotency_key like '%' || $2",
    )
    .bind(talk.as_uuid())
    .bind(&hash)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        queued, 1,
        "the correct job is queued, keyed by the transcript"
    );
    assert_eq!(state(&fx.pool, talk).await.0, "segmenting");

    // The database refuses a student segment outright.
    let err = sqlx::query(
        "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
         select talk_id, id, 99, 'qa_student', 0, 1, 'x' from transcript where talk_id = $1",
    )
    .bind(talk.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap_err();
    assert!(err.to_string().contains("check"), "{err}");
}

#[tokio::test]
async fn a_failure_leaves_no_scratch_and_a_strong_second_speaker_is_held() {
    let fx = start().await;
    let talk = cleared_talk(&fx, true).await;
    let (words, turns) = timeline();

    let mut failing = FakeTranscriber::new(words.clone(), 600_000);
    failing.fail_with = Some("model crashed".into());
    assert!(run(&fx, talk, &failing, turns.clone()).await.is_err());
    assert!(
        fx.scratch_is_empty(),
        "scratch is deleted on a returned error"
    );
    assert_eq!(
        state(&fx.pool, talk).await.0,
        "transcribing",
        "the retry starts from here"
    );

    // The retry, with a student speaking half the time: the talk goes to a person.
    let half = vec![
        Turn {
            speaker: "S1".into(),
            start_ms: 0,
            end_ms: 300_000,
        },
        Turn {
            speaker: "S2".into(),
            start_ms: 300_000,
            end_ms: 600_000,
        },
    ];
    let outcome = run(&fx, talk, &FakeTranscriber::new(words, 600_000), half)
        .await
        .unwrap();
    assert_eq!(outcome, AudioOutcome::Held("multi_speaker".into()));
    assert_eq!(
        state(&fx.pool, talk).await,
        ("needs_review".into(), Some("multi_speaker".into()))
    );
    assert!(fx.scratch_is_empty());
    let stored: i64 = sqlx::query_scalar("select count(*) from transcript where talk_id = $1")
        .bind(talk.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(stored, 0, "nothing is stored for a held talk");
}

#[tokio::test]
async fn no_media_route_holds_the_talk_and_fetches_nothing() {
    let fx = start().await;
    let talk = cleared_talk(&fx, false).await;
    let (words, turns) = timeline();
    let outcome = run(&fx, talk, &FakeTranscriber::new(words, 600_000), turns)
        .await
        .unwrap();
    assert_eq!(outcome, AudioOutcome::NoMedia);
    assert_eq!(
        state(&fx.pool, talk).await,
        ("needs_review".into(), Some("no_media".into()))
    );
    let fetched = fx
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().contains("AudioFiles"))
        .count();
    assert_eq!(fetched, 0);
    assert!(fx.scratch_is_empty());
}

#[tokio::test]
async fn a_talk_that_no_longer_passes_the_gate_is_not_fetched() {
    let fx = start().await;
    let talk = cleared_talk(&fx, true).await;
    wt_db::rights::opt_out(&fx.pool, "Ada", "asked to leave", "test", true)
        .await
        .unwrap();
    let (words, turns) = timeline();
    let outcome = run(&fx, talk, &FakeTranscriber::new(words, 600_000), turns)
        .await
        .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::NotCleared(_)),
        "{outcome:?}"
    );
    assert!(fx.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn third_party_fetching_is_refused_before_anything_starts() {
    let fx = start().await;
    let talk = cleared_talk(&fx, true).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let consented = wt_db::talk::TalkRepo::authorize(&mut conn, talk)
        .await
        .unwrap()
        .unwrap();
    let scratch = ScratchDir::create(&fx.scratch(), talk).unwrap();
    let fetcher = MediaFetcher::new(Exclusions::default(), false).unwrap();
    let source = MediaSource::ThirdParty {
        url: format!("{}/watch", fx.server.uri()),
    };
    let err = fetcher
        .fetch(&consented, &source, &scratch)
        .await
        .unwrap_err();
    assert!(matches!(err, IngestError::ThirdPartyFetchDisabled), "{err}");
    assert!(fx.server.received_requests().await.unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(scratch.path()).unwrap().count(),
        0,
        "nothing written"
    );
}
