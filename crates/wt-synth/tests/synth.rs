//! Spec 05, M6 acceptance: the filters, the verifier and failures, with scripted models.
#![allow(clippy::unwrap_used, missing_docs)]

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use wt_core::config::LlmStage;
use wt_core::ids::PassageId;
use wt_extract::{DataClass, FakeLlm, LlmClient, LlmError, LlmRequest};
use wt_synth::{DropReason, Failure, Settings, SynthesisInput, synthesize};

fn pid(n: u128) -> PassageId {
    PassageId::from_uuid(uuid::Uuid::from_u128(n))
}

fn input() -> SynthesisInput {
    SynthesisInput {
        query: "I keep judging myself during zazen.".into(),
        passages: vec![
            (
                pid(1),
                "When judging thoughts arise in sitting, notice them gently and return to the breath."
                    .into(),
            ),
            (
                pid(2),
                "Judging is just more thinking; label it thinking and come back to posture and breath."
                    .into(),
            ),
            (
                pid(3),
                "Self criticism can become the very object of practice, met with kindness."
                    .into(),
            ),
        ],
    }
}

const GOOD: &str =
    "P1 and P2 both treat judging thoughts as thinking to notice before returning to breath.";

fn aliases() -> Vec<String> {
    vec!["Ada Fixture".into(), "Kosho".into()]
}

fn draft(themes: Vec<(&str, Vec<&str>)>) -> Value {
    json!({ "themes": themes.into_iter().map(|(d, r)| json!({"description": d, "passage_refs": r})).collect::<Vec<_>>() })
}

fn yes() -> Result<Value, String> {
    Ok(json!({"supported": true, "unsupported_part": null}))
}

fn no(part: &str) -> Result<Value, String> {
    Ok(json!({"supported": false, "unsupported_part": part}))
}

async fn run(llm: &dyn LlmClient) -> wt_synth::Synthesis {
    synthesize(
        llm,
        &input(),
        &aliases(),
        &Settings::default(),
        DataClass::Real,
    )
    .await
}

fn stages(llm: &FakeLlm) -> Vec<LlmStage> {
    llm.seen
        .lock()
        .unwrap()
        .iter()
        .map(|(r, _)| r.stage)
        .collect()
}

#[tokio::test]
async fn a_good_theme_passes_every_filter_and_maps_refs_to_ids() {
    let llm = FakeLlm::new(vec![Ok(draft(vec![(GOOD, vec!["P1", "p2", "P1"])])), yes()]);
    let out = run(&llm).await;
    assert_eq!(out.failure, None);
    assert!(out.dropped.is_empty(), "{:?}", out.dropped);
    assert_eq!(out.themes.len(), 1);
    assert_eq!(out.themes[0].description, GOOD);
    assert_eq!(
        out.themes[0].passage_ids,
        vec![pid(1), pid(2)],
        "duplicates collapse"
    );
    assert_eq!(stages(&llm), vec![LlmStage::Synth, LlmStage::Verify]);
    let seen = llm.seen.lock().unwrap();
    assert!(seen.iter().all(|(_, c)| *c == DataClass::Real));
    let verify = &seen[1].0.user;
    assert!(
        verify.contains("P1:") && verify.contains("P2:") && !verify.contains("P3:"),
        "the verifier sees only the cited passages"
    );
}

/// Each deterministic filter drops its fixture before any verifier call; the good theme in the
/// same reply passes them all.
#[tokio::test]
async fn each_filter_drops_its_fixture() {
    let long = "judging thoughts breath ".repeat(11);
    let cases: Vec<(&str, Vec<&str>, DropReason)> = vec![
        (
            "P1 and P4 both treat judging thoughts as thinking to notice.",
            vec!["P1", "P4"],
            DropReason::UnknownRef,
        ),
        (
            "P1 treats judging thoughts as thinking to notice before breath.",
            vec!["P1", "P1"],
            DropReason::TooFewPassages,
        ),
        (
            "Both notice judging thoughts. Both return to breath.",
            vec!["P1", "P2"],
            DropReason::NotOneSentence,
        ),
        (&long, vec!["P1", "P2"], DropReason::TooLong),
        (
            "Both call judging \"just more thinking\" to notice.",
            vec!["P1", "P2"],
            DropReason::QuotationMark,
        ),
        (
            "Kosho and P2 treat judging thoughts as thinking to notice.",
            vec!["P1", "P2"],
            DropReason::TeacherAlias,
        ),
        (
            "Both recommend therapy, journaling and medication for anxiety disorders.",
            vec!["P1", "P2"],
            DropReason::Ungrounded,
        ),
    ];
    for (description, refs, reason) in cases {
        let llm = FakeLlm::new(vec![
            Ok(draft(vec![
                (description, refs.clone()),
                (GOOD, vec!["P1", "P2"]),
            ])),
            yes(),
        ]);
        let out = run(&llm).await;
        assert_eq!(out.dropped.len(), 1, "{description}: {:?}", out.dropped);
        assert_eq!(out.dropped[0].reason, reason, "{description}");
        assert_eq!(out.themes.len(), 1, "{description}: the good theme passes");
        assert_eq!(
            stages(&llm),
            vec![LlmStage::Synth, LlmStage::Verify],
            "{description}: dropped before the verifier"
        );
    }
}

