//! Spec 05, M6 acceptance for the summary: the filters, quotations taken by code, the verifier
//! and failures, with scripted models and synthetic excerpts.
#![allow(clippy::unwrap_used, missing_docs)]

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use wt_core::answer::{SummaryPart, SummaryStatus};
use wt_core::config::LlmStage;
use wt_core::excerpt::{Excerpt, Window, WindowPassage};
use wt_core::ids::{PassageId, TeacherId};
use wt_extract::{DataClass, FakeLlm, LlmClient, LlmError, LlmRequest};
use wt_synth::{DropReason, Failure, Settings, SummaryExcerpt, SummaryInput, Synthesis, summarize};

fn pid(n: u128) -> PassageId {
    PassageId::from_uuid(uuid::Uuid::from_u128(n))
}

fn tid(n: u128) -> TeacherId {
    TeacherId::from_uuid(uuid::Uuid::from_u128(100 + n))
}

fn row(n: u128, text: &str) -> WindowPassage {
    WindowPassage {
        id: pid(n),
        text: text.into(),
        ordinal: 0,
        start_ms: 0,
        sentence_starts: Vec::new(),
        sentence_start_ms: Vec::new(),
    }
}

const ADA: &str = "When judging thoughts arise in sitting, notice them gently. \
                   Then return to the breath and the posture.";
const BO: &str = "Judging is just more thinking. \
                  Label it thinking and come back to posture and breath.";
const CY: &str = "Self criticism can become the very object of practice. \
                  Meet it with kindness when it comes.";

fn excerpt(n: u128, teacher: &str, year: Option<i32>, text: &str) -> SummaryExcerpt {
    let window = Window::alone(row(n, text));
    let len = window.len();
    SummaryExcerpt {
        passage_id: pid(n),
        teacher_id: tid(n),
        teacher: teacher.into(),
        year,
        window,
        range: (0, len),
    }
}

fn input() -> SummaryInput {
    SummaryInput {
        query: "I keep judging myself during zazen.".into(),
        excerpts: vec![
            excerpt(1, "Ada Fixture", Some(2011), ADA),
            excerpt(2, "Bo Lindqvist", Some(2015), BO),
            excerpt(3, "Cy Moreau", None, CY),
        ],
    }
}

fn aliases() -> Vec<(TeacherId, String)> {
    vec![
        (tid(1), "Ada Fixture".into()),
        (tid(2), "Bo Lindqvist".into()),
        (tid(3), "Cy Moreau Roshi".into()),
    ]
}

fn s(text: &str, cites: &[(&str, i64, i64)]) -> Value {
    json!({
        "text": text,
        "cites": cites.iter().map(|(e, a, b)| json!({"excerpt": e, "start": a, "end": b})).collect::<Vec<_>>()
    })
}

const HEADLINE: &str =
    "Talks treat judging thoughts as thinking to notice, then return to breath and posture.";
const AGREE: &str = "In a 2011 talk, Ada Fixture says to notice judging thoughts and return to the breath, and a 2015 talk describes coming back to posture and breath.";
const DIFFER: &str = "One talk labels judging as thinking, while another makes self criticism the object of practice, met with kindness.";
const SUM_UP: &str = "The talks turn from judging thoughts back to breath, posture or kindness.";

fn good_draft() -> Value {
    json!({
        "headline": s(HEADLINE, &[("E1", 2, 2), ("E2", 2, 2)]),
        "framing": [],
        "agree": [s(AGREE, &[("E1", 1, 2), ("E2", 2, 2)])],
        "differ": [s(DIFFER, &[("E2", 1, 2), ("E3", 1, 2)])],
        "keep_in_mind": [],
        "sum_up": s(SUM_UP, &[("E1", 2, 2), ("E3", 2, 2)]),
    })
}

fn yes() -> Result<Value, String> {
    Ok(json!({"supported": true, "unsupported_part": null}))
}

fn no(part: &str) -> Result<Value, String> {
    Ok(json!({"supported": false, "unsupported_part": part}))
}

async fn run_with(llm: &dyn LlmClient, settings: &Settings) -> Synthesis {
    summarize(llm, &input(), &aliases(), settings, DataClass::Real).await
}

async fn run(llm: &dyn LlmClient) -> Synthesis {
    run_with(llm, &Settings::default()).await
}

