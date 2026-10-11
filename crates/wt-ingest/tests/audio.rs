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
    run_scoped(
        fx,
        talk,
        transcriber,
        &FakeDiarizer::new(turns),
        &FakeConverter,
        "full",
    )
    .await
}

/// The job with a student-question editor.
async fn run_editing(
    fx: &Fx,
    talk: TalkId,
    transcriber: &FakeTranscriber,
    turns: Vec<Turn>,
    editor: Option<&dyn QuestionEditor>,
) -> Result<AudioOutcome, audio::AudioError> {
    let fetcher = MediaFetcher::new(Exclusions::default(), false).unwrap();
    let objects = fx.objects();
    let segment = SegmentSettings {
        diarize_scope: "full".into(),
        ..SegmentSettings::default()
    };
    let hints = AsrHints {
        glossary_prompt: String::new(),
        language: "en".into(),
    };
    let scratch = fx.scratch();
    let diarizer = FakeDiarizer::new(turns);
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
        editor,
    };
    let mut conn = fx.pool.acquire().await.unwrap();
    audio::run(&mut conn, talk, &deps, false).await
}

/// The job with a chosen diarizer, converter and diarization scope.
async fn run_scoped(
    fx: &Fx,
    talk: TalkId,
    transcriber: &FakeTranscriber,
    diarizer: &dyn wt_segment::Diarizer,
    converter: &dyn Converter,
    scope: &str,
) -> Result<AudioOutcome, audio::AudioError> {
    let fetcher = MediaFetcher::new(Exclusions::default(), false).unwrap();
    let objects = fx.objects();
    let segment = SegmentSettings {
        diarize_scope: scope.into(),
        ..SegmentSettings::default()
    };
    let hints = AsrHints {
        glossary_prompt: "Dogen".into(),
        language: "en".into(),
    };
    let scratch = fx.scratch();
    let deps = AudioDeps {
        transcriber,
        diarizer,
        labeler: &FixedLabeler(Label::Talk),
        converter,
        fetcher: &fetcher,
        objects: &objects,
        scratch_root: &scratch,
        segment: &segment,
        hints: &hints,
        actor: "test",
        editor: None,
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

/// Spec 04: changing the model requeues only talks transcribed with another one, when asked,
/// and the requeued job retranscribes even though a transcript exists.
#[tokio::test]
async fn a_new_model_requeues_only_talks_transcribed_with_another() {
    let fx = start().await;
    let talk = cleared_talk(&fx, true).await;
    let (words, turns) = timeline();
    let transcriber = FakeTranscriber::new(words.clone(), 600_000);
    run(&fx, talk, &transcriber, turns.clone()).await.unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();
    let objects = fx.objects();
    let jobs = |pool: PgPool| async move {
        let n: i64 = sqlx::query_scalar("select count(*) from job where kind = 'audio'")
            .fetch_one(&pool)
            .await
            .unwrap();
        n
    };

    // Same model: nothing. Dry run: counted, nothing queued.
    let same = audio::requeue_for_model(&mut conn, &objects, "fake-asr", false)
        .await
        .unwrap();
    assert!(same.is_empty());
    let dry = audio::requeue_for_model(&mut conn, &objects, "whisper-large-v3", true)
        .await
        .unwrap();
    assert_eq!(dry, vec![talk]);
    assert_eq!(jobs(fx.pool.clone()).await, 0);

    // Another model: one job with retranscribe, once.
    for _ in 0..2 {
        let queued = audio::requeue_for_model(&mut conn, &objects, "whisper-large-v3", false)
            .await
            .unwrap();
        assert_eq!(queued, vec![talk]);
    }
    assert_eq!(jobs(fx.pool.clone()).await, 1);
    let payload: serde_json::Value =
        sqlx::query_scalar("select payload from job where kind = 'audio'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(payload["retranscribe"], true);
    assert_eq!(payload["was"], "fake-asr");

    // Without retranscribe the job is a no-op; with it a new version is stored.
    assert_eq!(
        run(&fx, talk, &transcriber, turns.clone()).await.unwrap(),
        AudioOutcome::Current
    );
    let diarizer = FakeDiarizer::new(turns);
    let fetcher = MediaFetcher::new(Exclusions::default(), false).unwrap();
    let segment = SegmentSettings {
        diarize_scope: "full".into(),
        ..SegmentSettings::default()
    };
    let hints = AsrHints {
        glossary_prompt: String::new(),
        language: "en".into(),
    };
    let scratch = fx.scratch();
    let again = FakeTranscriber::new(words, 600_000);
    let deps = AudioDeps {
        transcriber: &again,
        diarizer: &diarizer,
        labeler: &FixedLabeler(Label::Talk),
        converter: &FakeConverter,
        fetcher: &fetcher,
        objects: &objects,
        scratch_root: &scratch,
        segment: &segment,
        hints: &hints,
        actor: "test",
        editor: None,
    };
    let out = audio::run(&mut conn, talk, &deps, true).await.unwrap();
    assert!(
        matches!(out, AudioOutcome::Segmented { version: 2, .. }),
        "{out:?}"
    );
}

/// A cleared EngageWisdom talk by Ada whose source transcript says what the timeline says, with
/// the source's own spelling of three teacher words and the student's question in it too.
async fn cleared_engagewisdom_talk(fx: &Fx, transcript: &str) -> TalkId {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title, raw)
         values ('engagewisdom', 'FX-00042', 'https://fx.example.invalid/talks/fixture',
                 'Fixture Talk', $1)
         returning id",
    )
    .bind(serde_json::json!({ "item": {
        "audio_url": format!("{}/AudioFiles/talk.mp3", fx.server.uri()),
        "transcript": transcript,
        "speakers": ["Ada Fixture"],
    }}))
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'Fixture Talk', id, sangha_id, 'Ada Fixture', 'awaiting_consent'
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
    TalkId::from(talk)
}

#[tokio::test]
async fn an_aligned_source_transcript_keeps_its_teacher_words_and_drops_the_student() {
    let fx = start().await;
    let (words, turns) = timeline();
    // The source writes every word the recording holds, but spells three teacher words its own
    // way and adds a stage direction and a marker; the student's question is in it too.
    let mut source: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
    source[10] = "Dōgen".into();
    source[400] = "zazen".into();
    source[1000] = "shikantaza".into();
    let text = format!(
        "[00:00]\n\n{} [laughs]\n\n{}",
        source[..750].join(" "),
        source[750..].join(" ")
    );
    let talk = cleared_engagewisdom_talk(&fx, &text).await;
    let transcriber = FakeTranscriber::new(words, 600_000);
    let outcome = run(&fx, talk, &transcriber, turns).await.unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert!(fx.scratch_is_empty());

    let json: serde_json::Value = serde_json::from_slice(
        &fx.objects()
            .get(&audio::transcript_key(talk, 1))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(json["origin"], "aligned");
    assert!(json["alignment_share"].as_f64().unwrap() > 0.99);
    assert!(
        json["model"]
            .as_str()
            .unwrap()
            .starts_with("source-aligned/")
    );
    let stored: Vec<&str> = json["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["w"].as_str().unwrap())
        .collect();
    for kept in ["Dōgen", "zazen", "shikantaza"] {
        assert!(
            stored.contains(&kept),
            "the source's wording of {kept} is kept"
        );
    }
    assert!(
        !stored
            .iter()
            .any(|w| w.starts_with('s') && w[1..].parse::<u32>().is_ok()),
        "no student word"
    );
    assert!(!stored.contains(&"[laughs]"));
    let origin: String = sqlx::query_scalar("select origin from transcript where talk_id = $1")
        .bind(talk.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(origin, "aligned");
}

#[tokio::test]
async fn a_source_transcript_of_another_recording_is_held_for_review() {
    let fx = start().await;
    let (words, turns) = timeline();
    let other: Vec<String> = (0..1400).map(|i| format!("other{i}")).collect();
    let talk = cleared_engagewisdom_talk(&fx, &other.join(" ")).await;
    let transcriber = FakeTranscriber::new(words, 600_000);
    let outcome = run(&fx, talk, &transcriber, turns).await.unwrap();
    assert_eq!(outcome, AudioOutcome::Held("alignment_failed".into()));
    assert_eq!(
        state(&fx.pool, talk).await,
        ("needs_review".into(), Some("alignment_failed".into()))
    );
    assert!(fx.scratch_is_empty());
}

/// Writes a real PCM WAV of silence, 100 frames a second, so the job can cut its tail.
struct WavConverter {
    seconds: usize,
}

impl Converter for WavConverter {
    fn to_wav(&self, dir: &ScratchDir, input: &Path) -> Result<PathBuf, ScratchError> {
        let path = dir.file(WAV_NAME)?;
        let rate: u32 = 100;
        let data = vec![0u8; self.seconds * 100 * 2];
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&u32::try_from(36 + data.len()).unwrap().to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        wav.extend_from_slice(&data);
        std::fs::write(&path, wav).unwrap();
        std::fs::remove_file(input).unwrap();
        Ok(path)
    }
}

/// Records the length of what it was asked to diarize, and answers with fixed turns.
struct MeasuringDiarizer {
    turns: Vec<Turn>,
    seconds: std::sync::Mutex<Vec<u64>>,
}

#[async_trait::async_trait]
impl wt_segment::Diarizer for MeasuringDiarizer {
    async fn diarize(&self, audio: &Path) -> Result<Vec<Turn>, wt_segment::DiarizeError> {
        let bytes = std::fs::metadata(audio).unwrap().len();
        self.seconds.lock().unwrap().push((bytes - 44) / 200);
        Ok(self.turns.clone())
    }
}

/// The timeline with the teacher inviting questions just before the student speaks at 5:00.
fn timeline_with_invitation() -> (Vec<Word>, Vec<Turn>) {
    let (mut words, turns) = timeline();
    // The teacher's last words before the question: "...are there any questions?"
    let i = words.iter().rposition(|w| w.start_ms < 299_000).unwrap();
    for (k, w) in ["are", "there", "any", "questions?"].iter().enumerate() {
        words[i - 3 + k].text = (*w).into();
    }
    (words, turns)
}

#[tokio::test]
async fn only_the_questions_are_diarized_and_the_student_is_still_dropped() {
    let fx = start().await;
    let talk = cleared_talk(&fx, true).await;
    let (words, _) = timeline_with_invitation();
    let transcriber = FakeTranscriber::new(words, 600_000);
    // Turns relative to the cut, a minute before the invitation near 4:58.
    let diarizer = MeasuringDiarizer {
        turns: vec![
            Turn {
                speaker: "S1".into(),
                start_ms: 0,
                end_ms: 61_500,
            },
            Turn {
                speaker: "S2".into(),
                start_ms: 61_500,
                end_ms: 81_500,
            },
            Turn {
                speaker: "S1".into(),
                start_ms: 81_500,
                end_ms: 361_500,
            },
        ],
        seconds: std::sync::Mutex::new(Vec::new()),
    };
    let outcome = run_scoped(
        &fx,
        talk,
        &transcriber,
        &diarizer,
        &WavConverter { seconds: 600 },
        "qa",
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    let diarized = diarizer.seconds.lock().unwrap().clone();
    assert_eq!(diarized.len(), 1);
    assert!(
        diarized[0] < 370,
        "only the tail was diarized: {diarized:?}"
    );
    assert!(fx.scratch_is_empty(), "the cut is deleted with the rest");
    let json: serde_json::Value = serde_json::from_slice(
        &fx.objects()
            .get(&audio::transcript_key(talk, 1))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let stored: Vec<&str> = json["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["w"].as_str().unwrap())
        .collect();
    assert!(stored.iter().any(|w| w.starts_with('t')));
    assert!(
        !stored
            .iter()
            .any(|w| w.starts_with('s') && w[1..].parse::<u32>().is_ok()),
        "no student word"
    );
}

#[tokio::test]
async fn a_monologue_is_not_diarized_at_all() {
    let fx = start().await;
    let talk = cleared_talk(&fx, true).await;
    let words: Vec<Word> = (0..1400)
        .map(|i| Word::new(format!("t{i}"), i * 400, i * 400 + 320, 0.9))
        .collect();
    let transcriber = FakeTranscriber::new(words, 600_000);
    let diarizer = MeasuringDiarizer {
        turns: Vec::new(),
        seconds: std::sync::Mutex::new(Vec::new()),
    };
    let outcome = run_scoped(
        &fx,
        talk,
        &transcriber,
        &diarizer,
        &WavConverter { seconds: 600 },
        "qa",
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert!(
        diarizer.seconds.lock().unwrap().is_empty(),
        "no diarization for a monologue"
    );
}

/// A cleared EngageWisdom talk by Ada with the source's transcript and no audio.
async fn cleared_text_only_talk(fx: &Fx, transcript: &str) -> TalkId {
    let talk = cleared_engagewisdom_talk(fx, transcript).await;
    sqlx::query(
        "update source_video set raw = raw #- '{item,audio_url}'
         where id = (select source_video_id from talk where id = $1)",
    )
    .bind(talk.as_uuid())
    .execute(&fx.pool)
    .await
    .unwrap();
    talk
}

fn words(prefix: &str, from: usize, n: usize) -> String {
    (from..from + n)
        .map(|i| format!("{prefix}{i}"))
        .collect::<Vec<_>>()
        .join(" ")
        + "."
}

/// A labeled transcript: Ada teaches, a student asks, Ada answers. Ada's words start with
/// `teacher`.
fn labeled_text(teacher: &str) -> String {
    format!(
        "{}\n\nStudent: {}\n\n{}\n\nAda: {}",
        words(teacher, 0, 1500),
        words("s", 0, 40),
        words("s", 40, 10),
        words(teacher, 1500, 300),
    )
}

fn stored_words(fx: &Fx, talk: TalkId) -> (serde_json::Value, Vec<String>) {
    let json: serde_json::Value = serde_json::from_slice(
        &fx.objects()
            .get(&audio::transcript_key(talk, 1))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let stored = json["words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["w"].as_str().unwrap().to_owned())
        .collect();
    (json, stored)
}

#[tokio::test]
async fn a_talk_with_only_a_labeled_text_keeps_the_teacher_and_fetches_nothing() {
    let fx = start().await;
    let talk = cleared_text_only_talk(&fx, &labeled_text("t")).await;
    let (w, turns) = timeline();
    let outcome = run(&fx, talk, &FakeTranscriber::new(w, 600_000), turns)
        .await
        .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert!(
        fx.server.received_requests().await.unwrap().is_empty(),
        "nothing fetched"
    );
    assert!(fx.scratch_is_empty());
    let (json, stored) = stored_words(&fx, talk);
    assert_eq!(json["origin"], "text");
    assert_eq!(json["model"], "source-text");
    assert!(
        stored.iter().any(|w| w.starts_with("t1799")),
        "the answer is kept"
    );
    assert!(
        !stored.iter().any(|w| w.starts_with('s')),
        "no student word, labeled or in the unlabeled paragraph after"
    );
    let origin: String = sqlx::query_scalar("select origin from transcript where talk_id = $1")
        .bind(talk.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(origin, "text");
}

#[tokio::test]
async fn a_labeled_text_whose_recording_does_not_align_is_used_alone() {
    let fx = start().await;
    // Ada's words here are not the recording's.
    let talk = cleared_engagewisdom_talk(&fx, &labeled_text("x")).await;
    let (w, turns) = timeline();
    let outcome = run(&fx, talk, &FakeTranscriber::new(w, 600_000), turns)
        .await
        .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert!(fx.scratch_is_empty());
    let (json, stored) = stored_words(&fx, talk);
    assert_eq!(json["origin"], "text");
    assert!(stored.iter().any(|w| w.starts_with("x1799")));
    assert!(!stored.iter().any(|w| w.starts_with('s')));
}

#[tokio::test]
async fn an_unlabeled_conversation_with_no_audio_is_held() {
    let fx = start().await;
    let talk = cleared_text_only_talk(
        &fx,
        &"What do you think? Yes. And the pond? No. It is still. ".repeat(80),
    )
    .await;
    let (w, turns) = timeline();
    let outcome = run(&fx, talk, &FakeTranscriber::new(w, 600_000), turns)
        .await
        .unwrap();
    assert_eq!(outcome, AudioOutcome::Held("unlabeled_dialogue".into()));
    assert_eq!(
        state(&fx.pool, talk).await,
        ("needs_review".into(), Some("unlabeled_dialogue".into()))
    );
}

#[tokio::test]
async fn held_talks_go_back_to_cleared_with_one_job_per_hold() {
    let fx = start().await;
    let talk = cleared_talk(&fx, false).await;
    let (w, turns) = timeline();
    let outcome = run(&fx, talk, &FakeTranscriber::new(w, 600_000), turns)
        .await
        .unwrap();
    assert_eq!(outcome, AudioOutcome::NoMedia);
    let mut conn = fx.pool.acquire().await.unwrap();
    let reasons = vec!["no_media".to_owned()];
    let queued = audio::retry_held(&mut conn, &reasons, "test")
        .await
        .unwrap();
    assert_eq!(queued, vec![talk]);
    assert_eq!(state(&fx.pool, talk).await, ("cleared".into(), None));
    let again = audio::retry_held(&mut conn, &reasons, "test")
        .await
        .unwrap();
    assert!(again.is_empty(), "nothing is held any more");
    let jobs: i64 = sqlx::query_scalar("select count(*) from job where kind = 'audio'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    let other = audio::retry_held(&mut conn, &["multi_speaker".to_owned()], "test")
        .await
        .unwrap();
    assert!(other.is_empty());
}

// ---- Spec 04, Student questions -------------------------------------------------------------

/// An editor whose every call fails, as with a misconfigured API key.
struct FailingEditor;

#[async_trait::async_trait]
impl QuestionEditor for FailingEditor {
    fn model(&self) -> &str {
        "failing"
    }

    async fn edit(
        &self,
        _turn: &wt_segment::AnsweredTurn,
    ) -> Result<wt_segment::QuestionEdit, wt_segment::EditError> {
        Err(wt_segment::EditError::Failed("400".into()))
    }
}

/// Edits that keep the student's first eight words, behind a word of the editor's own.
fn keeping_editor() -> FakeEditor<impl Fn(&str) -> String + Send + Sync> {
    FakeEditor::new(|s: &str| {
        let w: Vec<&str> = s.split_whitespace().take(8).collect();
        format!("Teacher, about {}?", w.join(" "))
    })
}

/// Indexes the talk with the fake embedder, which stores its staged questions.
async fn index(fx: &Fx, talk: TalkId) -> wt_ingest::textjobs::Indexed {
    let mut conn = fx.pool.acquire().await.unwrap();
    let glossary = wt_registry::glossary::Glossary::new(&[]).unwrap();
    wt_ingest::textjobs::run_index(
        &mut conn,
        talk,
        &fx.objects(),
        &glossary,
        &wt_index::ChunkSettings::default(),
        &wt_ml::FakeEmbedder::new("fake-bge-m3"),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn questions(pool: &PgPool, talk: TalkId) -> Vec<String> {
    sqlx::query_scalar("select text from student_question where talk_id = $1")
        .bind(talk.as_uuid())
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_text_only_talk_stores_its_students_question_as_edited() {
    let fx = start().await;
    let talk = cleared_text_only_talk(&fx, &labeled_text("t")).await;
    let (w, turns) = timeline();
    let editor = keeping_editor();
    let outcome = run_editing(
        &fx,
        talk,
        &FakeTranscriber::new(w, 600_000),
        turns,
        Some(&editor),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { questions: 1, .. }),
        "{outcome:?}"
    );
    assert!(fx.server.received_requests().await.unwrap().is_empty());
    let (_, stored) = stored_words(&fx, talk);
    assert!(!stored.iter().any(|w| w.starts_with('s')));
    let indexed = index(&fx, talk).await;
    assert_eq!(indexed.questions, 1);
    let edit = format!(
        "Teacher, about {}?",
        (0..8)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert_eq!(questions(&fx.pool, talk).await, [edit]);
}

#[tokio::test]
async fn an_edit_with_identifying_details_or_a_rewrite_is_not_stored() {
    let leaks: [fn(&str) -> String; 5] = [
        |s| format!("Ada Fixture, {s}"),
        |s| format!("{s} write me at sam@example.org"),
        |s| format!("{s} call 415 555 0100"),
        |s| format!("{s} see www.example.org"),
        |_| "Simply return attention gently, noticing thoughts without judgment.".into(),
    ];
    for leak in leaks {
        let fx = start().await;
        let talk = cleared_text_only_talk(&fx, &labeled_text("t")).await;
        let (w, turns) = timeline();
        let editor = FakeEditor::new(leak);
        let outcome = run_editing(
            &fx,
            talk,
            &FakeTranscriber::new(w, 600_000),
            turns,
            Some(&editor),
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, AudioOutcome::Segmented { questions: 0, .. }),
            "{outcome:?}"
        );
        assert!(
            fx.objects()
                .get(&audio::questions_key(talk))
                .unwrap()
                .is_none()
        );
        index(&fx, talk).await;
        assert!(questions(&fx.pool, talk).await.is_empty());
        // The transcript keeps the bare mark.
        let mut conn = fx.pool.acquire().await.unwrap();
        let (blocks, _) = wt_ingest::owner_export::talk_blocks(
            &mut conn,
            &fx.objects(),
            &wt_registry::glossary::Glossary::new(&[]).unwrap(),
            &wt_index::ChunkSettings::default(),
            talk,
        )
        .await
        .unwrap();
        assert!(blocks.iter().any(|b| matches!(
            b,
            wt_ingest::owner_export::Block::Gap {
                label: "A student asked a question",
                ..
            }
        )));
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, wt_ingest::owner_export::Block::Question { .. }))
        );
    }
}

#[tokio::test]
async fn a_real_talk_makes_no_model_call_until_real_data_is_allowed() {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use wt_extract::{ClaudeClient, ClaudeSettings, DataClass, LlmQuestionEditor, PgLedger};
    let fx = start().await;
    let talk = cleared_text_only_talk(&fx, &labeled_text("t")).await;
    let api = MockServer::start().await;
    let models: BTreeMap<&'static str, String> = [("question", "claude-haiku-4-5".to_owned())]
        .into_iter()
        .collect();
    let client = ClaudeClient::new(
        ClaudeSettings {
            base_url: api.uri(),
            api_key: wt_core::Secret::new("fixture-key"),
            models,
            real_data_ok: false,
            workspace_id: None,
            retry_base: std::time::Duration::from_millis(1),
        },
        wt_extract::PriceTable {
            checked_on: "2026-10-10".into(),
            models: vec![wt_extract::llm::ModelPrice {
                id: "claude-haiku-4-5".into(),
                input_per_mtok: 1.0,
                output_per_mtok: 5.0,
                sampling: true,
            }],
        },
        Arc::new(PgLedger::new(fx.pool.clone(), 25.0)),
        Arc::new(wt_extract::LogAlerter),
    )
    .unwrap();
    let editor = LlmQuestionEditor::new(&client, DataClass::Real, "claude-haiku-4-5");
    let (w, turns) = timeline();
    let outcome = run_editing(
        &fx,
        talk,
        &FakeTranscriber::new(w, 600_000),
        turns,
        Some(&editor),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { questions: 0, .. }),
        "{outcome:?}"
    );
    assert!(api.received_requests().await.unwrap().is_empty(), "no call");
    index(&fx, talk).await;
    assert!(questions(&fx.pool, talk).await.is_empty());
}

#[tokio::test]
async fn the_backfill_counts_both_kinds_and_recovers_a_text_only_talks_question() {
    let fx = start().await;
    // Both talks were processed before questions were kept.
    let text_only = cleared_text_only_talk(&fx, &labeled_text("t")).await;
    let recorded = cleared_talk(&fx, true).await;
    for talk in [text_only, recorded] {
        let (w, turns) = timeline();
        run(&fx, talk, &FakeTranscriber::new(w, 600_000), turns)
            .await
            .unwrap();
    }
    let mut conn = fx.pool.acquire().await.unwrap();
    let jobs = |kind: &'static str| {
        let pool = fx.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("select count(*) from job where kind = $1")
                .bind(kind)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let before = (jobs("audio").await, jobs("questions").await);

    let dry = audio::queue_student_questions(&mut conn, None, None, true, false, false)
        .await
        .unwrap();
    assert_eq!((dry.text_only, dry.recording), (1, 1), "{dry:?}");
    assert_eq!((dry.text_only_queued, dry.recording_queued), (0, 0));
    assert_eq!((jobs("audio").await, jobs("questions").await), before);

    // Text-only first; the recording waits for a GPU run.
    let queued = audio::queue_student_questions(&mut conn, None, None, false, true, false)
        .await
        .unwrap();
    assert_eq!((queued.text_only_queued, queued.recording_queued), (1, 0));
    let again = audio::queue_student_questions(&mut conn, None, None, false, true, false)
        .await
        .unwrap();
    assert_eq!(again.text_only_queued, 0, "keyed by the transcript");
    let once_more = audio::queue_student_questions(&mut conn, None, None, false, true, true)
        .await
        .unwrap();
    assert_eq!(
        once_more.text_only_queued, 1,
        "--again queues one more round"
    );
    assert_eq!(jobs("audio").await, before.0);

    // The questions job's work: rerun the source text, no fetch, keep the transcript, stage.
    let editor = keeping_editor();
    let fetcher = MediaFetcher::new(Exclusions::default(), false).unwrap();
    let objects = fx.objects();
    let segment = SegmentSettings::default();
    let hints = AsrHints {
        glossary_prompt: String::new(),
        language: "en".into(),
    };
    let scratch = fx.scratch();
    let (w, turns) = timeline();
    let transcriber = FakeTranscriber::new(w, 600_000);
    let diarizer = FakeDiarizer::new(turns);
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
        editor: Some(&editor),
    };
    let requests = fx.server.received_requests().await.unwrap().len();
    // A failed model call fails the job, so it is retried, and stages nothing.
    let failing = AudioDeps {
        editor: Some(&FailingEditor),
        ..deps
    };
    let err = audio::backfill_questions(&mut conn, text_only, &failing, false)
        .await
        .unwrap_err();
    assert!(matches!(err, audio::AudioError::QuestionEdits(1)), "{err}");
    assert!(
        objects
            .get(&audio::questions_key(text_only))
            .unwrap()
            .is_none()
    );
    let deps = AudioDeps {
        editor: Some(&editor),
        ..failing
    };
    let out = audio::backfill_questions(&mut conn, text_only, &deps, false)
        .await
        .unwrap();
    assert_eq!(
        out,
        audio::QuestionsOutcome::Staged {
            turns: 1,
            questions: 1,
            rejected: 0
        }
    );
    assert_eq!(
        audio::backfill_questions(&mut conn, recorded, &deps, false)
            .await
            .unwrap(),
        audio::QuestionsOutcome::NeedsAudio
    );
    assert_eq!(fx.server.received_requests().await.unwrap().len(), requests);
    let versions: i64 = sqlx::query_scalar("select count(*) from transcript where talk_id = $1")
        .bind(text_only.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(versions, 1, "the transcript is kept");
    let index_jobs: i64 = sqlx::query_scalar(
        "select count(*) from job where kind = 'index' and talk_id = $1
           and payload->>'student_questions' = 'true'",
    )
    .bind(text_only.as_uuid())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(index_jobs, 1);
    index(&fx, text_only).await;
    assert_eq!(questions(&fx.pool, text_only).await.len(), 1);
}
