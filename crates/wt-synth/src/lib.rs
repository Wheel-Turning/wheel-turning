//! Synthesis, the synthesis validator and the verifier (Spec 05, Synthesis and verifier).
//!
//! [`synthesize`] drafts at most `max_themes` one-line themes about how the selected passages
//! approach a question, runs the deterministic filters in the spec's order, and sends each
//! surviving theme to the verifier in parallel. The model returns refs and JSON only; the code
//! maps refs back to passage ids. A failed or slow call removes the synthesis line and nothing
//! else, so the caller always renders the passages. Every drop is logged with its reason.

pub mod filter;

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::future::join_all;
use schemars::JsonSchema;
use serde::Deserialize;
use wt_core::answer::StoredTheme;
use wt_core::config::LlmStage;
use wt_core::ids::PassageId;
use wt_extract::{DataClass, LlmClient, LlmError, LlmRequest, complete_json};

/// The generation prompt.
pub const SYNTHESIZE_PROMPT: &str = include_str!("../prompts/synthesize.md");
/// The verifier prompt.
pub const VERIFY_PROMPT: &str = include_str!("../prompts/verify.md");

/// What the generation call sees: the question and the selected passages in score order. The
/// model knows them as `P1..Pn` by position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesisInput {
    /// The question as used for retrieval.
    pub query: String,
    /// The passages, best first, with their verbatim text.
    pub passages: Vec<(PassageId, String)>,
}

/// One theme as the model drafts it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ThemeDraft {
    /// One sentence of at most 30 words.
    pub description: String,
    /// The passage labels that support it, such as `P2`.
    pub passage_refs: Vec<String>,
}

/// The generation call's reply.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct SynthesisOutput {
    /// At most four themes.
    pub themes: Vec<ThemeDraft>,
}

/// The verifier's reply about one theme.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct VerifierOutput {
    /// Every assertion is stated or directly entailed by a cited passage.
    pub supported: bool,
    /// When unsupported, the part the passages do not support.
    pub unsupported_part: Option<String>,
}

/// `[synthesis]` in `config/retrieval.toml`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Themes kept from one reply.
    pub max_themes: usize,
    /// The least share of a theme's content words found in its cited passages.
    pub min_grounding_ratio: f32,
    /// The limit on each generation and verifier call.
    pub timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_themes: 4,
            min_grounding_ratio: 0.4,
            timeout: Duration::from_secs(20),
        }
    }
}

/// Why a theme was dropped. The filters run in the order of the variants up to `Ungrounded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Beyond `max_themes`.
    OverMaxThemes,
    /// A cited ref is not among the offered passages.
    UnknownRef,
    /// Fewer than two distinct passages cited.
    TooFewPassages,
    /// More than one sentence, or none.
    NotOneSentence,
    /// More than 30 words.
    TooLong,
    /// A quotation mark.
    QuotationMark,
    /// A teacher alias from the registry.
    TeacherAlias,
    /// A passage label such as P3, which would not match the page's numbering.
    PassageLabel,
    /// Too few content words found in the cited passages.
    Ungrounded,
    /// The verifier did not mark it supported.
    Unsupported,
}

impl DropReason {
    /// The reason as logged.
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::OverMaxThemes => "over_max_themes",
            DropReason::UnknownRef => "unknown_ref",
            DropReason::TooFewPassages => "too_few_passages",
            DropReason::NotOneSentence => "not_one_sentence",
            DropReason::TooLong => "too_long",
            DropReason::QuotationMark => "quotation_mark",
            DropReason::TeacherAlias => "teacher_alias",
            DropReason::PassageLabel => "passage_label",
            DropReason::Ungrounded => "ungrounded",
            DropReason::Unsupported => "unsupported",
        }
    }
}

/// A dropped theme, as the integrity audit reads it from the log.
#[derive(Debug, Clone, PartialEq)]
pub struct Dropped {
    /// Why.
    pub reason: DropReason,
    /// The drafted sentence.
    pub description: String,
    /// The refs it cited, as drafted.
    pub refs: Vec<String>,
    /// The alias found, the grounding ratio, or the verifier's unsupported part.
    pub detail: Option<String>,
}

/// Why the synthesis line is missing altogether.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The month's language-model cap is reached (Spec 04, `cap_reached`).
    CapReached,
    /// A generation or verifier call failed or ran past the timeout (`synthesis_failed`).
    Failed {
        /// `synth` or `verify`.
        stage: &'static str,
        /// What went wrong, never the request text.
        error: String,
    },
}