fn stages(llm: &FakeLlm) -> Vec<LlmStage> {
    llm.seen
        .lock()
        .unwrap()
        .iter()
        .map(|(r, _)| r.stage)
        .collect()
}

fn reasons(out: &Synthesis) -> Vec<DropReason> {
    out.dropped.iter().map(|d| d.reason).collect()
}

#[tokio::test]
async fn a_good_summary_passes_and_each_citation_becomes_a_verbatim_quotation() {
    let llm = FakeLlm::new(vec![Ok(good_draft()), yes(), yes(), yes(), yes()]);
    let out = run(&llm).await;
    assert_eq!(out.failure, None);
    assert!(out.dropped.is_empty(), "{:?}", out.dropped);
    assert_eq!(out.status(), SummaryStatus::Shown);
    let summary = out.summary.unwrap();
    let parts: Vec<SummaryPart> = summary.sentences.iter().map(|s| s.part).collect();
    assert_eq!(
        parts,
        vec![
            SummaryPart::Headline,
            SummaryPart::Agree,
            SummaryPart::Differ,
            SummaryPart::SumUp
        ]
    );
    assert_eq!(summary.sentences[0].text, HEADLINE);
    let headline = &summary.sentences[0];
    assert_eq!(headline.passage_ids(), vec![pid(1), pid(2)]);
    // The code slices the cited sentences from the excerpt; the model sent no words of them.
    let rows = [row(1, ADA), row(2, BO), row(3, CY)];
    let quote = |c: &wt_core::answer::StoredCite| Excerpt::rebuild(&c.quote, &rows).unwrap().text;
    assert_eq!(
        quote(&headline.cites[0]),
        "Then return to the breath and the posture."
    );
    assert_eq!(
        quote(&headline.cites[1]),
        "Label it thinking and come back to posture and breath."
    );
    assert_eq!(quote(&summary.sentences[1].cites[0]), ADA);

    // One generation call, then one verifier call per sentence, all with real-data rules.
    assert_eq!(
        stages(&llm),
        vec![
            LlmStage::Synth,
            LlmStage::Verify,
            LlmStage::Verify,
            LlmStage::Verify,
            LlmStage::Verify
        ]
    );
    let seen = llm.seen.lock().unwrap();
    assert!(seen.iter().all(|(_, c)| *c == DataClass::Real));
    let generation = &seen[0].0.user;
    assert!(
        generation.contains("E1: Ada Fixture, talk from 2011"),
        "{generation}"
    );
    assert!(
        generation.contains("E3: Cy Moreau, undated talk"),
        "{generation}"
    );
    assert!(generation.contains("[2] (8 words) Then return to the breath and the posture."));
    // The verifier sees only the sentence's own excerpts.
    let verify_headline = &seen[1].0.user;
    assert!(verify_headline.contains(HEADLINE));
    assert!(verify_headline.contains("E2: Bo Lindqvist") && !verify_headline.contains("E3"));
}

#[tokio::test]
async fn each_filter_drops_its_fixture_before_the_verifier() {
    let draft = json!({
        "headline": s(HEADLINE, &[("E1", 2, 2), ("E2", 2, 2)]),
        "framing": [
            s("The talks call it \"just thinking\" and return to breath.", &[("E2", 1, 1)]),
            s("E2 labels judging thoughts as thinking.", &[("E2", 1, 2)]),
        ],
        "agree": [
            s("Two talks say to notice judging thoughts and return to the breath.", &[("E1", 1, 2)]),
            s("Talks say judging thoughts return to breath.", &[("E9", 1, 1), ("E2", 1, 1)]),
            s("Talks say judging thoughts return to breath.", &[]),
        ],
        "differ": [
            s("In a 2019 talk, Ada Fixture says to notice judging thoughts, unlike labeling them thinking.", &[("E1", 1, 1), ("E2", 1, 1)]),
            s("Ada Fixture believes judging thoughts pass, while another talk labels judging as thinking.", &[("E1", 1, 1), ("E2", 1, 1)]),
            s("Therapy and medication cure anxiety disorders, unlike journaling.", &[("E1", 1, 1), ("E3", 1, 1)]),
            s("Talks differ. One stresses breath.", &[("E1", 1, 2), ("E2", 1, 2)]),
        ],
        "keep_in_mind": [
            s("Lindqvist says to notice judging thoughts gently in sitting.", &[("E1", 1, 1)]),
        ],
        "sum_up": null,
    });
    let settings = Settings {
        max_themes: 4,
        ..Settings::default()
    };
    let llm = FakeLlm::new(vec![Ok(draft), yes()]);
    let out = run_with(&llm, &settings).await;
    assert_eq!(
        reasons(&out),
        vec![
            DropReason::QuotationMark,
            DropReason::PassageLabel,
            DropReason::OneTeacher,
            DropReason::UnknownRef,
            DropReason::NoCitation,
            DropReason::WrongYear,
            DropReason::BeliefWord,
            DropReason::Ungrounded,
            DropReason::NotOneSentence,
            DropReason::Misattributed,
        ]
    );
    assert_eq!(out.dropped[5].detail.as_deref(), Some("2019"));
    assert_eq!(out.dropped[9].detail.as_deref(), Some("Lindqvist"));
    // Only the headline reached the verifier.
    assert_eq!(stages(&llm), vec![LlmStage::Synth, LlmStage::Verify]);
    assert_eq!(out.summary.unwrap().sentences.len(), 1);
}

