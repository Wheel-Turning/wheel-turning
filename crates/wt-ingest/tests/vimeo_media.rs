//! Spec 03, Seattle Soto Zen sources, items 2b and 5: a Vimeo talk with no podcast match takes
//! its file from the Vimeo API with its source's token. The API and the file host are a local
//! `wiremock` server, recognition and diarization are fakes, and the database is real. Every
//! word and file is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqlx::PgPool;
use sqlx::types::Uuid;
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_asr::{AsrHints, FakeTranscriber};
use wt_core::Secret;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_core::objects::{LocalObjects, ObjectStore};
use wt_core::scratch::{Converter, ScratchDir, ScratchError, WAV_NAME};
use wt_core::transcript::Word;
use wt_db::test_support::TestDb;
use wt_ingest::audio::{self, AudioDeps, AudioOutcome};
use wt_ingest::http::Exclusions;
use wt_ingest::media::{CAPTION_REFERENCE, MediaFetcher, VimeoMedia};
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

const SOURCE: &str = "fixture-vimeo";
const TOKEN: &str = "fixture-vimeo-token-5c1e";
const VIDEO: &str = "900000001";
const SIGNATURE: &str = "sig-77ab0";
const CAPTION: &str = "WEBVTT\n\n00:00.000 --> 00:02.000\ncaptionword alpha\n";

/// Writes a WAV of silence instead of running ffmpeg, deletes the fetched file as ffmpeg's
/// converter does, and records what the caption reference held at that moment.
#[derive(Default)]
struct FakeConverter {
    caption: std::sync::Mutex<Option<String>>,
}

impl Converter for FakeConverter {
    fn to_wav(&self, dir: &ScratchDir, input: &Path) -> Result<PathBuf, ScratchError> {
        *self.caption.lock().unwrap() =
            std::fs::read_to_string(dir.path().join(CAPTION_REFERENCE)).ok();
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

    /// A fetcher with route (b), holding the source's token when `with_token`.
    fn fetcher(&self, with_token: bool) -> MediaFetcher {
        let mut tokens = BTreeMap::new();
        if with_token {
            tokens.insert(SOURCE.to_owned(), Secret::new(TOKEN));
        }
        MediaFetcher::new(Exclusions::default(), false)
            .unwrap()
            .with_vimeo(VimeoMedia {
                api: Url::parse(&self.server.uri()).unwrap(),
                tokens,
            })
    }

    async fn requests_to(&self, prefix: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().starts_with(prefix))
            .count()
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
    let dir = std::env::temp_dir().join(format!("wt-vimeo-media-{}", Uuid::new_v4()));
    Fx {
        _db: db,
        pool,
        server,
        dir,
    }
}

/// The API answers for the video: its file links (none when `files` is false) and a text
/// track written by a person beside a machine one. Both links are signed.
async fn mount_api(fx: &Fx, files: bool) {
    let base = fx.server.uri();
    let body = if files {
        serde_json::json!({
            "download": [
                { "quality": "source", "type": "source", "size": 9000,
                  "link": format!("{base}/cdn/source.mp4?s={SIGNATURE}") }
            ],
            "files": [
                { "quality": "hls", "type": "video/mp4", "size": 1,
                  "link": format!("{base}/cdn/master.m3u8?s={SIGNATURE}") },
                { "quality": "sd", "type": "video/mp4", "size": 17,
                  "link": format!("{base}/cdn/240p.mp4?s={SIGNATURE}") }
            ]
        })
    } else {
        // A plan below Standard, or downloads off: the fields are simply absent.
        serde_json::json!({})
    };
    Mock::given(method("GET"))
        .and(path(format!("/videos/{VIDEO}")))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/videos/{VIDEO}/texttracks")))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                { "language": "en-x-autogen", "name": "English (auto-generated)",
                  "link": format!("{base}/cdn/auto.vtt?s={SIGNATURE}") },
                { "language": "en", "name": "English",
                  "link": format!("{base}/cdn/captions.vtt?s={SIGNATURE}") }
            ]
        })))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/240p.mp4"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fixture media 240p".to_vec()))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/captions.vtt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(CAPTION))
        .mount(&fx.server)
        .await;
}

