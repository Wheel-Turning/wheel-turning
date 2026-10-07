//! Spec 04, the `correct`, `extract` and `resolve` jobs on fixture talks, with a fake language
//! model and a real database. Every word is invented.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_core::gate::ConsentStatus;
use wt_core::ids::TalkId;
use wt_core::objects::{LocalObjects, ObjectStore};
use wt_db::test_support::TestDb;
use wt_extract::extract::Staging;
use wt_extract::{DataClass, FakeLlm};
use wt_ingest::textjobs::{
    self, PgHandList, TextOutcome, requeue_after_glossary_change, staging_key, suggestions_key,
};
use wt_registry::RegistryFile;
use wt_registry::glossary::{Glossary, GlossaryTerm, TermKind};

const REGISTRY: &str = r#"
[[sangha]]
name = "Fixture Mountain Zendo"

[[teacher]]
display_name = "Ada Fixture"
sangha = "Fixture Mountain Zendo"
aliases = ["Ada"]
"#;

struct Fx {
    _db: TestDb,
    pool: PgPool,
    objects: LocalObjects,
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
    let objects =
        LocalObjects::new(std::env::temp_dir().join(format!("wt-text-{}", Uuid::new_v4())));
    Fx {
        _db: db,
        pool,
        objects,
    }
}

/// A segmented talk by Ada whose one segment says `text`, word by word.
async fn segmented_talk(fx: &Fx, id: &str, text: &str) -> TalkId {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('vimeo', $1, 'https://vimeo.com/' || $1, 'Fixture') returning id",
    )
    .bind(id)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state,
                           consent_status, rights_basis)
         select $1, 'Fixture', id, sangha_id, 'Ada Fixture', 'cleared', 'granted', 'speaker_consent'
         from teacher where display_name = 'Ada Fixture' returning id",
    )
    .bind(video)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    sqlx::query("update talk set state = 'segmenting' where id = $1")
        .bind(talk)
        .execute(&fx.pool)
        .await
        .unwrap();
    let words: Vec<serde_json::Value> = text
        .split_whitespace()
        .enumerate()
        .map(|(i, w)| json!({ "i": i, "w": w, "s": i * 400, "e": i * 400 + 300, "p": 0.9 }))
        .collect();
    let end = i32::try_from(words.len()).unwrap() * 400;
    let key = format!("transcripts/{talk}/v1.json");
    fx.objects
        .put(
            &key,
            json!({ "schema": 1, "words": words })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let transcript: Uuid = sqlx::query_scalar(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', $2, $3) returning id",
    )
    .bind(talk)
    .bind(&key)
    .bind(format!("hash-{id}"))
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
         values ($1, $2, 0, 'talk', 0, $3, $4)",
    )
    .bind(talk)
    .bind(transcript)
    .bind(end)
    .bind(text)
    .execute(&fx.pool)
    .await
    .unwrap();
    TalkId::from(talk)
}

fn glossary(extra_variant: Option<&str>) -> Glossary {
    let mut variants = vec!["doe gen".to_owned()];
    variants.extend(extra_variant.map(str::to_owned));
    Glossary::new(&[
        GlossaryTerm {
            canonical: "Dogen".into(),
            kind: TermKind::Name,
            variants,
        },
        GlossaryTerm {
            canonical: "Genjokoan".into(),
            kind: TermKind::TextTitle,
            variants: vec!["genjo koan".into()],
        },
    ])
    .unwrap()
}

