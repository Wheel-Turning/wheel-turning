//! The summary, its deterministic filters and the verifier (Spec 05, Summary and verifier).
//!
//! [`summarize`] asks the model for a summary in the manner of a Consensus research answer:
//! a headline, framing, where the talks agree and differ, points to keep in mind and a sum-up.
//! The model returns excerpt labels and sentence numbers as JSON, never quoted text (CLAUDE.md
//! rule 5); the code turns each citation into a quotation span sliced from the cited excerpt,
//! runs the filters in the spec's order, and sends each surviving sentence to the verifier in
//! parallel. A failed or slow call removes the summary and nothing else, so the caller always
//! renders the excerpts. Every drop is logged with its reason.

// Private items carry docs too, as the public ones do through `missing_docs`.
#![warn(clippy::missing_docs_in_private_items)]

pub mod excerpt;
pub mod filter;

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::future::join_all;
use schemars::JsonSchema;
use serde::Deserialize;
use wt_core::answer::{
    StoredCite, StoredSentence, StoredSummary, StoredUnused, SummaryPart, SummaryStatus,
    UnusedReason,
};
use wt_core::config::LlmStage;
use wt_core::excerpt::{Chooser, Window};
use wt_core::ids::{PassageId, TeacherId};
use wt_extract::{DataClass, LlmClient, LlmError, LlmRequest, complete_json};

/// The generation prompt.
pub const SUMMARIZE_PROMPT: &str = include_str!("../prompts/summarize.md");
/// The verifier prompt.
pub const VERIFY_PROMPT: &str = include_str!("../prompts/verify.md");

/// One excerpt as the summary sees it: the window it was cut from, its words in that window,
/// and whose talk it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryExcerpt {
    /// The selected passage the excerpt stands for.
    pub passage_id: PassageId,
    /// The teacher who gave the talk.
    pub teacher_id: TeacherId,
    /// The teacher's display name, as the header gives it.
    pub teacher: String,
    /// The talk's year, when dated.
    pub year: Option<i32>,
    /// The window the excerpt was cut from.
    pub window: Window,
    /// The excerpt's window-wide words, `start..end`.
    pub range: (usize, usize),
}

impl SummaryExcerpt {
    /// The excerpt's whole sentences as window-wide word ranges, or the excerpt as one sentence
    /// when the window holds none inside it.
    pub fn sentences(&self) -> Vec<(usize, usize)> {
        let (start, end) = self.range;
        let inside: Vec<(usize, usize)> = self
            .window
            .sentences()
            .into_iter()
            .filter(|&(s, e)| s >= start && e <= end)
            .collect();
        if inside.is_empty() && end > start {
            vec![(start, end)]
        } else {
            inside
        }
    }

    /// The excerpt's verbatim text.
    pub fn text(&self) -> String {
        self.window.text(self.range.0, self.range.1)
    }

    /// The excerpt's header in the model's message: its label, the teacher and the talk's year.
    fn header(&self, label: &str) -> String {
        match self.year {
            Some(y) => format!("{label}: {}, talk from {y}", self.teacher),
            None => format!("{label}: {}, undated talk", self.teacher),
        }
    }
}

/// What the generation call sees: the question and the answer's excerpts in score order. The
/// model knows them as `E1..En` by position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryInput {
    /// The question as asked.
    pub query: String,
    /// The excerpts, best first.
    pub excerpts: Vec<SummaryExcerpt>,
}

/// One citation as the model gives it: an excerpt label and a run of its sentences.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct Cite {
    /// The excerpt label, such as `E2`.
    pub excerpt: String,
    /// The first sentence of the run, numbered from 1.
    pub start: i64,
    /// The last sentence of the run.
    pub end: i64,
}

/// One drafted sentence.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct DraftSentence {
    /// The sentence, at most 40 words.
    pub text: String,
    /// The excerpts that support it.
    pub cites: Vec<Cite>,
}

