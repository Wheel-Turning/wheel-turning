//! The registry, consent, talk, takedown and audit commands, run as the real `wt` binary against
//! a throwaway database. Everything here is synthetic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_db::test_support::TestDb;

const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Mountain Zendo"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Mountain Zendo"
aliases = ["Ada"]
"#;

struct Fx {
    db: TestDb,
    pool: PgPool,
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    Fx { db, pool }
}

impl Fx {
    fn wt(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_wt"))
            .args(args)
            .env("DATABASE_URL", self.db.url().expose())
            .env("WT_LOG", "warn")
            .env_remove("WT_ENV")
            .env_remove("WT_DB_HOST")
            .output()
            .unwrap()
    }

    /// Runs `wt` and expects success, returning stdout parsed as JSON.
    fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.wt(args);
        assert!(out.status.success(), "wt {args:?} failed: {}", stderr(&out));
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("wt {args:?} printed no JSON ({e}): {}", stdout(&out)))
    }

    fn import_registry(&self) {
        let path = registry_file();
        let report = self.json(&[
            "registry",
            "import",
            "--file",
            path.to_str().unwrap(),
            "--by",
            "test",
        ]);
        assert_eq!(report["teachers"], 1);
        std::fs::remove_file(path).unwrap();
    }

    /// A talk by Ada in `awaiting_consent` with the given rights holder.
    async fn talk(&self, holder: Option<&str>) -> Uuid {
        let video: Uuid = sqlx::query_scalar(
            "insert into source_video (platform, external_id, url, title)
             values ('youtube', $1, 'https://example.invalid/v', 'fixture') returning id",
        )
        .bind(Uuid::new_v4().to_string())
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query_scalar(
            "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
             select $1, 'A fixture talk', id, sangha_id, $2, 'awaiting_consent' from teacher
             where display_name = 'Ada Fixture' returning id",
        )
        .bind(video)
        .bind(holder)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn state(&self, talk: Uuid) -> String {
        sqlx::query_scalar("select state::text from talk where id = $1")
            .bind(talk)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn consent_rows(&self) -> i64 {
        sqlx::query_scalar("select count(*) from speaker_consent")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

fn registry_file() -> PathBuf {
    let path = std::env::temp_dir().join(format!("wt-registry-{}.toml", Uuid::new_v4()));
    std::fs::write(&path, REGISTRY).unwrap();
    path
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[tokio::test]
async fn consent_and_rights_commands_clear_a_talk() {
    let fx = start().await;
    fx.import_registry();
    let known = fx.talk(Some("Ada Fixture")).await;
    let unknown = fx.talk(None).await;

    let out = fx.json(&[
        "consent",
        "set",
        "--teacher",
        "ada",
        "--status",
        "granted",
        "--evidence",
        "reply-1",
        "--by",
        "test",
    ]);
    assert_eq!(out["cleared"], serde_json::json!([known]));
    assert_eq!(
        out["still_waiting"],
        serde_json::json!([[unknown, "rights_unknown"]])
    );

    let out = fx.json(&[
        "talk",
        "set-rights",
        "--talk",
        &unknown.to_string(),
        "--holder",
        "speaker",
        "--by",
        "test",
    ]);
    assert_eq!(out["cleared"], serde_json::json!([unknown]));
    assert_eq!(fx.state(unknown).await, "cleared");

    let list = fx.wt(&["consent", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let text = stdout(&list);
    let row = text.lines().find(|l| l.starts_with("Ada Fixture")).unwrap();
    assert!(
        row.contains("granted") && row.contains("reply-1") && row.contains("cleared=2"),
        "{row}"
    );
}

#[tokio::test]
async fn a_decline_needs_yes_and_then_takes_the_teacher_down() {
    let fx = start().await;
    fx.import_registry();
    let t = fx.talk(Some("Ada Fixture")).await;
    fx.json(&[
        "consent",
        "set",
        "--teacher",
        "Ada",
        "--status",
        "granted",
        "--by",
        "test",
    ]);

    let refused = fx.wt(&[
        "consent",
        "set",
        "--teacher",
        "Ada",
        "--status",
        "declined",
        "--by",
        "test",
    ]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
    let preview: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["talks"], serde_json::json!([t]));
    assert_eq!(
        fx.state(t).await,
        "cleared",
        "nothing changes without --yes"
    );
    assert_eq!(fx.consent_rows().await, 1, "no consent row without --yes");

    let out = fx.json(&[
        "consent",
        "set",
        "--teacher",
        "Ada",
        "--status",
        "declined",
        "--by",
        "test",
        "--yes",
    ]);
    assert_eq!(out["takedown"]["talks"], serde_json::json!([t]));
    assert_eq!(fx.state(t).await, "withdrawn");
}

#[tokio::test]
async fn exclude_needs_yes_and_then_takes_the_talk_down() {
    let fx = start().await;
    fx.import_registry();
    let t = fx.talk(Some("Ada Fixture")).await;
    let id = t.to_string();

    let refused = fx.wt(&[
        "talk", "exclude", "--talk", &id, "--reason", "held out", "--by", "test",
    ]);
    assert!(!refused.status.success());
    assert_eq!(fx.state(t).await, "awaiting_consent");

    let out = fx.json(&[
        "talk", "exclude", "--talk", &id, "--reason", "held out", "--by", "test", "--yes",
    ]);
    assert_eq!(out["talks"], serde_json::json!([t]));
    assert_eq!(fx.state(t).await, "withdrawn");
}

#[tokio::test]
async fn takedown_previews_refuses_without_yes_and_runs_with_it() {
    let fx = start().await;
    fx.import_registry();
    let t = fx.talk(Some("Ada Fixture")).await;

    let preview = fx.json(&[
        "takedown",
        "--teacher",
        "Ada",
        "--actor",
        "test",
        "--dry-run",
    ]);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["talks"], serde_json::json!([t]));

    let refused = fx.wt(&["takedown", "--teacher", "Ada", "--actor", "test"]);
    assert!(!refused.status.success());
    assert_eq!(fx.state(t).await, "awaiting_consent");

    let report = fx.json(&["takedown", "--teacher", "Ada", "--actor", "test", "--yes"]);
    assert_eq!(report["dry_run"], false);
    assert_eq!(fx.state(t).await, "withdrawn");
}

#[tokio::test]
async fn bad_arguments_are_refused_before_touching_the_database() {
    let fx = start().await;
    for args in [
        &[
            "takedown",
            "--teacher",
            "Ada",
            "--talk",
            "00000000-0000-0000-0000-000000000000",
            "--actor",
            "x",
        ][..],
        &["takedown", "--teacher", "Ada", "--dry-run"][..],
        &[
            "takedown",
            "--teacher",
            "Ada",
            "--actor",
            "x",
            "--dry-run",
            "--yes",
        ][..],
        &[
            "talk",
            "exclude",
            "--talk",
            "not-a-uuid",
            "--reason",
            "x",
            "--by",
            "x",
        ][..],
        &[
            "consent",
            "set",
            "--teacher",
            "Ada",
            "--status",
            "maybe",
            "--by",
            "x",
        ][..],
    ] {
        let out = fx.wt(args);
        assert_eq!(out.status.code(), Some(2), "clap usage error for {args:?}");
    }
}

#[tokio::test]
async fn audit_exits_zero_when_clean_and_one_when_a_fail_check_has_findings() {
    let fx = start().await;
    fx.import_registry();

    let clean = fx.wt(&["audit"]);
    assert_eq!(clean.status.code(), Some(0), "{}", stderr(&clean));
    let report: serde_json::Value = serde_json::from_slice(&clean.stdout).unwrap();
    assert!(report["checks"].as_array().unwrap().len() >= 11);

    // A transcript row left behind for a withdrawn talk, as a restore from an old backup would.
    let t = fx.talk(Some("Ada Fixture")).await;
    fx.json(&[
        "takedown",
        "--talk",
        &t.to_string(),
        "--actor",
        "test",
        "--yes",
    ]);
    sqlx::query(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', 'restored', 'h')",
    )
    .bind(t)
    .execute(&fx.pool)
    .await
    .unwrap();

    let failing = fx.wt(&["audit"]);
    assert_eq!(failing.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&failing.stdout).unwrap();
    let check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "takedown_completeness")
        .unwrap();
    assert_eq!(check["findings"], 1);
}

/// A config directory with the committed pipeline and series files and a fixture source.
fn config_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wt-config-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config");
    for file in ["pipeline.toml", "series.toml"] {
        std::fs::copy(repo.join(file), dir.join(file)).unwrap();
    }
    std::fs::write(
        dir.join("sources.toml"),
        "[[source]]\nname = \"fixture-vimeo\"\nplatform = \"vimeo\"\ndefault_sangha = \"Fixture Mountain Zendo\"\n",
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn the_worker_drains_classify_jobs_and_the_report_lists_them() {
    let fx = start().await;
    fx.import_registry();
    let item = serde_json::json!({});
    let mut conn = fx.pool.acquire().await.unwrap();
    for (id, title, seconds) in [
        ("9001", "Ada: Uji, Part 2", 2400),
        ("9002", "Buddhist Quote", 11),
    ] {
        wt_db::ingest::record_video(
            &mut conn,
            &wt_db::ingest::NewVideo {
                source: "fixture-vimeo",
                platform: "vimeo",
                external_id: id,
                url: "https://example.invalid/v",
                title,
                description: None,
                published_at: None,
                duration_s: Some(seconds),
                channel_ref: None,
                has_captions: false,
                item: &item,
                input_hash: "h",
            },
        )
        .await
        .unwrap();
    }
    drop(conn);
    let dir = config_dir();
    let dir_arg = dir.to_str().unwrap();

    let refused = fx.wt(&[
        "worker",
        "--kinds",
        "audio",
        "--drain",
        "--config-dir",
        dir_arg,
    ]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("no handler for audio jobs"),
        "{}",
        stderr(&refused)
    );
    let unknown = fx.wt(&["worker", "--kinds", "discover", "--drain"]);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("unknown job kind"),
        "{}",
        stderr(&unknown)
    );

    let out = fx.wt(&[
        "worker",
        "--kinds",
        "classify",
        "--concurrency",
        "2",
        "--drain",
        "--config-dir",
        dir_arg,
    ]);
    assert!(out.status.success(), "worker failed: {}", stderr(&out));
    let states: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "select sv.external_id, t.state::text, t.state_reason
         from talk t join source_video sv on sv.id = t.source_video_id order by 1",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        states,
        [
            (
                "9001".to_owned(),
                "awaiting_consent".to_owned(),
                Some("consent_missing".to_owned())
            ),
            (
                "9002".to_owned(),
                "excluded".to_owned(),
                Some("non_talk".to_owned())
            ),
        ]
    );
    let done: i64 = sqlx::query_scalar("select count(*) from job where state = 'succeeded'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(done, 2);

    let page = dir.join("report.html");
    let out = fx.wt(&[
        "ingest",
        "report",
        "--out",
        page.to_str().unwrap(),
        "--config-dir",
        dir_arg,
    ]);
    assert!(out.status.success(), "report failed: {}", stderr(&out));
    let html = std::fs::read_to_string(&page).unwrap();
    assert!(html.contains("<h1>Ingest review</h1>"));
    assert!(
        html.contains("excluded: non_talk"),
        "the clip is listed with its reason"
    );

    // Discovery queued these talks under a metadata key, so the first reclassify queues both
    // once under the rules version, and a second run with the same rules queues nothing.
    let dry = fx.json(&["ingest", "reclassify", "--dry-run", "--config-dir", dir_arg]);
    assert_eq!(
        (dry["eligible"].as_u64(), dry["queued"].as_u64()),
        (Some(2), Some(2))
    );
    let first = fx.json(&["ingest", "reclassify", "--config-dir", dir_arg]);
    assert_eq!(first["queued"], 2);
    assert_eq!(first["rules_version"], dry["rules_version"]);
    let second = fx.json(&["ingest", "reclassify", "--config-dir", dir_arg]);
    assert_eq!(
        (second["queued"].as_u64(), second["already_done"].as_u64()),
        (Some(0), Some(2))
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn resolve_feed_prints_the_address_and_saves_it_only_with_yes() {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // The command reads the active exclusions before its one request.
    let fx = start().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /*/lookup?\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/lookup"))
        .and(query_param("id", "1000000001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "resultCount": 1,
            "results": [{ "collectionName": "Fixture Zendo Talks", "feedUrl": "https://example.invalid/feed.xml" }],
        })))
        .mount(&server)
        .await;

    let dir = std::env::temp_dir().join(format!("wt-feed-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let sources = dir.join("sources.toml");
    let original = "# Fixture sources. This comment must survive.\n\n\
                    [[source]]\nname = \"fixture-vimeo\"\nplatform = \"vimeo\"\ndefault_sangha = \"Fixture Mountain Zendo\"\n\n\
                    # The feed, once resolved.\n[[source]]\nname = \"fixture-podcast\"\nplatform = \"rss\"\n\
                    default_sangha = \"Fixture Mountain Zendo\"\nrole = \"audio_only\"\n";
    std::fs::write(&sources, original).unwrap();
    let lookup = format!("{}/lookup", server.uri());
    let run = |extra: &[&str]| {
        let mut args = vec![
            "source",
            "resolve-feed",
            "--apple-id",
            "1000000001",
            "--source",
            "fixture-podcast",
            "--config-dir",
            dir.to_str().unwrap(),
            "--lookup-url",
            &lookup,
        ];
        args.extend_from_slice(extra);
        Command::new(env!("CARGO_BIN_EXE_wt"))
            .args(&args)
            .env("DATABASE_URL", fx.db.url().expose())
            .env("WT_LOG", "warn")
            .env_remove("WT_ENV")
            .env_remove("WT_DB_HOST")
            .output()
            .unwrap()
    };

    let printed = run(&[]);
    assert!(printed.status.success(), "{}", stderr(&printed));
    let json: serde_json::Value = serde_json::from_slice(&printed.stdout).unwrap();
    assert_eq!(json["feed_url"], "https://example.invalid/feed.xml");
    assert_eq!(json["podcast"], "Fixture Zendo Talks");
    assert_eq!(json["saved"], false);
    assert!(stderr(&printed).contains("--yes"));
    assert_eq!(
        std::fs::read_to_string(&sources).unwrap(),
        original,
        "nothing saved without --yes"
    );

    let saved = run(&["--yes"]);
    assert!(saved.status.success(), "{}", stderr(&saved));
    let text = std::fs::read_to_string(&sources).unwrap();
    assert!(text.contains("feed_url = \"https://example.invalid/feed.xml\""));
    assert!(text.contains("# Fixture sources. This comment must survive."));
    assert!(text.contains("# The feed, once resolved."));
    let file = wt_ingest::config::SourcesFile::parse(&text, "saved").unwrap();
    assert_eq!(
        file.get("fixture-podcast").unwrap().feed_url.as_deref(),
        Some("https://example.invalid/feed.xml")
    );
    assert_eq!(
        file.get("fixture-vimeo").unwrap().feed_url,
        None,
        "only the named source changes"
    );

    // An excluded host is never asked.
    let before = server.received_requests().await.unwrap().len();
    sqlx::query(
        "insert into source_exclusion (scope, pattern, reason, created_by)
         values ('host', '127.0.0.1', 'fixture exclusion', 'test')",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    let refused = run(&[]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("fixture exclusion"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(server.received_requests().await.unwrap().len(), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn the_rights_commands_record_a_mandate_its_request_and_a_decline() {
    let fx = start().await;
    let dir = std::env::temp_dir().join(format!("wt-rights-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let records = dir.join("records");
    let records = records.to_str().unwrap();
    let draft = dir.join("draft.txt");
    std::fs::write(&draft, "May we excerpt your texts with attribution?").unwrap();
    let reply = dir.join("reply.txt");
    std::fs::write(&reply, "Please do not use them.").unwrap();

    let added = fx.json(&[
        "source",
        "mandate",
        "--key",
        "fixturepress",
        "--publisher",
        "Fixture Press",
        "--prefix",
        "https://press.example/texts/",
        "--by",
        "Owner Fixture",
        "--rationale",
        "free to download",
        "--display-notice",
        "From {title}, {publisher}.",
        "--records-dir",
        records,
    ]);
    assert_eq!(added["basis"], "mandate");
    let refused = fx.wt(&[
        "source",
        "add",
        "--key",
        "nonotice",
        "--publisher",
        "P",
        "--prefix",
        "https://p.example/",
        "--by",
        "Owner Fixture",
        "--rationale",
        "r",
    ]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("--display-notice"),
        "{}",
        stderr(&refused)
    );

    let requested = fx.json(&[
        "rights",
        "request",
        "--source",
        "fixturepress",
        "--to",
        "rights@press.example",
        "--channel",
        "email",
        "--draft-file",
        draft.to_str().unwrap(),
        "--by",
        "Owner Fixture",
        "--records-dir",
        records,
    ]);
    let key = requested["draft"].as_str().unwrap();
    let stored = std::path::Path::new(records).join(key.trim_start_matches("records/"));
    assert_eq!(
        std::fs::read_to_string(stored).unwrap(),
        "May we excerpt your texts with attribution?"
    );
    let sent = fx.json(&[
        "rights",
        "sent",
        "--source",
        "fixturepress",
        "--on",
        "2026-10-06",
        "--by",
        "Owner Fixture",
    ]);
    assert_eq!(sent["follow_up_on"], "2026-10-20");
    let due = fx.json(&["rights", "due", "--today", "2026-10-20"]);
    assert_eq!(due[0]["why"], "follow_up_due");

    let unconfirmed = fx.wt(&[
        "rights",
        "decline",
        "--source",
        "fixturepress",
        "--statement-file",
        reply.to_str().unwrap(),
        "--by",
        "Owner Fixture",
        "--records-dir",
        records,
    ]);
    assert!(!unconfirmed.status.success());
    assert!(stderr(&unconfirmed).contains("--yes"));
    let declined = fx.json(&[
        "rights",
        "decline",
        "--source",
        "fixturepress",
        "--statement-file",
        reply.to_str().unwrap(),
        "--by",
        "Owner Fixture",
        "--records-dir",
        records,
        "--yes",
    ]);
    assert_eq!(declined["exclusions"].as_array().unwrap().len(), 1);

    let again = fx.wt(&[
        "source",
        "add",
        "--key",
        "again",
        "--publisher",
        "Fixture Press",
        "--prefix",
        "https://press.example/texts/",
        "--by",
        "Owner Fixture",
        "--rationale",
        "r",
        "--display-notice",
        "n",
        "--records-dir",
        records,
    ]);
    assert!(!again.status.success());
    assert!(
        stderr(&again).contains("refused by exclusion"),
        "{}",
        stderr(&again)
    );

    let ledger = fx.json(&["rights", "ledger", "--source", "fixturepress"]);
    let events: Vec<_> = ledger
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        events,
        [
            "mandated",
            "request_drafted",
            "request_sent",
            "permission_declined",
            "disabled",
            "exclusion_added"
        ]
    );
    let listed = fx.json(&["exclusion", "list"]);
    assert_eq!(listed[0]["pattern"], "https://press.example/texts/");
    let _ = std::fs::remove_dir_all(&dir);
}