async fn segment_text(pool: &PgPool, talk: TalkId) -> String {
    sqlx::query_scalar("select text from segment where talk_id = $1")
        .bind(talk.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn queued(pool: &PgPool, talk: TalkId, kind: &str) -> i64 {
    sqlx::query_scalar("select count(*) from job where talk_id = $1 and kind = $2")
        .bind(talk.as_uuid())
        .bind(kind)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn correct_extract_and_resolve_run_in_turn() {
    let fx = start().await;
    let text = "As doe gen wrote in the genjo koan, to study the way is to study the self. \
                We sit with dohgen each morning.";
    let talk = segmented_talk(&fx, "1", text).await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let g = glossary(None);

    // Correct: canonical spellings in the segment, the stored transcript untouched.
    let out = textjobs::run_correct(&mut conn, talk, &fx.objects, &g, &|_| false)
        .await
        .unwrap();
    assert_eq!(out, TextOutcome::Done(Some(wt_db::job::JobKind::Extract)));
    let corrected = segment_text(&fx.pool, talk).await;
    assert!(
        corrected.starts_with("As Dogen wrote in the Genjokoan, to study"),
        "{corrected}"
    );
    let stored = String::from_utf8(
        fx.objects
            .get(&format!("transcripts/{talk}/v1.json"))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(
        stored.contains("\"doe\""),
        "the stored JSON stays uncorrected"
    );
    // A near miss is suggested, never applied.
    assert!(corrected.contains("dohgen"));
    let suggestions =
        String::from_utf8(fx.objects.get(&suggestions_key(talk)).unwrap().unwrap()).unwrap();
    assert!(suggestions.contains(",dohgen,Dogen,"), "{suggestions}");

    // Extract: the fabricated citation is dropped; real data is marked Real.
    let llm = FakeLlm::new(vec![Ok(json!({
        "practice_period": null,
        "citations": [
            {"text": "to study the way is to study the self", "work_hint": "Genjokoan",
             "locator_hint": null, "kind": "quotation"},
            {"text": "to forget the self", "work_hint": "Genjokoan", "locator_hint": null,
             "kind": "quotation"}
        ]
    }))]);
    let out = textjobs::run_extract(&mut conn, talk, &fx.objects, &g, &llm)
        .await
        .unwrap();
    assert_eq!(out, TextOutcome::Done(Some(wt_db::job::JobKind::Resolve)));
    assert_eq!(llm.seen.lock().unwrap()[0].1, DataClass::Real);
    let staging: Staging =
        serde_json::from_slice(&fx.objects.get(&staging_key(talk)).unwrap().unwrap()).unwrap();
    assert_eq!((staging.citations.len(), staging.discarded), (1, 1));
    // Stored-word positions: "doe gen" and "genjo koan" count as two words each.
    assert_eq!((staging.citations[0].w0, staging.citations[0].w1), (8, 17));

    // Resolve against a hand list in the database.
    let work: Uuid =
        sqlx::query_scalar("insert into work (title) values ('Genjokoan') returning id")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    sqlx::query(
        "insert into work_locator (work_id, locator, label, aliases)
         values ($1, 'genjokoan:5', 'Genjokoan, paragraph 5', '{To study the Way is to study the self}')",
    )
    .bind(work)
    .execute(&fx.pool)
    .await
    .unwrap();
    let hand_list = PgHandList::new(fx.pool.clone());
    let out = textjobs::run_resolve(&mut conn, talk, &fx.objects, &[], &hand_list)
        .await
        .unwrap();
    assert_eq!(out, TextOutcome::Done(Some(wt_db::job::JobKind::Index)));
    let staging: Staging =
        serde_json::from_slice(&fx.objects.get(&staging_key(talk)).unwrap().unwrap()).unwrap();
    let r = staging.citations[0].resolution.clone().unwrap();
    assert_eq!(
        (r.confidence.as_str(), r.method.as_deref()),
        ("resolved", Some("hand_list"))
    );
    let state: String = sqlx::query_scalar("select state::text from talk where id = $1")
        .bind(talk.as_uuid())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(state, "resolving");
}

#[tokio::test]
async fn a_glossary_change_requeues_only_the_talks_it_changes_and_skips_extraction() {
    let fx = start().await;
    let a = segmented_talk(
        &fx,
        "a",
        "We sit with dogon each morning, as doe gen taught.",
    )
    .await;
    let b = segmented_talk(&fx, "b", "Nothing here names the founder at all.").await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let old = glossary(None);
    for talk in [a, b] {
        textjobs::run_correct(&mut conn, talk, &fx.objects, &old, &|_| false)
            .await
            .unwrap();
    }
    // Pretend extraction ran for talk a, so its citations are staged.
    fx.objects
        .put(
            &staging_key(a),
            br#"{"schema":1,"talk_id":"a","practice_period":null,"citations":[],"discarded":0}"#,
        )
        .unwrap();

    let new = glossary(Some("dogon"));
    assert_ne!(old.version(), new.version());
    let affected = requeue_after_glossary_change(&mut conn, &fx.objects, &new)
        .await
        .unwrap();
    assert_eq!(affected, vec![a], "only the talk whose text changes");
    assert_eq!(queued(&fx.pool, b, "correct").await, 0);

    // The re-correction goes straight to index: citations sit at stored-word positions.
    let job = wt_db::job::claim(&mut conn, &[wt_db::job::JobKind::Correct], "w")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.talk_id, Some(a));
    let out = textjobs::run_correct(&mut conn, a, &fx.objects, &new, &|_| false)
        .await
        .unwrap();
    assert_eq!(out, TextOutcome::Done(Some(wt_db::job::JobKind::Index)));
    assert!(
        segment_text(&fx.pool, a)
            .await
            .starts_with("We sit with Dogen each morning")
    );
    assert_eq!(
        queued(&fx.pool, a, "extract").await,
        1,
        "only the first run queued extract"
    );
}

/// Owner's decision of October 7, 2026: with no model available for a real talk, extraction is
/// staged as skipped, with no citations and no input hash, and the talk goes on to `resolve`.
#[tokio::test]
async fn with_no_model_a_real_talk_is_staged_as_skipped_and_goes_on() {
    let fx = start().await;
    let talk = segmented_talk(&fx, "nomodel", "To study the way is to study the self.").await;
    let mut conn = fx.pool.acquire().await.unwrap();
    let g = glossary(None);
    textjobs::run_correct(&mut conn, talk, &fx.objects, &g, &|_| false)
        .await
        .unwrap();
    let out = textjobs::run_extract(
        &mut conn,
        talk,
        &fx.objects,
        &g,
        &wt_extract::UnavailableLlm,
    )
    .await
    .unwrap();
    assert_eq!(out, TextOutcome::Done(Some(wt_db::job::JobKind::Resolve)));
    let staging: Staging =
        serde_json::from_slice(&fx.objects.get(&staging_key(talk)).unwrap().unwrap()).unwrap();
    assert!(staging.citations.is_empty());
    assert!(staging.skipped.is_some());
    assert!(
        staging.input_hash.is_none(),
        "it runs again once a model may"
    );
    assert_eq!(queued(&fx.pool, talk, "resolve").await, 1);
}