/// The generation call's reply.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct SummaryDraft {
    /// One sentence that answers the question as the excerpts do.
    pub headline: Option<DraftSentence>,
    /// One or two sentences on what the excerpts address.
    pub framing: Vec<DraftSentence>,
    /// Where talks by different teachers say alike.
    pub agree: Vec<DraftSentence>,
    /// Where they put the weight differently.
    pub differ: Vec<DraftSentence>,
    /// Context a quoted line leaves out.
    pub keep_in_mind: Vec<DraftSentence>,
    /// One closing sentence.
    pub sum_up: Option<DraftSentence>,
    /// Each excerpt no sentence cites, with why (Spec 11, PB2.2).
    #[serde(default)]
    pub unused: Vec<DraftUnused>,
}

/// An excerpt the draft leaves out, as the model gives it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct DraftUnused {
    /// The excerpt label, such as `E4`.
    pub excerpt: String,
    /// `off_topic` or `duplicate`.
    pub reason: DraftReason,
}

/// Why the draft leaves an excerpt out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DraftReason {
    /// It does not speak to the question.
    OffTopic,
    /// It repeats what a cited excerpt says.
    Duplicate,
}

impl From<DraftReason> for UnusedReason {
    fn from(r: DraftReason) -> Self {
        match r {
            DraftReason::OffTopic => UnusedReason::OffTopic,
            DraftReason::Duplicate => UnusedReason::Duplicate,
        }
    }
}

/// The verifier's reply about one sentence.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct VerifierOutput {
    /// Every assertion is stated or directly entailed by a cited excerpt.
    pub supported: bool,
    /// When unsupported, the part the excerpts do not support.
    pub unsupported_part: Option<String>,
}

/// `[synthesis]` in `config/retrieval.toml`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Sentences kept in each of the agree and differ sections.
    pub max_themes: usize,
    /// The most words in one sentence.
    pub max_words: usize,
    /// The most words in one quotation.
    pub quote_max_words: usize,
    /// The least share of a sentence's content words found in its cited excerpts.
    pub min_grounding_ratio: f32,
    /// The limit on each generation and verifier call.
    pub timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_themes: 3,
            max_words: 40,
            quote_max_words: 60,
            min_grounding_ratio: 0.4,
            timeout: Duration::from_secs(20),
        }
    }
}

/// Why a sentence was dropped. The filters run in the order of the variants up to `Ungrounded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Beyond its section's limit.
    OverLimit,
    /// No citation at all.
    NoCitation,
    /// A citation names no offered excerpt or a run outside it.
    UnknownRef,
    /// A cited run is longer than `quote_max_words`.
    QuoteTooLong,
    /// A sentence of agreement or difference cites one teacher only.
    OneTeacher,
    /// More than one sentence, or none.
    NotOneSentence,
    /// More than `max_words` words.
    TooLong,
    /// A quotation mark.
    QuotationMark,
    /// An excerpt label such as E3, which would not match the page's numbering.
    PassageLabel,
    /// A teacher named who gave none of the cited excerpts.
    Misattributed,
    /// A year that no cited talk is from.
    WrongYear,
    /// States what a teacher believes rather than what a talk says.
    BeliefWord,
    /// Speaks to the reader ("you", "your"), as advice in the site's voice.
    AddressesReader,
    /// Too few content words found in the cited excerpts.
    Ungrounded,
    /// The verifier did not mark it supported.
    Unsupported,
}

impl DropReason {
    /// The reason as logged.
    pub fn as_str(self) -> &'static str {
        match self {
            DropReason::OverLimit => "over_limit",
            DropReason::NoCitation => "no_citation",
            DropReason::UnknownRef => "unknown_ref",
            DropReason::QuoteTooLong => "quote_too_long",
            DropReason::OneTeacher => "one_teacher",
            DropReason::NotOneSentence => "not_one_sentence",
            DropReason::TooLong => "too_long",
            DropReason::QuotationMark => "quotation_mark",
            DropReason::PassageLabel => "passage_label",
            DropReason::Misattributed => "misattributed",
            DropReason::WrongYear => "wrong_year",
            DropReason::BeliefWord => "belief_word",
            DropReason::AddressesReader => "addresses_reader",
            DropReason::Ungrounded => "ungrounded",
            DropReason::Unsupported => "unsupported",
        }
    }
}

