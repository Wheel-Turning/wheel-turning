//! Spec 03, the storage adapter and Seattle Soto Zen sources item 2c: files the owner received
//! are matched to Vimeo talks by `wt ingest match-storage`, and a matched talk whose Vimeo file
//! the API does not offer copies its storage file into scratch. The files are a few synthetic
//! bytes with a fake `ffprobe`, the Vimeo API is a local `wiremock` server, recognition and
//! diarization are fakes, and the database is real.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sqlx::PgPool;
use sqlx::types::Uuid;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wt_asr::{AsrHints, FakeTranscriber};
use wt_core::Secret;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_core::objects::LocalObjects;
use wt_core::scratch::{Converter, ScratchDir, ScratchError, WAV_NAME};
use wt_core::transcript::Word;
use wt_db::feed::{self, MediaRoute};
use wt_db::storage::{self as db_storage, StorageRef};
use wt_db::test_support::TestDb;
use wt_ingest::audio::{self, AudioDeps, AudioOutcome};
use wt_ingest::config::ClassifyConfig;
use wt_ingest::http::Exclusions;
use wt_ingest::media::{MediaFetcher, StorageMedia, VimeoMedia};
use wt_ingest::storage::{self, Prober, StorageError, StorageFile};
use wt_ingest::storagematch::{self, MatchError, SheetRow};
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

const VIMEO: &str = "fixture-vimeo";
const STORAGE: &str = "fixture-storage";
const TOKEN: &str = "fixture-vimeo-token-9d2a";
const MEDIA: &[u8] = b"fixture storage media, not audio";

