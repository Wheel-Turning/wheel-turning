//! Spec 05, Sensitive questions: the layered classifier against a labeled fixture of synthetic
//! questions, and the rule that no failing layer lowers the level. No network: the triage gate
//! is a fake, and the embedder is the hashing fake or a scripted one.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use wt_ml::{Embedder, FakeEmbedder, MlError};
use wt_retrieve::sensitive::triage::{
    self, Confidence, Relevance, Risk, Triage, TriageError, Verdict,
};
use wt_retrieve::sensitive::{Exemplars, SensitiveConfig};
use wt_retrieve::{Level, Sensitivity};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn shipped() -> (SensitiveConfig, Exemplars) {
    (
        SensitiveConfig::load(&repo().join("config/sensitive.toml")).unwrap(),
        Exemplars::load(&repo().join("config/sensitive_exemplars.toml")).unwrap(),
    )
}

#[derive(Deserialize)]
struct Fixture {
    acute: Vec<String>,
    distress: Vec<String>,
    medical: Vec<String>,
    ordinary: Vec<String>,
}

fn fixture() -> Fixture {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sensitive_questions.toml");
    toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Classifies every fixture question and checks Spec 05's three numbers. Returns the misses
/// so a failure names them.
async fn check_fixture(s: &Sensitivity, embedder: &dyn Embedder) {
    let f = fixture();
    let mut misses = Vec::new();
    let mut hits = HashMap::new();
    for (label, list, ok) in [
        (
            "acute",
            &f.acute,
            (|l: Level| l == Level::Acute) as fn(Level) -> bool,
        ),
        ("distress", &f.distress, |l| l >= Level::Distress),
        ("medical", &f.medical, |l| l >= Level::Medical),
        ("ordinary", &f.ordinary, |l| l == Level::None),
    ] {
        let mut n = 0usize;
        for q in list {
            let c = s.classify(q, embedder).await;
            if ok(c.level) {
                n += 1;
            } else {
                misses.push(format!("{label}: {q:?} -> {:?} ({:?})", c.level, c.layers));
            }
        }
        hits.insert(label, (n, list.len()));
    }
    let rate = |label: &str| {
        let (n, total) = hits[label];
        n as f64 / total as f64
    };
    assert!(
        f.ordinary.len() >= 50,
        "the ordinary set is too small to measure 5 percent"
    );
    assert_eq!(
        rate("acute"),
        1.0,
        "acute recall must be 100 percent: {misses:#?}"
    );
    assert!(rate("distress") >= 0.9, "distress recall: {misses:#?}");
    assert!(rate("medical") >= 0.9, "medical recall: {misses:#?}");
    assert!(
        1.0 - rate("ordinary") < 0.05,
        "ordinary questions flagged: {misses:#?}"
    );
}

#[tokio::test]
async fn the_labeled_fixture_meets_spec_05s_recall_and_false_alarm_limits() {
    let (config, exemplars) = shipped();
    let embedder = FakeEmbedder::new("fake");
    let s = Sensitivity::prepare(&config, &exemplars, &embedder)
        .await
        .unwrap();
    check_fixture(&s, &embedder).await;
}

#[tokio::test]
async fn the_rules_alone_catch_every_acute_question_when_the_embedder_is_down() {
    let (config, exemplars) = shipped();
    let s = Sensitivity::prepare(&config, &exemplars, &FakeEmbedder::new("fake"))
        .await
        .unwrap();
    for q in fixture().acute {
        let c = s.classify(&q, &DownEmbedder).await;
        assert_eq!(c.level, Level::Acute, "{q}");
    }
}

/// Runs the fixture through the real bge-m3 model, to calibrate the thresholds. Needs
/// `WT_MODEL_DIR`, `WT_EMBED_MODEL` and usually `WT_ORT_DYLIB`; run with `--ignored`.
#[tokio::test]
#[ignore = "needs the local embedding model"]
async fn real_embedder() {
    let dir = std::env::var("WT_MODEL_DIR").expect("WT_MODEL_DIR");
    let model = std::env::var("WT_EMBED_MODEL").unwrap_or_else(|_| "bge-m3-int8".into());
    let mut settings = wt_ml::OnnxSettings::new(dir, model);
    settings.runtime = std::env::var("WT_ORT_DYLIB").ok().map(PathBuf::from);
    let embedder = wt_ml::OnnxEmbedder::load(&settings).unwrap();
    let (config, exemplars) = shipped();
    let s = Sensitivity::prepare(&config, &exemplars, &embedder)
        .await
        .unwrap();
    // Print the closest-exemplar similarity per question, for calibration.
    let f = fixture();
    for (label, list) in [
        ("acute", &f.acute),
        ("distress", &f.distress),
        ("medical", &f.medical),
        ("ordinary", &f.ordinary),
    ] {
        for q in list {
            let c = s.classify(q, &embedder).await;
            let sims = c
                .vector
                .as_deref()
                .map(|v| s.similarities(v).map(|(_, x)| format!("{x:.3}")).join("\t"))
                .unwrap_or_else(|| "-\t-\t-".into());
            println!("{label}\t{:?}\t{sims}\t{q}", c.level);
        }
    }
    check_fixture(&s, &embedder).await;
}

// ---- layers ---------------------------------------------------------------------------------

const DIM: usize = 4;

fn basis(i: usize) -> Vec<f32> {
    let mut v = vec![0.0; DIM];
    v[i] = 1.0;
    v
}

/// Acute exemplars are axis 0, distress 1, medical 2; a question gets the vector scripted for
/// it, or axis 3, which is near no exemplar.
struct Scripted {
    exemplars: Exemplars,
    questions: HashMap<String, Vec<f32>>,
}

#[async_trait]
impl Embedder for Scripted {
    fn model_id(&self) -> &str {
        "scripted"
    }
    fn dim(&self) -> usize {
        DIM
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MlError> {
        Ok(texts
            .iter()
            .map(|t| {
                if self.exemplars.acute.contains(t) {
                    basis(0)
                } else if self.exemplars.distress.contains(t) {
                    basis(1)
                } else if self.exemplars.medical.contains(t) {
                    basis(2)
                } else {
                    self.questions.get(t).cloned().unwrap_or_else(|| basis(3))
                }
            })
            .collect())
    }
}

struct DownEmbedder;

#[async_trait]
impl Embedder for DownEmbedder {
    fn model_id(&self) -> &str {
        "down"
    }
    fn dim(&self) -> usize {
        DIM
    }
    async fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>, MlError> {
        Err(MlError::Output("the embedder is down".into()))
    }
}

/// A gate that answers a fixed verdict, fails, or hangs, and counts its calls.
struct FakeGate {
    reply: Mutex<Result<Verdict, TriageError>>,
    hang: bool,
    calls: AtomicUsize,
}

fn verdict(relevance: Relevance, risk: Risk) -> Verdict {
    Verdict {
        relevance,
        risk,
        ongoing: true,
        confidence: Confidence::High,
    }
}

impl FakeGate {
    fn says(risk: Risk) -> Self {
        Self {
            reply: Mutex::new(Ok(verdict(Relevance::Practice, risk))),
            hang: false,
            calls: AtomicUsize::new(0),
        }
    }
    fn fails() -> Self {
        Self {
            reply: Mutex::new(Err(TriageError::Failed(
                "the monthly language-model cap is reached".into(),
            ))),
            hang: false,
            calls: AtomicUsize::new(0),
        }
    }
    fn declines() -> Self {
        Self {
            reply: Mutex::new(Err(TriageError::Declined)),
            hang: false,
            calls: AtomicUsize::new(0),
        }
    }
    fn hangs() -> Self {
        Self {
            hang: true,
            ..Self::says(Risk::None)
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Triage for FakeGate {
    async fn triage(&self, _: &str) -> Result<Verdict, TriageError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hang {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        self.reply.lock().unwrap().clone()
    }
}

const PLAIN: &str = "How do I count the breath?";
const SAD: &str = "I feel hopeless about everything";

async fn scripted(questions: &[(&str, usize)]) -> (Sensitivity, Scripted) {
    let (mut config, exemplars) = shipped();
    config.triage.triage_timeout_s = 1;
    let e = Scripted {
        exemplars: exemplars.clone(),
        questions: questions
            .iter()
            .map(|(q, axis)| ((*q).to_owned(), basis(*axis)))
            .collect(),
    };
    let s = Sensitivity::prepare(&config, &exemplars, &e).await.unwrap();
    (s, e)
}

/// The first two layers, then the gate, as the ask path combines them.
async fn all_layers(s: &Sensitivity, e: &dyn Embedder, gate: &FakeGate, q: &str) -> Level {
    let c = s.classify(q, e).await;
    triage::run(Some(gate), s.triage_settings(), q)
        .await
        .raise(c.level)
}

#[tokio::test]
async fn the_embedding_layer_raises_a_question_the_rules_missed() {
    let (s, e) = scripted(&[(PLAIN, 1)]).await;
    let c = s.classify(PLAIN, &e).await;
    assert_eq!(c.layers.rules, Level::None);
    assert_eq!(c.layers.embedding, Some(Level::Distress));
    assert_eq!(c.level, Level::Distress);
    assert!(!c.degraded);
    assert_eq!(
        c.vector,
        Some(basis(1)),
        "the vector is handed on to retrieval"
    );
}

#[tokio::test]
async fn each_gate_risk_maps_to_its_level_on_an_ordinary_question() {
    let (s, e) = scripted(&[]).await;
    for (risk, level) in [
        (Risk::None, Level::None),
        (Risk::Medical, Level::Medical),
        (Risk::Distress, Level::Distress),
        (Risk::Acute, Level::Acute),
        (Risk::HarmToOthers, Level::Acute),
        (Risk::Abuse, Level::Acute),
        (Risk::Dangerous, Level::OutOfScope),
    ] {
        let gate = FakeGate::says(risk);
        assert_eq!(all_layers(&s, &e, &gate, PLAIN).await, level, "{risk:?}");
        assert_eq!(gate.calls(), 1);
    }
    let past = FakeGate {
        reply: Mutex::new(Ok(Verdict {
            ongoing: false,
            ..verdict(Relevance::Practice, Risk::Abuse)
        })),
        hang: false,
        calls: AtomicUsize::new(0),
    };
    assert_eq!(all_layers(&s, &e, &past, PLAIN).await, Level::Distress);
}

#[tokio::test]
async fn the_gate_never_lowers_the_level() {
    // The rules say distress and the gate says none.
    let (s, e) = scripted(&[]).await;
    let gate = FakeGate::says(Risk::None);
    assert_eq!(all_layers(&s, &e, &gate, SAD).await, Level::Distress);
    let gate = FakeGate::says(Risk::Medical);
    assert_eq!(all_layers(&s, &e, &gate, SAD).await, Level::Distress);
}

#[tokio::test]
async fn a_failing_slow_or_capped_gate_degrades_and_leaves_the_rules_to_decide() {
    let (s, e) = scripted(&[]).await;
    for gate in [FakeGate::fails(), FakeGate::hangs()] {
        let t = triage::run(Some(&gate), s.triage_settings(), SAD).await;
        assert_eq!(gate.calls(), 1);
        assert!(t.degraded, "triage_degraded");
        assert_eq!(t.verdict, None);
        assert_eq!(t.raise(s.classify(SAD, &e).await.level), Level::Distress);
    }
}

#[tokio::test]
async fn a_declined_classification_fails_closed_as_out_of_scope() {
    // Spec 11, PB2.0 second comparison: a model that declines to classify must not let the
    // question through. It is routed as out_of_scope, and an acute question stays acute.
    let (s, e) = scripted(&[]).await;
    for (q, level) in [
        (PLAIN, Level::OutOfScope),
        (SAD, Level::OutOfScope),
        (
            "I have the pills ready and I am going to kill myself tonight",
            Level::Acute,
        ),
    ] {
        let gate = FakeGate::declines();
        let t = triage::run(Some(&gate), s.triage_settings(), q).await;
        assert_eq!(gate.calls(), 1);
        assert!(t.declined);
        assert!(!t.degraded, "a refusal is not triage_degraded");
        assert_eq!(t.verdict, None);
        assert_eq!(
            all_layers(&s, &e, &FakeGate::declines(), q).await,
            level,
            "{q}"
        );
    }
}

#[tokio::test]
async fn a_down_embedder_degrades_to_the_rules_without_lowering() {
    let (s, _) = scripted(&[]).await;
    let c = s.classify(SAD, &DownEmbedder).await;
    assert!(c.degraded);
    assert_eq!(c.level, Level::Distress);
    assert_eq!(c.layers.embedding, None);
    assert!(c.vector.is_none());

    let c = s.classify(PLAIN, &DownEmbedder).await;
    assert!(c.degraded);
    assert_eq!(c.level, Level::None);
}

#[tokio::test]
async fn an_acute_rule_match_needs_no_other_layer() {
    let (s, _) = scripted(&[]).await;
    let c = s.classify("I want to end my life", &DownEmbedder).await;
    assert_eq!(c.level, Level::Acute);
    assert!(!c.degraded);
}

#[tokio::test]
async fn the_gate_can_be_turned_off_and_then_degrades() {
    let (mut config, exemplars) = shipped();
    config.triage.enabled = false;
    let e = FakeEmbedder::new("f");
    let s = Sensitivity::prepare(&config, &exemplars, &e).await.unwrap();
    let gate = FakeGate::says(Risk::Acute);
    let t = triage::run(Some(&gate), s.triage_settings(), PLAIN).await;
    assert_eq!(gate.calls(), 0);
    assert!(t.degraded);
}

#[tokio::test]
async fn shipped_out_of_scope_rules_and_exemplars_are_optional() {
    let (mut config, mut exemplars) = shipped();
    config.rules.out_of_scope = vec!["synthetic forbidden phrase".into()];
    exemplars.out_of_scope.clear();
    let e = FakeEmbedder::new("f");
    let s = Sensitivity::prepare(&config, &exemplars, &e).await.unwrap();
    assert_eq!(
        s.rules_level("How about a synthetic forbidden phrase?"),
        Level::OutOfScope
    );
    config.rules.out_of_scope = vec!["  ".into()];
    assert!(Sensitivity::prepare(&config, &exemplars, &e).await.is_err());
}

#[tokio::test]
async fn empty_lists_are_refused_at_start() {
    let (mut config, exemplars) = shipped();
    config.rules.acute.clear();
    assert!(
        Sensitivity::prepare(&config, &exemplars, &FakeEmbedder::new("f"))
            .await
            .is_err()
    );
    let (config, mut exemplars) = shipped();
    exemplars.medical.clear();
    assert!(
        Sensitivity::prepare(&config, &exemplars, &FakeEmbedder::new("f"))
            .await
            .is_err()
    );
}