impl DropReason {
    /// What the revision request tells the model about a sentence dropped for this reason.
    fn explain(self) -> &'static str {
        match self {
            DropReason::OverLimit => "the section already had as many sentences as it takes",
            DropReason::NoCitation => "it cited no excerpt",
            DropReason::UnknownRef => {
                "a citation named an excerpt or sentence number that does not exist"
            }
            DropReason::QuoteTooLong => {
                "its cited sentences ran over 60 words; cite one short sentence"
            }
            DropReason::OneTeacher => {
                "it is an agree or differ sentence but every excerpt it cites is by the same teacher; \
                 see the teacher list, and put it in another part or cite two teachers"
            }
            DropReason::NotOneSentence => "it was not exactly one sentence",
            DropReason::TooLong => "it was over 40 words",
            DropReason::QuotationMark => "it used quotation marks",
            DropReason::PassageLabel => "it wrote an excerpt label such as E2 in the sentence",
            DropReason::Misattributed => {
                "it named a teacher who gave none of the excerpts it cites"
            }
            DropReason::WrongYear => "it gave a year that none of the cited talks is from",
            DropReason::BeliefWord => {
                "it said what a teacher believes or thinks instead of what a talk says"
            }
            DropReason::AddressesReader => {
                "it spoke to the reader with you or your; report what a talk says instead"
            }
            DropReason::Ungrounded => {
                "its words are not the excerpts' words; use the teachers' own key words"
            }
            DropReason::Unsupported => {
                "the cited sentences do not say this; report only what they say, as a talk or teacher saying it"
            }
        }
    }
}

/// A dropped sentence, as the integrity audit reads it from the log.
#[derive(Debug, Clone, PartialEq)]
pub struct Dropped {
    /// Its section.
    pub part: SummaryPart,
    /// Why.
    pub reason: DropReason,
    /// The drafted sentence.
    pub text: String,
    /// The excerpt labels it cited, as drafted.
    pub refs: Vec<String>,
    /// The name or year found, the grounding ratio, or the verifier's unsupported part.
    pub detail: Option<String>,
}

/// Why the summary is missing altogether.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The month's language-model cap is reached (Spec 04, `cap_reached`).
    CapReached,
    /// No model may be called: none is configured, or real text is not allowed.
    NoModel {
        /// `synth` or `verify`.
        stage: &'static str,
    },
    /// A generation or verifier call ran past the timeout.
    TimedOut {
        /// `synth` or `verify`.
        stage: &'static str,
    },
    /// A generation or verifier call failed (`synthesis_failed`).
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
            Failure::NoModel { .. } | Failure::TimedOut { .. } | Failure::Failed { .. } => {
                "synthesis_failed"
            }
        }
    }

    /// The answer's summary status for this failure (Spec 11, PB2.4).
    pub fn status(&self) -> SummaryStatus {
        match self {
            Failure::CapReached => SummaryStatus::CapReached,
            Failure::NoModel { .. } => SummaryStatus::NoKey,
            Failure::TimedOut { .. } => SummaryStatus::Timeout,
            Failure::Failed { .. } => SummaryStatus::ModelError,
        }
    }

    /// A failed call's error as a failure of `stage`.
    fn of(stage: &'static str, e: LlmError) -> Failure {
        match e {
            LlmError::CapReached { .. } => Failure::CapReached,
            LlmError::NoModel(_) | LlmError::RealDataRefused => Failure::NoModel { stage },
            other => Failure::Failed {
                stage,
                error: other.to_string(),
            },
        }
    }
}