#[tokio::test]
async fn limits_on_sections_and_quotations_drop_the_excess() {
    let draft = json!({
        "headline": s(HEADLINE, &[("E1", 1, 2)]),
        "framing": [],
        "agree": [
            s(AGREE, &[("E1", 2, 2), ("E2", 2, 2)]),
            s(AGREE, &[("E1", 2, 2), ("E2", 2, 2)]),
        ],
        "differ": [],
        "keep_in_mind": [],
        "sum_up": null,
    });
    let settings = Settings {
        max_themes: 1,
        quote_max_words: 10,
        ..Settings::default()
    };
    let llm = FakeLlm::new(vec![Ok(draft), yes()]);
    let out = run_with(&llm, &settings).await;
    // The headline quotes the whole 17-word excerpt; the second agree sentence is over the limit.
    assert_eq!(
        reasons(&out),
        vec![DropReason::OverLimit, DropReason::QuoteTooLong]
    );
    assert!(out.summary.is_none(), "no headline, no summary");
}

#[tokio::test]
async fn an_over_long_citation_is_left_out_while_another_remains() {
    let draft = json!({
        "headline": s(HEADLINE, &[("E1", 1, 2), ("E2", 2, 2)]),
        "framing": [], "agree": [], "differ": [], "keep_in_mind": [], "sum_up": null,
    });
    let settings = Settings {
        quote_max_words: 10,
        ..Settings::default()
    };
    let llm = FakeLlm::new(vec![Ok(draft), yes()]);
    let out = run_with(&llm, &settings).await;
    assert!(out.dropped.is_empty(), "{:?}", out.dropped);
    let headline = &out.summary.unwrap().sentences[0];
    assert_eq!(
        headline.passage_ids(),
        vec![pid(2)],
        "only the short quotation stays"
    );
}

/// A draft that loses its headline is revised once, told why; a revision that keeps it gives
/// the summary (October 15, 2026).
#[tokio::test]
async fn a_lost_headline_gets_one_revision_told_why() {
    let llm = FakeLlm::new(vec![
        Ok(good_draft()),
        no("posture"),
        yes(),
        yes(),
        yes(),
        Ok(good_draft()),
        yes(),
        yes(),
        yes(),
        yes(),
    ]);
    let out = run(&llm).await;
    assert!(out.summary.is_some(), "{:?}", out.dropped);
    assert_eq!(out.failure, None);
    assert_eq!(out.calls, 10);
    assert_eq!(out.dropped[0].reason, DropReason::Unsupported);
    assert_eq!(out.dropped[0].part, SummaryPart::Headline);
    assert_eq!(out.dropped[0].detail.as_deref(), Some("posture"));
    let seen = llm.seen.lock().unwrap();
    let revision = &seen[5].0.user;
    assert!(
        revision.contains("Your first draft lost these sentences")
            && revision.contains("the cited sentences do not say this"),
        "{revision}"
    );
}

#[tokio::test]
async fn a_headline_lost_twice_leaves_no_summary() {
    let llm = FakeLlm::new(vec![
        Ok(good_draft()),
        no("posture"),
        yes(),
        yes(),
        yes(),
        Ok(good_draft()),
        no("posture"),
        yes(),
        yes(),
        yes(),
    ]);
    let out = run(&llm).await;
    assert!(out.summary.is_none());
    assert_eq!(out.failure, None);
    assert_eq!(out.status(), SummaryStatus::AllSentencesRejected);
    assert_eq!(out.calls, 10, "one revision only");
}