#[tokio::test]
async fn themes_beyond_max_themes_are_dropped() {
    let five = vec![(GOOD, vec!["P1", "P2"]); 5];
    let llm = FakeLlm::new(vec![Ok(draft(five)), yes(), yes(), yes(), yes()]);
    let out = run(&llm).await;
    assert_eq!(out.themes.len(), 4);
    assert_eq!(out.dropped.len(), 1);
    assert_eq!(out.dropped[0].reason, DropReason::OverMaxThemes);
}

#[tokio::test]
async fn a_reply_off_the_schema_fails_without_a_line() {
    let llm = FakeLlm::new(vec![
        Ok(json!({"lines": []})),
        Ok(json!({"themes": "none"})),
    ]);
    let out = run(&llm).await;
    assert!(out.themes.is_empty());
    assert_eq!(
        out.failure.as_ref().map(Failure::as_str),
        Some("synthesis_failed")
    );
}

#[tokio::test]
async fn a_verifier_that_rejects_a_theme_drops_it_with_the_reason() {
    let other = "P2 and P3 both treat judging as the object of practice met with kindness.";
    let llm = FakeLlm::new(vec![
        Ok(draft(vec![
            (GOOD, vec!["P1", "P2"]),
            (other, vec!["P2", "P3"]),
        ])),
        yes(),
        no("met with kindness"),
    ]);
    let out = run(&llm).await;
    assert_eq!(out.failure, None);
    assert_eq!(out.themes.len(), 1);
    assert_eq!(out.themes[0].description, GOOD);
    assert_eq!(out.dropped.len(), 1);
    assert_eq!(out.dropped[0].reason, DropReason::Unsupported);
    assert_eq!(out.dropped[0].reason.as_str(), "unsupported");
    assert_eq!(out.dropped[0].detail.as_deref(), Some("met with kindness"));
}

#[tokio::test]
async fn a_verifier_failure_removes_the_whole_line() {
    let llm = FakeLlm::new(vec![Ok(draft(vec![(GOOD, vec!["P1", "P2"])]))]);
    let out = run(&llm).await;
    assert!(out.themes.is_empty());
    assert!(matches!(
        out.failure,
        Some(Failure::Failed {
            stage: "verify",
            ..
        })
    ));
}

#[tokio::test]
async fn fewer_than_two_passages_makes_no_call() {
    let llm = FakeLlm::new(vec![]);
    let mut one = input();
    one.passages.truncate(1);
    let out = synthesize(
        &llm,
        &one,
        &aliases(),
        &Settings::default(),
        DataClass::Real,
    )
    .await;
    assert_eq!(out, wt_synth::Synthesis::default());
    assert!(llm.seen.lock().unwrap().is_empty());
}

/// A model that answers after a delay, or refuses at the cap.
struct Slow {
    delay: Duration,
    cap: bool,
}

#[async_trait]
impl LlmClient for Slow {
    async fn complete_value(
        &self,
        _: &LlmRequest,
        _: &Value,
        _: DataClass,
    ) -> Result<Value, LlmError> {
        if self.cap {
            return Err(LlmError::CapReached {
                month_to_date: 60.0,
                cap: 60.0,
            });
        }
        tokio::time::sleep(self.delay).await;
        Ok(draft(vec![(GOOD, vec!["P1", "P2"])]))
    }
}

#[tokio::test]
async fn a_generation_timeout_omits_the_line() {
    let settings = Settings {
        timeout: Duration::from_millis(20),
        ..Settings::default()
    };
    let slow = Slow {
        delay: Duration::from_secs(5),
        cap: false,
    };
    let out = synthesize(&slow, &input(), &aliases(), &settings, DataClass::Real).await;
    assert!(out.themes.is_empty());
    assert!(matches!(
        out.failure,
        Some(Failure::Failed { stage: "synth", .. })
    ));
}

#[tokio::test]
async fn a_refusal_at_the_cap_is_logged_as_cap_reached() {
    let capped = Slow {
        delay: Duration::ZERO,
        cap: true,
    };
    let out = run(&capped).await;
    assert!(out.themes.is_empty());
    assert_eq!(out.failure, Some(Failure::CapReached));
    assert_eq!(
        out.failure.as_ref().map(Failure::as_str),
        Some("cap_reached")
    );
}

/// Collects formatted log lines.
#[derive(Clone, Default)]
struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn every_drop_and_its_reason_land_in_the_log() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let llm = FakeLlm::new(vec![
        Ok(draft(vec![
            (GOOD, vec!["P1", "P2"]),
            (
                "Kosho treats judging thoughts as thinking.",
                vec!["P1", "P2"],
            ),
        ])),
        no("before returning to breath"),
    ]);
    let out = run(&llm).await;
    assert!(out.themes.is_empty());
    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("synthesis theme dropped"), "{log}");
    assert!(log.contains("reason=\"teacher_alias\""), "{log}");
    assert!(log.contains("reason=\"unsupported\""), "{log}");
    assert!(log.contains("before returning to breath"), "{log}");
}