/// A cleared Vimeo talk by Ada, discovered through `SOURCE`, with no podcast episode.
async fn cleared_vimeo_talk(fx: &Fx) -> TalkId {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title, raw)
         values ('vimeo', $1, 'https://vimeo.com/900000001', 'Ada: Fixture Talk', $2)
         returning id",
    )
    .bind(VIDEO)
    .bind(serde_json::json!({ "source": SOURCE }))
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
    TalkId::from(talk)
}

/// Ten minutes of the teacher alone.
fn monologue() -> (Vec<Word>, Vec<Turn>) {
    let words = (0..1000)
        .map(|i| Word::new(format!("t{i}"), i * 600, i * 600 + 400, 0.9))
        .collect();
    let turns = vec![Turn {
        speaker: "S1".into(),
        start_ms: 0,
        end_ms: 600_000,
    }];
    (words, turns)
}

async fn run(
    fx: &Fx,
    talk: TalkId,
    fetcher: &MediaFetcher,
    converter: &FakeConverter,
) -> AudioOutcome {
    let (words, turns) = monologue();
    let transcriber = FakeTranscriber::new(words, 600_000);
    let diarizer = FakeDiarizer::new(turns);
    let objects = fx.objects();
    let segment = SegmentSettings::default();
    let hints = AsrHints {
        glossary_prompt: String::new(),
        language: "en".into(),
    };
    let scratch = fx.scratch();
    let deps = AudioDeps {
        transcriber: &transcriber,
        diarizer: &diarizer,
        labeler: &FixedLabeler(Label::Talk),
        converter,
        fetcher,
        objects: &objects,
        scratch_root: &scratch,
        segment: &segment,
        hints: &hints,
        actor: "test",
        editor: None,
    };
    let mut conn = fx.pool.acquire().await.unwrap();
    audio::run(&mut conn, talk, &deps, false).await.unwrap()
}

