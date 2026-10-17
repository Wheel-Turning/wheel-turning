//! Spec 07 acceptance for the evaluation harness: a set loads once, a run answers every
//! question through the ask path with stage timings, the review page works offline, ratings and
//! a 20-answer claim audit import, and the report counts and judges them. Synthetic teachers and
//! passages only; the language model is a fake.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::sync::Arc;

use serde_json::json;
use sqlx::PgPool;
use sqlx::types::Uuid;
use wt_api::Site;
use wt_core::gate::ConsentStatus;
use wt_db::test_support::TestDb;
use wt_eval::questions::{self, Question, SetRef};
use wt_eval::ratings::{self, RatingsFile};
use wt_eval::report::{self, Verdict};
use wt_eval::run::{self, RunConfig};
use wt_eval::{EvalError, review};
use wt_extract::FakeLlm;
use wt_ml::FakeEmbedder;
use wt_registry::RegistryFile;

const REGISTRY: &str = r#"
[[sangha]]
name = "North Fixture Zendo"
donation_url = "https://example.invalid/north"

[[teacher]]
display_name = "Ada Fixture"
sangha = "North Fixture Zendo"

[[teacher]]
display_name = "Bo Fixture"
sangha = "North Fixture Zendo"
"#;

const MODEL: &str = "fake-embed";
const ACTOR: &str = "test";
const ANGER: &str = "When anger arises, turn toward the anger and breathe with it. \
                     <script>alert(1)</script> The bell rang twice before the service.";
const BO_ANGER: &str = "Anger is only energy. Sit with the anger, turn toward it, and breathe \
                        until it softens on its own.";
const THEME: &str = "Both passages turn toward anger and breathe with it.";

struct Fx {
    _db: TestDb,
    pool: PgPool,
}

async fn start() -> Fx {
    let db = TestDb::start()
        .await
        .expect("docker must be running for database tests");
    let pool = wt_db::connect(db.url()).await.unwrap();
    wt_db::migrate(&pool).await.unwrap();
    let file = RegistryFile::parse(REGISTRY, "fixture").unwrap();
    wt_db::registry::import(&pool, &file, ACTOR).await.unwrap();
    passage(&pool, "Ada Fixture", "vid1", ANGER).await;
    passage(&pool, "Bo Fixture", "vid2", BO_ANGER).await;
    Fx { _db: db, pool }
}

/// A ready, consented talk by `teacher` with one passage.
async fn passage(pool: &PgPool, teacher: &str, video_id: &str, text: &str) {
    let video: Uuid = sqlx::query_scalar(
        "insert into source_video (platform, external_id, url, title)
         values ('youtube', $1, 'https://example.invalid/' || $1, 'Fixture talk') returning id",
    )
    .bind(video_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let talk: Uuid = sqlx::query_scalar(
        "insert into talk (source_video_id, title, teacher_id, sangha_id, rights_holder, state)
         select $1, 'On anger', te.id, te.sangha_id, te.display_name, 'awaiting_consent'
         from teacher te where te.display_name = $2 returning id",
    )
    .bind(video)
    .bind(teacher)
    .fetch_one(pool)
    .await
    .unwrap();
    wt_db::consent::set_consent(
        pool,
        teacher,
        ConsentStatus::Granted,
        Some("f"),
        None,
        ACTOR,
    )
    .await
    .unwrap();
    sqlx::query("update talk set state = 'ready', content_hash = 'h' where id = $1")
        .bind(talk)
        .execute(pool)
        .await
        .unwrap();
    let transcript: Uuid = sqlx::query_scalar(
        "insert into transcript (talk_id, version, origin, s3_key, content_hash)
         values ($1, 1, 'asr', 'k', 'h') returning id",
    )
    .bind(talk)
    .fetch_one(pool)
    .await
    .unwrap();
    let segment: Uuid = sqlx::query_scalar(
        "insert into segment (talk_id, transcript_id, ordinal, kind, start_ms, end_ms, text)
         values ($1, $2, 0, 'talk', 65000, 125000, $3) returning id",
    )
    .bind(talk)
    .bind(transcript)
    .bind(text)
    .fetch_one(pool)
    .await
    .unwrap();
    let v = wt_retrieve::search::vector_literal(&FakeEmbedder::vector(text));
    sqlx::query(
        "insert into passage (talk_id, segment_id, segment_kind, ordinal, start_ms, end_ms, text,
                              word_count, content_hash, embedding, embedding_model)
         values ($1, $2, 'talk', 0, 65000, 125000, $3, 20, 'h', $4::text::vector, $5)",
    )
    .bind(talk)
    .bind(segment)
    .bind(text)
    .bind(v)
    .bind(MODEL)
    .execute(pool)
    .await
    .unwrap();
}