/// The result of one summary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Synthesis {
    /// The verified summary, with excerpt ids and quotation spans the code mapped from the
    /// model's labels and sentence numbers. `None` when the headline did not survive.
    pub summary: Option<StoredSummary>,
    /// Every sentence dropped, with its reason.
    pub dropped: Vec<Dropped>,
    /// Set when a call failed; `summary` is then `None`.
    pub failure: Option<Failure>,
    /// Model calls made.
    pub calls: usize,
}

impl Synthesis {
    /// Sentences shown.
    pub fn shown(&self) -> usize {
        self.summary.as_ref().map_or(0, |s| s.sentences.len())
    }

    /// How the summary settled (Spec 11, PB2.4). No call at all, as for an answer without
    /// excerpts, is `not_attempted`; calls that left no headline are `all_sentences_rejected`.
    pub fn status(&self) -> SummaryStatus {
        match (&self.summary, &self.failure) {
            (Some(_), _) => SummaryStatus::Shown,
            (None, Some(f)) => f.status(),
            (None, None) if self.calls == 0 => SummaryStatus::NotAttempted,
            (None, None) => SummaryStatus::AllSentencesRejected,
        }
    }
}

/// A sentence that passed the deterministic filters, waiting on the verifier.
struct Candidate {
    /// Its section.
    part: SummaryPart,
    /// The sentence.
    text: String,
    /// The excerpt labels it cites.
    refs: Vec<String>,
    /// Distinct citations: excerpt position and window-wide words of the quotation.
    cites: Vec<(usize, usize, usize)>,
}

impl Candidate {
    /// Distinct cited excerpt positions, in citing order.
    fn excerpts(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for &(i, _, _) in &self.cites {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }
}

/// Drafts, filters and verifies the summary for one standard-mode answer. Named mode must not
/// call this (Spec 05). `aliases` is every teacher alias and display name in the registry.
pub async fn summarize(
    llm: &dyn LlmClient,
    input: &SummaryInput,
    aliases: &[(TeacherId, String)],
    settings: &Settings,
    class: DataClass,
) -> Synthesis {
    let mut out = Synthesis::default();
    if input.excerpts.is_empty() {
        return out;
    }

    let names = filter::name_words(aliases);
    let mut draft = match round(llm, input, &names, settings, class, None, &mut out).await {
        Ok(d) => d,
        Err(done) => return done,
    };
    // A draft that loses its headline gets one revision that is told why each sentence went
    // (October 15, 2026: on the beta every summary was lost this way). The summary is written
    // after the page is shown, so the second call costs the reader no wait.
    if !draft.kept.iter().any(|c| c.part == SummaryPart::Headline) && !out.dropped.is_empty() {
        let first = out.dropped.clone();
        match round(llm, input, &names, settings, class, Some(&first), &mut out).await {
            Ok(d) => draft = d,
            Err(done) => return done,
        }
    }
    let unused = draft.unused(input.excerpts.len());
    let unlisted = input.excerpts.len() - draft.cited().len() - unused.len();
    if unlisted > 0 {
        // Spec 11, PB2.2: the draft neither cited these nor listed them as unused.
        tracing::info!(
            target: "wt_synth",
            unlisted,
            offered = input.excerpts.len(),
            "summary excerpts unaccounted for"
        );
    }
    let summary = StoredSummary {
        unused: unused
            .into_iter()
            .map(|(i, reason)| StoredUnused {
                passage_id: input.excerpts[i].passage_id,
                reason,
            })
            .collect(),
        sentences: draft
            .kept
            .into_iter()
            .map(|c| StoredSentence {
                part: c.part,
                text: c.text.trim().to_owned(),
                cites: c
                    .cites
                    .iter()
                    .map(|&(i, s, e)| {
                        let x = &input.excerpts[i];
                        StoredCite {
                            passage_id: x.passage_id,
                            quote: x.window.span(s, e, Chooser::Model),
                        }
                    })
                    .collect(),
            })
            .collect(),
    };
    out.summary = summary.has_headline().then_some(summary);
    log(&out);
    out
}

/// What one draft left after the filters and the verifier.
struct Round {
    /// The sentences the verifier kept.
    kept: Vec<Candidate>,
    /// The excerpts the draft listed as unused, by offered position.
    listed: Vec<(usize, UnusedReason)>,
    /// The offered positions any drafted sentence cited, kept or not.
    drafted: Vec<usize>,
}

impl Round {
    /// Distinct excerpt positions the kept sentences cite.
    fn cited(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for i in self.kept.iter().flat_map(Candidate::excerpts) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }

    /// Spec 11, PB2.2: each offered excerpt no kept sentence cites, with why, in offered order.
    /// The draft's own reason comes first. An excerpt the draft cited, whose every citing
    /// sentence or citation the checks dropped, is `dropped`. One the draft neither cited nor
    /// listed is left out, as the miss it is.
    fn unused(&self, offered: usize) -> Vec<(usize, UnusedReason)> {
        let cited = self.cited();
        (0..offered)
            .filter(|i| !cited.contains(i))
            .filter_map(|i| match self.listed.iter().find(|(j, _)| *j == i) {
                Some(&(_, reason)) => Some((i, reason)),
                None => self
                    .drafted
                    .contains(&i)
                    .then_some((i, UnusedReason::Dropped)),
            })
            .collect()
    }
}

/// One draft: generation, the filters and the verifier, or the finished synthesis when a call
/// failed. `revise` holds the drops a revision is told about.
async fn round(
    llm: &dyn LlmClient,
    input: &SummaryInput,
    names: &[(TeacherId, String)],
    settings: &Settings,
    class: DataClass,
    revise: Option<&[Dropped]>,
    out: &mut Synthesis,
) -> Result<Round, Synthesis> {
    out.calls += 1;
    let request = LlmRequest {
        stage: LlmStage::Synth,
        system: SUMMARIZE_PROMPT.to_owned(),
        user: match revise {
            Some(dropped) => revision_message(input, dropped),
            None => generation_message(input),
        },
        max_tokens: 2000,
    };
    let drafted = match tokio::time::timeout(
        settings.timeout,
        complete_json::<SummaryDraft>(llm, &request, class),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(fail(std::mem::take(out), "synth", e)),
        Err(_) => return Err(timed_out(std::mem::take(out), "synth")),
    };
    let listed = unused_positions(&drafted.unused, input.excerpts.len());
    let drafted_cites = drafted_positions(&drafted, input.excerpts.len());
    let candidates = filter_draft(drafted, input, names, settings, &mut out.dropped);
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
                part: candidate.part,
                reason: DropReason::Unsupported,
                text: candidate.text,
                refs: candidate.refs,
                detail: v.unsupported_part,
            }),
            Err(failure) => {
                let mut done = std::mem::take(out);
                done.failure = Some(failure);
                log(&done);
                return Err(done);
            }
        }
    }
    Ok(Round {
        kept,
        listed,
        drafted: drafted_cites,
    })
}

/// Records a failed model call on the synthesis and logs it; a spend cap that stopped the call is
/// reported as such, not as a failure.
fn fail(mut out: Synthesis, stage: &'static str, e: LlmError) -> Synthesis {
    out.failure = Some(Failure::of(stage, e));
    log(&out);
    out
}

/// Records a model call that did not answer in time, and logs it.
fn timed_out(mut out: Synthesis, stage: &'static str) -> Synthesis {
    out.failure = Some(Failure::TimedOut { stage });
    log(&out);
    out
}

/// The draft's unused list as offered positions, each once; labels naming no offered excerpt
/// are left out.
fn unused_positions(list: &[DraftUnused], offered: usize) -> Vec<(usize, UnusedReason)> {
    let mut out: Vec<(usize, UnusedReason)> = Vec::new();
    for u in list {
        if let Some(i) = position(&u.excerpt, offered)
            && !out.iter().any(|(j, _)| *j == i)
        {
            out.push((i, u.reason.into()));
        }
    }
    out
}