/// Lengths by file name, instead of running ffprobe.
struct FakeProber(BTreeMap<&'static str, f64>);

impl Prober for FakeProber {
    fn duration_s(&self, path: &Path) -> Result<Option<f64>, StorageError> {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        Ok(self.0.get(name).copied())
    }
}

/// Writes a WAV of silence instead of running ffmpeg, deletes the fetched copy as ffmpeg's
/// converter does, and records what the copy held.
#[derive(Default)]
struct FakeConverter {
    fetched: std::sync::Mutex<Option<Vec<u8>>>,
}

impl Converter for FakeConverter {
    fn to_wav(&self, dir: &ScratchDir, input: &Path) -> Result<PathBuf, ScratchError> {
        *self.fetched.lock().unwrap() = std::fs::read(input).ok();
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
    fn root(&self) -> PathBuf {
        self.dir.join("storage")
    }

    fn scratch(&self) -> PathBuf {
        self.dir.join("scratch")
    }

    fn scratch_is_empty(&self) -> bool {
        wt_core::scratch::leftovers(&self.scratch())
            .unwrap()
            .is_empty()
    }

    /// Writes a storage file under the root.
    fn put(&self, key: &str, body: &[u8]) {
        let path = self.root().join(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn list(&self, lengths: &[(&'static str, f64)]) -> Vec<StorageFile> {
        let exts = ["mp4", "mp3"].map(String::from).to_vec();
        storage::list(
            &self.root(),
            &exts,
            &FakeProber(lengths.iter().copied().collect()),
        )
        .unwrap()
    }

    /// A fetcher with route (c), and route (b) holding the token when `with_token`.
    fn fetcher(&self, with_token: bool) -> MediaFetcher {
        let mut tokens = BTreeMap::new();
        if with_token {
            tokens.insert(VIMEO.to_owned(), Secret::new(TOKEN));
        }
        MediaFetcher::new(Exclusions::default(), false)
            .unwrap()
            .with_vimeo(VimeoMedia {
                api: Url::parse(&self.server.uri()).unwrap(),
                tokens,
            })
            .with_storage(StorageMedia {
                roots: [(STORAGE.to_owned(), self.root())].into_iter().collect(),
            })
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
    let dir = std::env::temp_dir().join(format!("wt-storage-{}", Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("storage")).unwrap();
    Fx {
        _db: db,
        pool,
        server,
        dir,
    }
}

fn cfg() -> ClassifyConfig {
    ClassifyConfig::parse("", "pipeline", "", "series").unwrap()
}

fn vimeo_sources() -> Vec<String> {
    vec![VIMEO.to_owned()]
}

/// A Vimeo talk by Ada in `state`, with its date and length.
async fn vimeo_talk(
    fx: &Fx,
    vimeo_id: &str,
    title: &str,
    date: &str,
    secs: i32,
    state: &str,
) -> TalkId {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title, duration_s, published_at, raw)
         values ('vimeo', $1, 'https://vimeo.com/' || $1, $2, $3, $4::date, $5)
         returning id",
    )
    .bind(vimeo_id)
    .bind(title)
    .bind(secs)
    .bind(date)
    .bind(serde_json::json!({ "source": VIMEO }))
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, talk_date, duration_s, teacher_id, sangha_id,
                           rights_holder, state)
         select $1, $2, $3::date, $4, id, sangha_id, 'Ada Fixture', $5::text::talk_state
         from teacher where display_name = 'Ada Fixture' returning id",
    )
    .bind(video)
    .bind(title)
    .bind(date)
    .bind(secs)
    .bind(state)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    TalkId::from(talk)
}

/// A cleared Vimeo talk with no podcast episode.
async fn cleared_talk(fx: &Fx, vimeo_id: &str) -> TalkId {
    let talk = vimeo_talk(
        fx,
        vimeo_id,
        "Ada: Fixture Talk",
        "2021-11-07",
        600,
        "awaiting_consent",
    )
    .await;
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
    talk
}

/// Records `key` as the storage file of `vimeo_id`, as `--apply` does.
async fn record(fx: &Fx, vimeo_id: &str, file: &StorageFile) {
    let mut conn = fx.pool.acquire().await.unwrap();
    let target = StorageRef {
        source: STORAGE.into(),
        key: file.key.clone(),
        bytes: file.bytes,
        fingerprint: file.fingerprint(),
    };
    db_storage::set_storage(
        &mut conn,
        &vimeo_sources(),
        vimeo_id,
        &target,
        "test",
        "test",
    )
    .await
    .unwrap();
}

async fn run(
    fx: &Fx,
    talk: TalkId,
    fetcher: &MediaFetcher,
    converter: &FakeConverter,
    fail: bool,
) -> Result<AudioOutcome, audio::AudioError> {
    let words = (0..1000)
        .map(|i| Word::new(format!("t{i}"), i * 600, i * 600 + 400, 0.9))
        .collect();
    let mut transcriber = FakeTranscriber::new(words, 600_000);
    if fail {
        transcriber.fail_with = Some("fixture recognizer failure".into());
    }
    let diarizer = FakeDiarizer::new(vec![Turn {
        speaker: "S1".into(),
        start_ms: 0,
        end_ms: 600_000,
    }]);
    let objects = LocalObjects::new(fx.dir.join("objects"));
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
    audio::run(&mut conn, talk, &deps, false).await
}

async fn storage_logs(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "select count(*) from correction_log where entity = 'source_video' and field = 'storage'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

fn statuses(rows: &[SheetRow]) -> Vec<(String, String, String)> {
    rows.iter()
        .map(|r| (r.status.clone(), r.file.clone(), r.vimeo_id.clone()))
        .collect()
}

#[tokio::test]
async fn match_storage_proposes_then_records_once_and_rediscovery_keeps_it() {
    let fx = start().await;
    vimeo_talk(
        &fx,
        "800000001",
        "Rivers and Mountains 2021-11-07",
        "2021-11-07",
        2400,
        "needs_review",
    )
    .await;
    vimeo_talk(
        &fx,
        "800000002",
        "Shobogenzo-Uji_Part-3",
        "2022-03-13",
        3000,
        "needs_review",
    )
    .await;
    vimeo_talk(
        &fx,
        "800000003",
        "An Unfiled Talk",
        "2022-05-01",
        1800,
        "needs_review",
    )
    .await;
    fx.put("2021/2021-11-07 Sunday Dharma Talk.mp4", MEDIA);
    fx.put("export/Shobogenzo Uji Part 3.mp4", MEDIA);
    fx.put("stray.mp3", b"x");
    fx.put("notes.txt", b"not media");
    let lengths = [
        ("2021-11-07 Sunday Dharma Talk.mp4", 2405.0),
        ("Shobogenzo Uji Part 3.mp4", 3010.0),
        ("stray.mp3", 999.0),
    ];
    let files = fx.list(&lengths);
    assert_eq!(files.len(), 3, "the text file is not listed");

    let mut conn = fx.pool.acquire().await.unwrap();
    let review = storagematch::review(&mut conn, &cfg(), STORAGE, &vimeo_sources(), &files)
        .await
        .unwrap();
    assert_eq!(
        statuses(&review.rows),
        [
            (
                "proposed".into(),
                "2021/2021-11-07 Sunday Dharma Talk.mp4".into(),
                "800000001".into()
            ),
            (
                "proposed".into(),
                "export/Shobogenzo Uji Part 3.mp4".into(),
                "800000002".into()
            ),
            ("unmatched_file".into(), "stray.mp3".into(), String::new()),
            ("unmatched_talk".into(), String::new(), "800000003".into()),
        ]
    );
    assert_eq!(review.rows[0].how, "recording_date");
    assert_eq!(review.rows[1].how, "title");
    assert_eq!(storage_logs(&fx.pool).await, 0, "a review changes nothing");

    let rows = storagematch::confirmed(&review, None);
    let applied = storagematch::apply(&mut conn, STORAGE, &vimeo_sources(), &files, &rows, "test")
        .await
        .unwrap();
    assert_eq!((applied.recorded, applied.unchanged), (2, 0));
    assert_eq!(storage_logs(&fx.pool).await, 2);

    // Running again on unchanged files changes nothing.
    let again = storagematch::review(&mut conn, &cfg(), STORAGE, &vimeo_sources(), &files)
        .await
        .unwrap();
    assert_eq!(again.counts.get("matched"), Some(&2));
    assert_eq!(again.counts.get("proposed"), None);
    let applied = storagematch::apply(&mut conn, STORAGE, &vimeo_sources(), &files, &rows, "test")
        .await
        .unwrap();
    assert_eq!((applied.recorded, applied.unchanged), (0, 2));
    assert_eq!(storage_logs(&fx.pool).await, 2);

    // A reviewer gives the stray file a talk by hand; a stale sheet is refused whole.
    let mut sheet = storagematch::read_sheet(&storagematch::to_csv(&again).unwrap()).unwrap();
    let stray = sheet.iter_mut().find(|r| r.file == "stray.mp3").unwrap();
    stray.accept = "yes".into();
    stray.vimeo_id = "800000003".into();
    let mut stale = sheet.clone();
    stale
        .iter_mut()
        .find(|r| r.file == "stray.mp3")
        .unwrap()
        .fingerprint = "0".repeat(64);
    let refused = storagematch::apply(
        &mut conn,
        STORAGE,
        &vimeo_sources(),
        &files,
        &storagematch::confirmed(&again, Some(&stale)),
        "test",
    )
    .await;
    assert!(
        matches!(refused, Err(MatchError::Stale { .. })),
        "{refused:?}"
    );
    let applied = storagematch::apply(
        &mut conn,
        STORAGE,
        &vimeo_sources(),
        &files,
        &storagematch::confirmed(&again, Some(&sheet)),
        "test",
    )
    .await
    .unwrap();
    assert_eq!(
        (applied.recorded, applied.unchanged),
        (1, 0),
        "only the row marked yes"
    );

    // The talk's route carries its file, and rediscovering the video keeps the match.
    let talk: Uuid = sqlx::query_scalar(
        "select t.id from talk t join source_video sv on sv.id = t.source_video_id
         where sv.external_id = '800000001'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let route = feed::media_route(&mut conn, TalkId::from(talk))
        .await
        .unwrap();
    let MediaRoute::Vimeo {
        storage: Some(file),
        ..
    } = &route
    else {
        panic!("{route:?}");
    };
    assert_eq!(file.key, "2021/2021-11-07 Sunday Dharma Talk.mp4");
    let item = serde_json::json!({ "name": "Rivers and Mountains 2021-11-07" });
    wt_db::ingest::record_video(
        &mut conn,
        &wt_db::ingest::NewVideo {
            source: VIMEO,
            platform: "vimeo",
            external_id: "800000001",
            url: "https://vimeo.com/800000001",
            title: "Rivers and Mountains 2021-11-07",
            description: None,
            published_at: None,
            duration_s: Some(2400),
            channel_ref: None,
            has_captions: false,
            item: &item,
            input_hash: "fixture-rediscovery",
            rights_source: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        feed::media_route(&mut conn, TalkId::from(talk))
            .await
            .unwrap(),
        route
    );

    // Matching never touches the owner's files.
    assert_eq!(
        std::fs::read(fx.root().join("export/Shobogenzo Uji Part 3.mp4")).unwrap(),
        MEDIA
    );
}

#[tokio::test]
async fn two_files_for_one_talk_are_a_conflict_and_nothing_is_proposed() {
    let fx = start().await;
    vimeo_talk(
        &fx,
        "800000010",
        "Shobogenzo-Uji_Part-3",
        "2022-03-13",
        3000,
        "needs_review",
    )
    .await;
    fx.put("a/Shobogenzo Uji Part 3.mp4", MEDIA);
    fx.put("b/Shobogenzo Uji Part 3 copy.mp4", MEDIA);
    let files = fx.list(&[
        ("Shobogenzo Uji Part 3.mp4", 3000.0),
        ("Shobogenzo Uji Part 3 copy.mp4", 3001.0),
    ]);
    let mut conn = fx.pool.acquire().await.unwrap();
    let review = storagematch::review(&mut conn, &cfg(), STORAGE, &vimeo_sources(), &files)
        .await
        .unwrap();
    assert_eq!(review.counts.get("conflict"), Some(&2));
    assert!(storagematch::confirmed(&review, None).is_empty());
}

/// The Vimeo API answers with no file links, as on SSZ's plan.
async fn mount_no_files(fx: &Fx, vimeo_id: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/videos/{vimeo_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&fx.server)
        .await;
}

#[tokio::test]
async fn with_no_vimeo_file_the_talk_copies_its_storage_file_and_the_original_stays() {
    let fx = start().await;
    mount_no_files(&fx, "800000020").await;
    let talk = cleared_talk(&fx, "800000020").await;
    fx.put("talk.mp4", MEDIA);
    let files = fx.list(&[("talk.mp4", 600.0)]);
    record(&fx, "800000020", &files[0]).await;

    let converter = FakeConverter::default();
    let outcome = run(&fx, talk, &fx.fetcher(true), &converter, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert_eq!(converter.fetched.lock().unwrap().as_deref(), Some(MEDIA));
    assert_eq!(
        fx.server.received_requests().await.unwrap().len(),
        1,
        "route (b) was asked first"
    );
    assert!(fx.scratch_is_empty(), "the copy went with the scratch");
    assert_eq!(
        std::fs::read(fx.root().join("talk.mp4")).unwrap(),
        MEDIA,
        "the original stays"
    );
}

#[tokio::test]
async fn without_a_token_the_storage_file_is_used_and_vimeo_is_not_asked() {
    let fx = start().await;
    let talk = cleared_talk(&fx, "800000030").await;
    fx.put("talk.mp4", MEDIA);
    let files = fx.list(&[("talk.mp4", 600.0)]);
    record(&fx, "800000030", &files[0]).await;

    let converter = FakeConverter::default();
    let outcome = run(&fx, talk, &fx.fetcher(false), &converter, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert!(fx.server.received_requests().await.unwrap().is_empty());
    assert!(fx.scratch_is_empty());
    assert!(fx.root().join("talk.mp4").exists());
}

#[tokio::test]
async fn a_refused_podcast_enclosure_falls_back_to_the_storage_file() {
    let fx = start().await;
    let talk = cleared_talk(&fx, "800000035").await;
    fx.put("talk.mp4", MEDIA);
    let files = fx.list(&[("talk.mp4", 600.0)]);
    record(&fx, "800000035", &files[0]).await;
    // A matched podcast episode whose audio host answers 403, as SSZ's old podcast audio does
    // for some talks (October 14, 2026).
    Mock::given(method("GET"))
        .and(path("/audio/talk.mp3"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&fx.server)
        .await;
    let enclosure = format!("{}/audio/talk.mp3", fx.server.uri());
    sqlx::query(
        "insert into source_video (platform, external_id, url, title, raw)
         values ('rss', 'episode-1', $1, 'Fixture episode', $2)",
    )
    .bind(&enclosure)
    .bind(serde_json::json!({
        "item": { "enclosure": { "url": enclosure, "type": "audio/mpeg" } },
        "match": { "vimeo_id": "800000035" },
    }))
    .execute(&fx.pool)
    .await
    .unwrap();

    let converter = FakeConverter::default();
    let outcome = run(&fx, talk, &fx.fetcher(false), &converter, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert_eq!(converter.fetched.lock().unwrap().as_deref(), Some(MEDIA));
    assert!(
        fx.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path() == "/audio/talk.mp3"),
        "route (a) was tried first"
    );
    assert!(fx.scratch_is_empty());
    assert_eq!(std::fs::read(fx.root().join("talk.mp4")).unwrap(), MEDIA);
}

#[tokio::test]
async fn a_failure_after_the_copy_deletes_the_scratch_and_keeps_the_original() {
    let fx = start().await;
    let talk = cleared_talk(&fx, "800000040").await;
    fx.put("talk.mp4", MEDIA);
    let files = fx.list(&[("talk.mp4", 600.0)]);
    record(&fx, "800000040", &files[0]).await;

    let converter = FakeConverter::default();
    let error = run(&fx, talk, &fx.fetcher(false), &converter, true)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("fixture recognizer failure"),
        "{error}"
    );
    assert_eq!(converter.fetched.lock().unwrap().as_deref(), Some(MEDIA));
    assert!(fx.scratch_is_empty());
    assert_eq!(std::fs::read(fx.root().join("talk.mp4")).unwrap(), MEDIA);
}

#[tokio::test]
async fn a_missing_or_changed_file_holds_the_talk_as_no_media() {
    let fx = start().await;
    let talk = cleared_talk(&fx, "800000050").await;
    fx.put("talk.mp4", MEDIA);
    let files = fx.list(&[("talk.mp4", 600.0)]);
    record(&fx, "800000050", &files[0]).await;
    fx.put("talk.mp4", b"a different, longer file than the one matched");

    let outcome = run(
        &fx,
        talk,
        &fx.fetcher(false),
        &FakeConverter::default(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(outcome, AudioOutcome::Held("no_media".into()));
    assert!(fx.scratch_is_empty());

    std::fs::remove_file(fx.root().join("talk.mp4")).unwrap();
    let mut conn = fx.pool.acquire().await.unwrap();
    audio::retry_held(&mut conn, &["no_media".to_owned()], "test")
        .await
        .unwrap();
    drop(conn);
    let outcome = run(
        &fx,
        talk,
        &fx.fetcher(false),
        &FakeConverter::default(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(outcome, AudioOutcome::Held("no_media".into()));
    assert!(fx.scratch_is_empty());
}

#[tokio::test]
async fn retry_held_moves_a_no_media_talk_onto_its_storage_file() {
    let fx = start().await;
    mount_no_files(&fx, "800000060").await;
    let talk = cleared_talk(&fx, "800000060").await;

    // Held while the API offers no file and nothing is matched.
    let outcome = run(
        &fx,
        talk,
        &fx.fetcher(true),
        &FakeConverter::default(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(outcome, AudioOutcome::Held("no_media".into()));

    // The files arrive and are matched; retry-held sends the talk back.
    fx.put("Ada - Fixture Talk.mp4", MEDIA);
    let files = fx.list(&[("Ada - Fixture Talk.mp4", 610.0)]);
    let mut conn = fx.pool.acquire().await.unwrap();
    let review = storagematch::review(&mut conn, &cfg(), STORAGE, &vimeo_sources(), &files)
        .await
        .unwrap();
    let rows = storagematch::confirmed(&review, None);
    assert_eq!(rows.len(), 1, "{:?}", review.rows);
    storagematch::apply(&mut conn, STORAGE, &vimeo_sources(), &files, &rows, "test")
        .await
        .unwrap();
    let queued = audio::retry_held(&mut conn, &["no_media".to_owned()], "test")
        .await
        .unwrap();
    assert_eq!(queued, vec![talk]);
    drop(conn);

    let converter = FakeConverter::default();
    let outcome = run(&fx, talk, &fx.fetcher(true), &converter, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, AudioOutcome::Segmented { .. }),
        "{outcome:?}"
    );
    assert_eq!(converter.fetched.lock().unwrap().as_deref(), Some(MEDIA));
    assert!(fx.scratch_is_empty());
    assert!(fx.root().join("Ada - Fixture Talk.mp4").exists());
}