#[tokio::test]
async fn a_rejected_section_sentence_is_dropped_and_the_rest_stays() {
    let llm = FakeLlm::new(vec![Ok(good_draft()), yes(), yes(), no("kindness"), yes()]);
    let out = run(&llm).await;
    let summary = out.summary.unwrap();
    assert_eq!(summary.sentences.len(), 3);
    assert!(
        summary
            .sentences
            .iter()
            .all(|s| s.part != SummaryPart::Differ)
    );
    assert_eq!(out.dropped.len(), 1);
}

#[tokio::test]
async fn a_verifier_failure_omits_the_summary() {
    // The generation reply parses, then the verifier has no reply left, so it fails.
    let llm = FakeLlm::new(vec![Ok(good_draft())]);
    let out = run(&llm).await;
    assert!(out.summary.is_none());
    assert!(matches!(
        out.failure,
        Some(Failure::Failed {
            stage: "verify",
            ..
        })
    ));
    assert_eq!(
        out.failure.as_ref().map(Failure::as_str),
        Some("synthesis_failed")
    );
    assert_eq!(out.status(), SummaryStatus::ModelError);
}

#[tokio::test]
async fn no_excerpts_make_no_call() {
    let llm = FakeLlm::new(vec![]);
    let empty = SummaryInput {
        query: "q".into(),
        excerpts: Vec::new(),
    };
    let out = summarize(
        &llm,
        &empty,
        &aliases(),
        &Settings::default(),
        DataClass::Real,
    )
    .await;
    assert_eq!(out, Synthesis::default());
    assert_eq!(out.status(), SummaryStatus::NotAttempted);
    assert!(llm.seen.lock().unwrap().is_empty());
}

/// A model that answers late, or refuses at the cap.
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
        Ok(good_draft())
    }
}

#[tokio::test]
async fn a_generation_timeout_omits_the_summary() {
    let settings = Settings {
        timeout: Duration::from_millis(20),
        ..Settings::default()
    };
    let slow = Slow {
        delay: Duration::from_secs(5),
        cap: false,
    };
    let out = run_with(&slow, &settings).await;
    assert!(out.summary.is_none());
    assert_eq!(out.failure, Some(Failure::TimedOut { stage: "synth" }));
    assert_eq!(out.status(), SummaryStatus::Timeout);
}

#[tokio::test]
async fn a_refusal_at_the_cap_is_logged_as_cap_reached() {
    let capped = Slow {
        delay: Duration::ZERO,
        cap: true,
    };
    let out = run(&capped).await;
    assert!(out.summary.is_none());
    assert_eq!(out.failure, Some(Failure::CapReached));
    assert_eq!(out.status(), SummaryStatus::CapReached);
}

#[tokio::test]
async fn no_model_is_reported_as_no_key() {
    let out = run(&wt_extract::UnavailableLlm).await;
    assert!(out.summary.is_none());
    assert_eq!(out.failure, Some(Failure::NoModel { stage: "synth" }));
    assert_eq!(out.status(), SummaryStatus::NoKey);
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
    // A process-wide subscriber: tracing caches each callsite's interest globally, so a
    // thread-local one can miss events while other tests run without a subscriber.
    static CAPTURED: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();
    let captured = CAPTURED.get_or_init(|| {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        tracing::callsite::rebuild_interest_cache();
        captured
    });
    let draft = json!({
        "headline": s(HEADLINE, &[("E1", 2, 2), ("E2", 2, 2)]),
        "framing": [],
        "agree": [],
        "differ": [],
        "keep_in_mind": [s("Moreau says to notice judging thoughts.", &[("E1", 1, 1)])],
        "sum_up": null,
    });
    let llm = FakeLlm::new(vec![Ok(draft), no("before returning to breath")]);
    let out = run(&llm).await;
    assert!(out.summary.is_none());
    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("summary sentence dropped"), "{log}");
    assert!(log.contains("reason=\"misattributed\""), "{log}");
    assert!(log.contains("reason=\"unsupported\""), "{log}");
    assert!(log.contains("before returning to breath"), "{log}");
}