/// The offered positions the draft's sentences cite, each once, before any check.
fn drafted_positions(d: &SummaryDraft, offered: usize) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let sentences = d
        .headline
        .iter()
        .chain(&d.framing)
        .chain(&d.agree)
        .chain(&d.differ)
        .chain(&d.keep_in_mind)
        .chain(&d.sum_up);
    for c in sentences.flat_map(|s| &s.cites) {
        if let Some(i) = position(&c.excerpt, offered)
            && !out.contains(&i)
        {
            out.push(i);
        }
    }
    out
}

/// The label of the excerpt at `index`.
fn label(index: usize) -> String {
    format!("E{}", index + 1)
}

/// The offered position a label names: `E3`, `e3` or `3`.
fn position(r: &str, offered: usize) -> Option<usize> {
    let r = r.trim();
    let digits = r
        .strip_prefix('E')
        .or_else(|| r.strip_prefix('e'))
        .unwrap_or(r);
    let n: usize = digits.parse().ok()?;
    (1..=offered).contains(&n).then(|| n - 1)
}

/// The user message for the draft: the question, then each excerpt under its header with numbered
/// sentences and their word counts, which the draft cites by number.
fn generation_message(input: &SummaryInput) -> String {
    let mut text = format!("Question: {}\n\nExcerpts:\n", input.query);
    let mut teachers: Vec<(TeacherId, &str, Vec<String>)> = Vec::new();
    for (i, x) in input.excerpts.iter().enumerate() {
        match teachers.iter_mut().find(|(t, _, _)| *t == x.teacher_id) {
            Some((_, _, labels)) => labels.push(label(i)),
            None => teachers.push((x.teacher_id, &x.teacher, vec![label(i)])),
        }
    }
    for (i, x) in input.excerpts.iter().enumerate() {
        text.push_str(&format!("\n{}\n", x.header(&label(i))));
        for (n, (s, e)) in x.sentences().into_iter().enumerate() {
            text.push_str(&format!(
                "[{}] ({} words) {}\n",
                n + 1,
                e - s,
                x.window.text(s, e)
            ));
        }
    }
    text.push_str("\nTeachers (an agree or differ sentence cites excerpts of two of them):\n");
    for (_, name, labels) in &teachers {
        text.push_str(&format!("- {name}: {}\n", labels.join(", ")));
    }
    // Spec 11, PB2.2: drafts left excerpts neither cited nor listed, so the list is spelled out.
    let all: Vec<String> = (0..input.excerpts.len()).map(label).collect();
    text.push_str(&format!(
        "\nEach of {} appears in a sentence's cites or under unused.\n",
        all.join(", ")
    ));
    text
}

/// The revision request: the first draft's message, then each sentence that was dropped and
/// why, so the second draft can keep to the rules.
fn revision_message(input: &SummaryInput, dropped: &[Dropped]) -> String {
    let mut text = generation_message(input);
    text.push_str(
        "\nYour first draft lost these sentences, and without a headline there is no summary. \
         Write the whole summary again, keeping every rule:\n",
    );
    for d in dropped {
        text.push_str(&format!(
            "- {:?}: {} Dropped because {}.\n",
            d.part,
            d.text.trim(),
            d.reason.explain()
        ));
    }
    text
}

/// A drafted sentence with its section.
type Placed = (SummaryPart, DraftSentence);

/// The draft's sentences in page order, each with its section, and those over a section's
/// limit.
fn by_part(d: SummaryDraft, settings: &Settings) -> (Vec<Placed>, Vec<Placed>) {
    let mut kept = Vec::new();
    let mut over = Vec::new();
    let mut take = |part: SummaryPart, list: Vec<DraftSentence>, limit: usize| {
        for (n, s) in list.into_iter().enumerate() {
            if n < limit {
                kept.push((part, s));
            } else {
                over.push((part, s));
            }
        }
    };
    take(SummaryPart::Headline, d.headline.into_iter().collect(), 1);
    take(SummaryPart::Framing, d.framing, 2);
    take(SummaryPart::Agree, d.agree, settings.max_themes);
    take(SummaryPart::Differ, d.differ, settings.max_themes);
    take(SummaryPart::KeepInMind, d.keep_in_mind, 2);
    take(SummaryPart::SumUp, d.sum_up.into_iter().collect(), 1);
    (kept, over)
}

