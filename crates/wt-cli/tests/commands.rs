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