impl Failure {
    /// The outcome as logged.
    pub fn as_str(&self) -> &'static str {
        match self {
            Failure::CapReached => "cap_reached",
            Failure::Failed { .. } => "synthesis_failed",
        }
    }
}

/// The result of one synthesis.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Synthesis {
    /// Verified themes with the passage ids the code mapped from their refs.
    pub themes: Vec<StoredTheme>,
    /// Every theme dropped, with its reason.
    pub dropped: Vec<Dropped>,
    /// Set when a call failed; `themes` is then empty.
    pub failure: Option<Failure>,
    /// Model calls made.
    pub calls: usize,
}

/// A theme that passed the deterministic filters, waiting on the verifier.
struct Candidate {
    draft: ThemeDraft,
    /// Distinct offered positions it cites, in citing order.
    cited: Vec<usize>,
}

/// Drafts, filters and verifies themes for one standard-mode answer. Named mode must not call
/// this (Spec 05). `aliases` is every teacher alias and display name in the registry.
pub async fn synthesize(
    llm: &dyn LlmClient,
    input: &SynthesisInput,
    aliases: &[String],
    settings: &Settings,
    class: DataClass,
) -> Synthesis {
    let mut out = Synthesis::default();
    if input.passages.len() < 2 {
        // No theme can cite two passages, so no call is worth making.
        return out;
    }

    out.calls += 1;
    let request = LlmRequest {
        stage: LlmStage::Synth,
        system: SYNTHESIZE_PROMPT.to_owned(),
        user: generation_message(input),
        max_tokens: 1500,
    };
    let drafted = match tokio::time::timeout(
        settings.timeout,
        complete_json::<SynthesisOutput>(llm, &request, class),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return fail(out, "synth", e),
        Err(_) => return timed_out(out, "synth", settings.timeout),
    };

    let candidates = filter_drafts(drafted, input, aliases, settings, &mut out.dropped);

    out.calls += candidates.len();
    let verdicts = join_all(
        candidates
            .iter()
            .map(|c| verify(llm, c, input, settings, class)),
    )
    .await;
    let mut kept = Vec::new();
    for (candidate, verdict) in candidates.into_iter().zip(verdicts) {
        match verdict {
            Ok(v) if v.supported => kept.push(candidate),
            Ok(v) => out.dropped.push(Dropped {
                reason: DropReason::Unsupported,
                description: candidate.draft.description,
                refs: candidate.draft.passage_refs,
                detail: v.unsupported_part,
            }),
            Err(failure) => {
                out.failure = Some(failure);
                out.themes.clear();
                log(&out);
                return out;
            }
        }
    }
    out.themes = kept
        .into_iter()
        .map(|c| StoredTheme {
            description: c.draft.description.trim().to_owned(),
            passage_ids: c.cited.iter().map(|&i| input.passages[i].0).collect(),
        })
        .collect();
    log(&out);
    out
}

fn fail(mut out: Synthesis, stage: &'static str, e: LlmError) -> Synthesis {
    out.failure = Some(match e {
        LlmError::CapReached { .. } => Failure::CapReached,
        other => Failure::Failed {
            stage,
            error: other.to_string(),
        },
    });
    log(&out);
    out
}

fn timed_out(mut out: Synthesis, stage: &'static str, after: Duration) -> Synthesis {
    out.failure = Some(Failure::Failed {
        stage,
        error: format!("no reply within {} s", after.as_secs_f32()),
    });
    log(&out);
    out
}

/// The label of the passage at `index`.
fn label(index: usize) -> String {
    format!("P{}", index + 1)
}

/// The offered position a ref names: `P3`, `p3` or `3`.
fn position(r: &str, offered: usize) -> Option<usize> {
    let r = r.trim();
    let digits = r
        .strip_prefix('P')
        .or_else(|| r.strip_prefix('p'))
        .unwrap_or(r);
    let n: usize = digits.parse().ok()?;
    (1..=offered).contains(&n).then(|| n - 1)
}

fn generation_message(input: &SynthesisInput) -> String {
    let mut text = format!("Question: {}\n\nPassages:\n", input.query);
    for (i, (_, passage)) in input.passages.iter().enumerate() {
        text.push_str(&format!("\n{}: {}\n", label(i), passage));
    }
    text
}