/// Filters 1 to 9, in order, on each sentence with its em dashes replaced. Survivors go to the
/// verifier.
fn filter_draft(
    drafted: SummaryDraft,
    input: &SummaryInput,
    names: &[(TeacherId, String)],
    settings: &Settings,
    dropped: &mut Vec<Dropped>,
) -> Vec<Candidate> {
    let (sentences, over) = by_part(drafted, settings);
    let refs_of = |d: &DraftSentence| -> Vec<String> {
        d.cites
            .iter()
            .map(|c| format!("{}[{}-{}]", c.excerpt, c.start, c.end))
            .collect()
    };
    for (part, d) in over {
        dropped.push(Dropped {
            part,
            reason: DropReason::OverLimit,
            refs: refs_of(&d),
            text: d.text,
            detail: None,
        });
    }
    let mut candidates = Vec::new();
    for (part, d) in sentences {
        // Spec 11, PB2.3: no em dash reaches a filter, the verifier or the page.
        let d = DraftSentence {
            text: filter::replace_dashes(&d.text),
            ..d
        };
        let refs = refs_of(&d);
        let mut reject = |reason: DropReason, detail: Option<String>, text: String| {
            dropped.push(Dropped {
                part,
                reason,
                text,
                refs: refs.clone(),
                detail,
            });
        };
        let cites = match citations(&d, input, settings) {
            Ok(c) => c,
            Err(reason) => {
                reject(reason, None, d.text);
                continue;
            }
        };
        let candidate = Candidate {
            part,
            text: d.text,
            refs: refs.clone(),
            cites,
        };
        let cited = candidate.excerpts();
        let teachers: Vec<TeacherId> = {
            let mut t: Vec<TeacherId> = Vec::new();
            for &i in &cited {
                if !t.contains(&input.excerpts[i].teacher_id) {
                    t.push(input.excerpts[i].teacher_id);
                }
            }
            t
        };
        let text = candidate.text.as_str();
        if matches!(part, SummaryPart::Agree | SummaryPart::Differ) && teachers.len() < 2 {
            reject(DropReason::OneTeacher, None, candidate.text);
            continue;
        }
        if let Some(reason) = filter::one_sentence(text, settings.max_words) {
            reject(reason, None, candidate.text);
            continue;
        }
        if filter::has_quotation_mark(text) {
            reject(DropReason::QuotationMark, None, candidate.text);
            continue;
        }
        if filter::has_passage_label(text) {
            reject(DropReason::PassageLabel, None, candidate.text);
            continue;
        }
        if let Some(name) = filter::misattributed(text, names, &teachers) {
            reject(DropReason::Misattributed, Some(name), candidate.text);
            continue;
        }
        let cited_years: Vec<i32> = cited
            .iter()
            .filter_map(|&i| input.excerpts[i].year)
            .collect();
        if let Some(y) = filter::years(text)
            .into_iter()
            .find(|y| !cited_years.contains(y))
        {
            reject(DropReason::WrongYear, Some(y.to_string()), candidate.text);
            continue;
        }
        if let Some(w) = filter::belief_word(text) {
            reject(DropReason::BeliefWord, Some(w.to_owned()), candidate.text);
            continue;
        }
        if let Some(w) = filter::addresses_reader(text) {
            reject(
                DropReason::AddressesReader,
                Some(w.to_owned()),
                candidate.text,
            );
            continue;
        }
        let texts: Vec<String> = cited.iter().map(|&i| input.excerpts[i].text()).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let name_list: Vec<String> = names.iter().map(|(_, n)| n.clone()).collect();
        let ratio = filter::grounding_ratio(text, &texts, &name_list);
        if ratio < settings.min_grounding_ratio {
            reject(
                DropReason::Ungrounded,
                Some(format!("{ratio:.2}")),
                candidate.text,
            );
            continue;
        }
        candidates.push(candidate);
    }
    candidates
}