async fn state(pool: &PgPool, talk: TalkId) -> (String, Option<String>) {
    sqlx::query_as("select state::text, state_reason from talk where id = $1")
        .bind(talk.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_vimeo_talk_fetches_its_smallest_file_with_its_sources_token() {
    let fx = start().await;
    mount_api(&fx, true).await;
    let talk = cleared_vimeo_talk(&fx).await;
    let converter = FakeConverter::default();

    let outcome = run(&fx, talk, &fx.fetcher(true), &converter).await;
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert!(fx.scratch_is_empty(), "no audio or caption left in scratch");
    assert_eq!(
        fx.requests_to("/cdn/240p.mp4").await,
        1,
        "the smallest file"
    );
    assert_eq!(fx.requests_to("/cdn/source.mp4").await, 0);
    assert_eq!(fx.requests_to("/cdn/master.m3u8").await, 0);
    assert_eq!(fx.requests_to("/robots.txt").await, 0);

    // The person-written track was in scratch as a reference; the machine one was never read.
    assert_eq!(converter.caption.lock().unwrap().as_deref(), Some(CAPTION));
    assert_eq!(fx.requests_to("/cdn/auto.vtt").await, 0);

    // The transcript is the recognizer's, never the caption's, and holds no link or token.
    let key = audio::transcript_key(talk, 1);
    let json = String::from_utf8(fx.objects().get(&key).unwrap().unwrap()).unwrap();
    assert!(json.contains("\"t0\""));
    for leak in ["captionword", SIGNATURE, TOKEN] {
        assert!(!json.contains(leak), "{leak} in the transcript");
    }
    let logged: i64 = sqlx::query_scalar(
        "select count(*) from correction_log where new_value::text like '%' || $1 || '%'
            or reason like '%' || $1 || '%'",
    )
    .bind(SIGNATURE)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(logged, 0);
}

#[tokio::test]
async fn without_a_token_the_talk_waits_as_no_media_and_nothing_is_asked() {
    let fx = start().await;
    mount_api(&fx, true).await;
    let talk = cleared_vimeo_talk(&fx).await;

    let outcome = run(&fx, talk, &fx.fetcher(false), &FakeConverter::default()).await;
    assert_eq!(outcome, AudioOutcome::NoMedia);
    assert_eq!(
        state(&fx.pool, talk).await,
        ("needs_review".into(), Some("no_media".into()))
    );
    assert!(fx.server.received_requests().await.unwrap().is_empty());
    assert!(fx.scratch_is_empty());
}

#[tokio::test]
async fn an_api_with_no_file_holds_the_talk_as_no_media() {
    let fx = start().await;
    mount_api(&fx, false).await;
    let talk = cleared_vimeo_talk(&fx).await;

    let outcome = run(&fx, talk, &fx.fetcher(true), &FakeConverter::default()).await;
    assert_eq!(outcome, AudioOutcome::Held("no_media".into()));
    assert_eq!(
        state(&fx.pool, talk).await,
        ("needs_review".into(), Some("no_media".into()))
    );
    assert_eq!(fx.requests_to("/cdn/").await, 0, "nothing was downloaded");
    assert!(fx.scratch_is_empty());
}

#[tokio::test]
async fn a_failed_download_names_the_video_and_never_the_signed_link() {
    let fx = start().await;
    let base = fx.server.uri();
    Mock::given(method("GET"))
        .and(path(format!("/videos/{VIDEO}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "files": [{ "quality": "sd", "type": "video/mp4", "size": 5,
                        "link": format!("{base}/cdn/gone.mp4?s={SIGNATURE}") }]
        })))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/gone.mp4"))
        .respond_with(ResponseTemplate::new(410))
        .mount(&fx.server)
        .await;
    let talk = cleared_vimeo_talk(&fx).await;
    let (words, turns) = monologue();
    let transcriber = FakeTranscriber::new(words, 600_000);
    let diarizer = FakeDiarizer::new(turns);
    let objects = fx.objects();
    let segment = SegmentSettings::default();
    let hints = AsrHints {
        glossary_prompt: String::new(),
        language: "en".into(),
    };
    let scratch = fx.scratch();
    let fetcher = fx.fetcher(true);
    let converter = FakeConverter::default();
    let deps = AudioDeps {
        transcriber: &transcriber,
        diarizer: &diarizer,
        labeler: &FixedLabeler(Label::Talk),
        converter: &converter,
        fetcher: &fetcher,
        objects: &objects,
        scratch_root: &scratch,
        segment: &segment,
        hints: &hints,
        actor: "test",
        editor: None,
    };
    let mut conn = fx.pool.acquire().await.unwrap();
    let error = audio::run(&mut conn, talk, &deps, false)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains(&format!("vimeo video {VIDEO}")), "{error}");
    assert!(
        !error.contains(SIGNATURE) && !error.contains("/cdn/"),
        "{error}"
    );
    assert!(!error.contains(TOKEN), "{error}");
    assert!(fx.scratch_is_empty());
}

#[tokio::test]
async fn retry_held_moves_a_no_media_talk_onto_the_vimeo_route() {
    let fx = start().await;
    mount_api(&fx, true).await;
    let talk = cleared_vimeo_talk(&fx).await;

    // Held before the token existed.
    let outcome = run(&fx, talk, &fx.fetcher(false), &FakeConverter::default()).await;
    assert_eq!(outcome, AudioOutcome::NoMedia);

    let mut conn = fx.pool.acquire().await.unwrap();
    let queued = audio::retry_held(&mut conn, &["no_media".to_owned()], "test")
        .await
        .unwrap();
    assert_eq!(queued, vec![talk]);
    drop(conn);

    let outcome = run(&fx, talk, &fx.fetcher(true), &FakeConverter::default()).await;
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert_eq!(fx.requests_to("/cdn/240p.mp4").await, 1);
    assert!(fx.scratch_is_empty());

    // `wt pipeline advance` keys a Vimeo talk by its route, so the route queues it again.
    let mut conn = fx.pool.acquire().await.unwrap();
    assert_eq!(
        wt_db::feed::media_route(&mut conn, talk).await.unwrap(),
        wt_db::feed::MediaRoute::Vimeo {
            source: SOURCE.into(),
            video_id: VIDEO.into(),
        }
    );
}