/// Filters 1 to 6, in order. Survivors go to the verifier.
fn filter_drafts(
    drafted: SynthesisOutput,
    input: &SynthesisInput,
    aliases: &[String],
    settings: &Settings,
    dropped: &mut Vec<Dropped>,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for (n, draft) in drafted.themes.into_iter().enumerate() {
        let mut reject = |reason: DropReason, detail: Option<String>, draft: ThemeDraft| {
            dropped.push(Dropped {
                reason,
                description: draft.description,
                refs: draft.passage_refs,
                detail,
            });
        };
        if n >= settings.max_themes {
            reject(DropReason::OverMaxThemes, None, draft);
            continue;
        }
        let positions: Vec<Option<usize>> = draft
            .passage_refs
            .iter()
            .map(|r| position(r, input.passages.len()))
            .collect();
        if positions.iter().any(Option::is_none) {
            reject(DropReason::UnknownRef, None, draft);
            continue;
        }
        let mut cited: Vec<usize> = Vec::new();
        for p in positions.into_iter().flatten() {
            if !cited.contains(&p) {
                cited.push(p);
            }
        }
        if cited.len() < 2 {
            reject(DropReason::TooFewPassages, None, draft);
            continue;
        }
        if let Some(reason) = filter::one_sentence(&draft.description) {
            reject(reason, None, draft);
            continue;
        }
        if filter::has_quotation_mark(&draft.description) {
            reject(DropReason::QuotationMark, None, draft);
            continue;
        }
        if filter::has_passage_label(&draft.description) {
            reject(DropReason::PassageLabel, None, draft);
            continue;
        }
        if let Some(alias) = filter::alias_in(&draft.description, aliases) {
            reject(DropReason::TeacherAlias, Some(alias.to_owned()), draft);
            continue;
        }
        let texts: Vec<&str> = cited
            .iter()
            .map(|&i| input.passages[i].1.as_str())
            .collect();
        let ratio = filter::grounding_ratio(&draft.description, &texts);
        if ratio < settings.min_grounding_ratio {
            reject(DropReason::Ungrounded, Some(format!("{ratio:.2}")), draft);
            continue;
        }
        candidates.push(Candidate { draft, cited });
    }
    candidates
}

/// One verifier call: the theme and its cited passages only.
async fn verify(
    llm: &dyn LlmClient,
    candidate: &Candidate,
    input: &SynthesisInput,
    settings: &Settings,
    class: DataClass,
) -> Result<VerifierOutput, Failure> {
    let mut user = format!(
        "Sentence: {}\n\nCited passages:\n",
        candidate.draft.description.trim()
    );
    for &i in &candidate.cited {
        user.push_str(&format!("\n{}: {}\n", label(i), input.passages[i].1));
    }
    let request = LlmRequest {
        stage: LlmStage::Verify,
        system: VERIFY_PROMPT.to_owned(),
        user,
        max_tokens: 400,
    };
    match tokio::time::timeout(
        settings.timeout,
        complete_json::<VerifierOutput>(llm, &request, class),
    )
    .await
    {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(LlmError::CapReached { .. })) => Err(Failure::CapReached),
        Ok(Err(e)) => Err(Failure::Failed {
            stage: "verify",
            error: e.to_string(),
        }),
        Err(_) => Err(Failure::Failed {
            stage: "verify",
            error: format!("no reply within {} s", settings.timeout.as_secs_f32()),
        }),
    }
}

/// Writes every drop and the outcome to the log, for the integrity audit (Spec 05).
fn log(out: &Synthesis) {
    for d in &out.dropped {
        tracing::info!(
            target: "wt_synth",
            reason = d.reason.as_str(),
            refs = ?d.refs,
            detail = d.detail.as_deref().unwrap_or(""),
            description = %d.description,
            "synthesis theme dropped"
        );
    }
    let mut by_reason: BTreeMap<&str, usize> = BTreeMap::new();
    for d in &out.dropped {
        *by_reason.entry(d.reason.as_str()).or_default() += 1;
    }
    match &out.failure {
        Some(Failure::Failed { stage, error }) => tracing::warn!(
            target: "wt_synth",
            outcome = "synthesis_failed",
            stage,
            error = %error,
            "synthesis line omitted"
        ),
        Some(Failure::CapReached) => tracing::warn!(
            target: "wt_synth",
            outcome = "cap_reached",
            "synthesis line omitted"
        ),
        None => tracing::info!(
            target: "wt_synth",
            themes = out.themes.len(),
            dropped = out.dropped.len(),
            drops = ?by_reason,
            calls = out.calls,
            "synthesis done"
        ),
    }
}