/// Filter 2: each citation as an excerpt position and the quotation's window-wide words,
/// duplicates collapsed. A citation naming no offered excerpt or sentence drops the sentence;
/// one longer than `quote_max_words` is left out, and drops the sentence only when no other
/// citation remains.
fn citations(
    d: &DraftSentence,
    input: &SummaryInput,
    settings: &Settings,
) -> Result<Vec<(usize, usize, usize)>, DropReason> {
    if d.cites.is_empty() {
        return Err(DropReason::NoCitation);
    }
    let mut out: Vec<(usize, usize, usize)> = Vec::new();
    let mut too_long = false;
    for c in &d.cites {
        let i = position(&c.excerpt, input.excerpts.len()).ok_or(DropReason::UnknownRef)?;
        let sentences = input.excerpts[i].sentences();
        let first = usize::try_from(c.start).ok().filter(|&n| n >= 1);
        let last = usize::try_from(c.end).ok();
        let (Some(first), Some(last)) = (first, last) else {
            return Err(DropReason::UnknownRef);
        };
        if last < first || last > sentences.len() {
            return Err(DropReason::UnknownRef);
        }
        let (s, e) = (sentences[first - 1].0, sentences[last - 1].1);
        // An over-long run is left out on its own; the sentence keeps its other citations.
        if e - s > settings.quote_max_words {
            too_long = true;
            continue;
        }
        if !out.contains(&(i, s, e)) {
            out.push((i, s, e));
        }
    }
    if out.is_empty() && too_long {
        return Err(DropReason::QuoteTooLong);
    }
    Ok(out)
}

/// One verifier call: the sentence and its cited excerpts only, with their headers.
async fn verify(
    llm: &dyn LlmClient,
    candidate: &Candidate,
    input: &SummaryInput,
    settings: &Settings,
    class: DataClass,
) -> Result<VerifierOutput, Failure> {
    let mut user = format!("Sentence: {}\n\nCited excerpts:\n", candidate.text.trim());
    for i in candidate.excerpts() {
        let x = &input.excerpts[i];
        user.push_str(&format!("\n{}\n{}\n", x.header(&label(i)), x.text()));
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
        Ok(Err(e)) => Err(Failure::of("verify", e)),
        Err(_) => Err(Failure::TimedOut { stage: "verify" }),
    }
}

/// Writes every drop and the outcome to the log, for the integrity audit (Spec 05).
fn log(out: &Synthesis) {
    for d in &out.dropped {
        tracing::info!(
            target: "wt_synth",
            part = ?d.part,
            reason = d.reason.as_str(),
            refs = ?d.refs,
            detail = d.detail.as_deref().unwrap_or(""),
            text = %d.text,
            "summary sentence dropped"
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
            "summary omitted"
        ),
        Some(Failure::TimedOut { stage }) => tracing::warn!(
            target: "wt_synth",
            outcome = "synthesis_failed",
            stage,
            error = "timed out",
            "summary omitted"
        ),
        Some(Failure::NoModel { stage }) => tracing::warn!(
            target: "wt_synth",
            outcome = "synthesis_failed",
            stage,
            error = "no model may be called",
            "summary omitted"
        ),
        Some(Failure::CapReached) => tracing::warn!(
            target: "wt_synth",
            outcome = "cap_reached",
            "summary omitted"
        ),
        None => tracing::info!(
            target: "wt_synth",
            sentences = out.shown(),
            headline = out.summary.is_some(),
            dropped = out.dropped.len(),
            drops = ?by_reason,
            calls = out.calls,
            "summary done"
        ),
    }
}