/// A site whose model drafts a one-sentence summary per standard answer and gives each verifier verdict.
fn site(standard_answers: usize) -> Site {
    let mut replies = Vec::new();
    for _ in 0..standard_answers {
        replies.push(Ok(json!({ "headline": { "text": THEME, "cites": [
                { "excerpt": "E1", "start": 1, "end": 1 },
                { "excerpt": "E2", "start": 1, "end": 1 }
            ] }, "framing": [], "agree": [], "differ": [], "keep_in_mind": [], "sum_up": null })));
        replies.push(Ok(json!({ "supported": true, "unsupported_part": null })));
    }
    Site {
        llm: Arc::new(FakeLlm::new(replies)),
        ..Site::with_fakes(MODEL)
    }
}

fn config() -> RunConfig {
    RunConfig {
        build: "test".into(),
        commit: "abc123".into(),
        retrieval_toml: String::new(),
        pipeline_toml: None,
        models: json!({ "embed": MODEL }),
    }
}

fn q(text: &str, tags: &[&str]) -> Question {
    Question {
        text: text.into(),
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
    }
}

fn set(name: &str) -> SetRef {
    SetRef {
        name: name.into(),
        version: 1,
    }
}

#[tokio::test]
async fn a_set_loads_once_and_a_changed_set_needs_a_new_version() {
    let fx = start().await;
    let qs = vec![q("How do I work with anger?", &["multi_voice"])];
    let (id, created) = questions::load(&fx.pool, &set("p"), &qs).await.unwrap();
    assert!(created);
    let (again, created) = questions::load(&fx.pool, &set("p"), &qs).await.unwrap();
    assert_eq!((again, created), (id, false));
    let changed = vec![q("How do I work with fear?", &["multi_voice"])];
    let err = questions::load(&fx.pool, &set("p"), &changed)
        .await
        .unwrap_err();
    assert!(matches!(err, EvalError::SetChanged(_)), "{err}");
    let next = SetRef {
        name: "p".into(),
        version: 2,
    };
    assert!(questions::load(&fx.pool, &next, &changed).await.unwrap().1);
}

#[tokio::test]
async fn a_run_answers_every_question_as_the_eval_tester_with_timings() {
    let fx = start().await;
    let qs = vec![
        q("How do I work with anger?", &["multi_voice"]),
        q("What has Ada Fixture said about anger?", &["named_teacher"]),
        q(
            "Is anger ever useful in practice?",
            &["multi_voice", "draft"],
        ),
    ];
    questions::load(&fx.pool, &set("p"), &qs).await.unwrap();
    let mut seen = Vec::new();
    let run_id = run::run(&fx.pool, &site(2), &set("p"), &config(), None, |p| {
        seen.push((p.label.clone(), p.outcome.mode.clone()));
    })
    .await
    .unwrap();
    assert_eq!(
        seen,
        [
            ("q01".to_owned(), "standard".to_owned()),
            ("q02".to_owned(), "named_teacher".to_owned()),
            ("q03".to_owned(), "standard".to_owned()),
        ]
    );

    let results = run::results(&fx.pool, run_id).await.unwrap();
    assert_eq!(results.len(), 3);
    for r in &results {
        assert!(r.timings.total_ms >= r.timings.synth_ms, "{:?}", r.timings);
        assert!(r.outcome.is_some());
    }
    assert_eq!(
        results[0]
            .payload
            .summary
            .as_ref()
            .map_or(0, |s| s.sentences.len()),
        1
    );
    assert_eq!(results[1].named_teacher.as_deref(), Some("Ada Fixture"));

    // Every answer was asked as the reserved, revoked tester, and analytics leave them out.
    let testers: Vec<(String, bool)> = sqlx::query_as(
        "select distinct t.label, t.revoked_at is not null from query_log q
         join tester t on t.id = q.tester_id",
    )
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(testers, [("eval".to_owned(), true)]);
    let counted: Option<i64> =
        sqlx::query_scalar("select sum(answers)::bigint from v_answer_stats")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(counted, None);
    let err = wt_db::tester::invite(&fx.pool, "Eval", b"x")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("reserved"), "{err}");

    let stored = run::find(&fx.pool, run_id).await.unwrap();
    assert!(stored.finished_at.is_some());
    assert_eq!(stored.config["commit"], "abc123");

    // A second run reuses the eval tester.
    run::run(&fx.pool, &site(2), &set("p"), &config(), Some(1), |_| {})
        .await
        .unwrap();
    let evals: i64 = sqlx::query_scalar("select count(*) from tester where label = 'eval'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(evals, 1);

    // The report prints every metric Spec 07 lists, before any rating.
    let r = report::build(&fx.pool, run_id, report::QUESTION_PASS_RATIO)
        .await
        .unwrap();
    assert_eq!((r.answered, r.set_size), (3, 3));
    assert_eq!((r.questions_counted, r.draft_questions), (2, 1));
    assert_eq!(r.retrieval, Verdict::NotRated);
    assert_eq!(r.synthesis, Verdict::NotRated);
    assert_eq!(r.named.len(), 1);
    assert_eq!(r.named[0].teacher.as_deref(), Some("Ada Fixture"));
    assert_eq!(r.teacher_counts.values().sum::<usize>(), 3);
    assert_eq!(r.latency.len(), 10);
    // Spec 11, PB2.4: two standard answers showed their summary; the named one had none asked.
    assert_eq!(
        r.summary_statuses.get("shown"),
        Some(&2),
        "{:?}",
        r.summary_statuses
    );
    assert_eq!(r.summary_statuses.get("not_attempted"), Some(&1));
    // Spec 11, PB2.2: each excerpt served with a summary is cited, unused or neither.
    let u = &r.summary_use;
    assert_eq!(
        u.excerpts,
        u.cited + u.off_topic + u.duplicate + u.neither,
        "{u:?}"
    );
    assert!(u.cited >= 2, "{u:?}");
    // Excerpts: the fixture passages are whole sentences too short to trim, so each shows as
    // it is and none starts or ends mid-sentence.
    let shown: usize = results.iter().map(|a| a.payload.passage_ids.len()).sum();
    assert_eq!(r.excerpts.count, shown);
    assert!(r.excerpts.mean_words > 10.0, "{:?}", r.excerpts);
    assert_eq!(r.excerpts.mid_sentence, 0, "{:?}", r.excerpts);
    assert_eq!(r.excerpts.by_chooser.get("passage"), Some(&shown));
    let text = report::render(&r);
    for needle in [
        "mean",
        "mid-sentence",
        "Questions passing",
        "Claim audit",
        "Answers by teacher count",
        "Thin-coverage answers",
        "Named-teacher queries",
        "Stage latency",
        "Dana events",
        "Draft questions",
        "PRD verdict",
        "Summary status by answer: shown 2, not_attempted 1",
        "Excerpts served with a summary:",
        "with an agree or differ sentence",
    ] {
        assert!(text.contains(needle), "{needle} missing from\n{text}");
    }
}

#[tokio::test]
async fn the_review_page_works_offline_and_its_ratings_import_and_score() {
    let fx = start().await;
    let qs = vec![
        q("How do I work with anger?", &["multi_voice"]),
        q("Is anger ever useful?", &["multi_voice"]),
        q("Can anger soften on its own?", &["multi_voice", "draft"]),
    ];
    questions::load(&fx.pool, &set("p"), &qs).await.unwrap();
    let run_id = run::run(&fx.pool, &site(3), &set("p"), &config(), None, |_| {})
        .await
        .unwrap();

    let html = review::export(&fx.pool, run_id, 2000).await.unwrap();
    // Self-contained: no external scripts, styles, fonts or images.
    for banned in ["<link", " src=", "@import", "url("] {
        assert!(!html.contains(banned), "{banned} in review page");
    }
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script>alert(1)"));
    assert!(html.contains("Ada Fixture: On anger"));
    assert!(html.contains("https://www.youtube.com/watch?v=vid1&amp;t=63s"));
    assert!(html.contains(THEME));
    assert!(html.contains("value='invented'"));
    assert!(html.contains(&format!("data-run='{run_id}'")));

    let results = run::results(&fx.pool, run_id).await.unwrap();
    let mut ratings = Vec::new();
    let mut claims = Vec::new();
    for (i, r) in results.iter().enumerate() {
        for p in &r.payload.passage_ids {
            // q01 all good, q02 all bad, q03 (draft) left half unrated.
            let (f, rel) = match i {
                0 => (Some(true), Some(true)),
                1 => (Some(true), Some(false)),
                _ => (Some(true), None),
            };
            ratings.push(
                json!({ "question_id": r.label(), "passage_id": p.to_string(),
                                 "faithful": f, "relevant": rel, "note": "fine" }),
            );
        }
        let verdict = ["supported", "invented", "drifting"][i];
        claims.push(
            json!({ "question_id": r.label(), "answer_id": r.answer_id.to_string(),
                            "theme": 0, "claim": "ignored", "verdict": verdict, "note": "" }),
        );
    }
    let file: RatingsFile = serde_json::from_value(json!({
        "run": run_id.to_string(), "reviewer": "R", "rated_on": "2026-10-11",
        "ratings": ratings, "claims": claims,
    }))
    .unwrap();

    let done = ratings::import(&fx.pool, run_id, &file, "Reviewer One")
        .await
        .unwrap();
    assert_eq!((done.ratings, done.unrated, done.claims), (4, 2, 3));
    // Importing again replaces the earlier import.
    ratings::import(&fx.pool, run_id, &file, "Reviewer One")
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("select count(*) from eval_rating")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(n, 4);
    let claim_text: String = sqlx::query_scalar("select claim_text from claim_audit limit 1")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(claim_text, THEME, "claim text comes from the stored answer");

    let r = report::build(&fx.pool, run_id, report::QUESTION_PASS_RATIO)
        .await
        .unwrap();
    assert_eq!(r.reviewers, ["Reviewer One"]);
    assert_eq!((r.questions_passing, r.questions_counted), (1, 2));
    assert_eq!(r.questions[2].passes, None, "draft question half rated");
    // One of two approved questions passing is not fewer than half.
    assert_eq!(r.retrieval, Verdict::Pass);
    assert_eq!(r.claims["invented"], 1);
    assert_eq!(r.synthesis, Verdict::Fail);

    // A file for another run, or naming a passage the run did not return, writes nothing.
    let mut wrong = file.clone();
    wrong.run = Uuid::new_v4().to_string();
    assert!(
        ratings::import(&fx.pool, run_id, &wrong, "R2")
            .await
            .is_err()
    );
    let mut stray = file.clone();
    stray.ratings[0].passage_id = Uuid::new_v4().to_string();
    assert!(
        ratings::import(&fx.pool, run_id, &stray, "R2")
            .await
            .is_err()
    );
    let mut verdict = file.clone();
    verdict.claims[0].verdict = Some("great".into());
    assert!(
        ratings::import(&fx.pool, run_id, &verdict, "R2")
            .await
            .is_err()
    );
    let n: i64 = sqlx::query_scalar("select count(*) from eval_rating where reviewer = 'R2'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);

    // Fewer than half passing fails retrieval.
    let mut worse = file.clone();
    for r in &mut worse.ratings {
        r.relevant = Some(false);
    }
    ratings::import(&fx.pool, run_id, &worse, "Reviewer One")
        .await
        .unwrap();
    let r = report::build(&fx.pool, run_id, report::QUESTION_PASS_RATIO)
        .await
        .unwrap();
    assert_eq!(r.retrieval, Verdict::Fail);
}

#[tokio::test]
async fn a_twenty_answer_claim_audit_imports_and_the_report_counts_each_verdict() {
    let fx = start().await;
    let qs: Vec<Question> = (1..=20)
        .map(|n| {
            q(
                &format!("How do I work with anger, case {n}?"),
                &["multi_voice"],
            )
        })
        .collect();
    questions::load(&fx.pool, &set("audit"), &qs).await.unwrap();
    let run_id = run::run(&fx.pool, &site(20), &set("audit"), &config(), None, |_| {})
        .await
        .unwrap();
    let results = run::results(&fx.pool, run_id).await.unwrap();
    assert_eq!(results.len(), 20);
    let claims: Vec<_> = results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            assert_eq!(
                r.payload.summary.as_ref().map_or(0, |s| s.sentences.len()),
                1,
                "{}",
                r.label()
            );
            let verdict = match i {
                0..=13 => "supported",
                14..=17 => "drifting",
                _ => "invented",
            };
            json!({ "question_id": r.label(), "answer_id": r.answer_id.to_string(),
                    "theme": 0, "claim": THEME, "verdict": verdict, "note": "" })
        })
        .collect();
    let file: RatingsFile = serde_json::from_value(json!({
        "run": run_id.to_string(), "reviewer": "R", "ratings": [], "claims": claims,
    }))
    .unwrap();
    let done = ratings::import(&fx.pool, run_id, &file, "R").await.unwrap();
    assert_eq!(done.claims, 20);

    let r = report::build(&fx.pool, run_id, report::QUESTION_PASS_RATIO)
        .await
        .unwrap();
    assert_eq!(r.claims["supported"], 14);
    assert_eq!(r.claims["drifting"], 4);
    assert_eq!(r.claims["invented"], 2);
    assert_eq!(r.synthesis, Verdict::Fail);
    assert!(report::render(&r).contains("14 supported, 4 drifting, 2 invented"));

    // Comparing a run with itself shows no change.
    let c = wt_eval::compare::compare(&fx.pool, run_id, run_id)
        .await
        .unwrap();
    assert_eq!(c.changes.len(), 20);
    assert!(
        c.changes
            .iter()
            .all(|ch| (ch.overlap - 1.0).abs() < f64::EPSILON)
    );
}
