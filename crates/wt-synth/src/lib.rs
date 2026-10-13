//! The summary, its deterministic filters and the verifier (Spec 05, Summary and verifier).
//!
//! [`summarize`] asks the model for a summary in the manner of a Consensus research answer:
//! a headline, framing, where the talks agree and differ, points to keep in mind and a sum-up.
//! The model returns excerpt labels and sentence numbers as JSON, never quoted text (CLAUDE.md
//! rule 5); the code turns each citation into a quotation span sliced from the cited excerpt,
//! runs the filters in the spec's order, and sends each surviving sentence to the verifier in
//! parallel. A failed or slow call removes the summary and nothing else, so the caller always
//! renders the excerpts. Every drop is logged with its reason.

pub mod excerpt;
pub mod filter;

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::future::join_all;
use schemars::JsonSchema;
use serde::Deserialize;
use wt_core::answer::{StoredCite, StoredSentence, StoredSummary, SummaryPart};
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
}

/// A sentence that passed the deterministic filters, waiting on the verifier.
struct Candidate {
    part: SummaryPart,
    text: String,
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
    // The two halves are independent: each drafts its parts and verifies them as soon as its
    // draft returns, so the stage takes as long as the slower half (Spec 05, latency).
    let (frame, body) = tokio::join!(
        half(llm, input, &names, settings, class, Half::Frame),
        half(llm, input, &names, settings, class, Half::Body),
    );
    let mut kept = Vec::new();
    for h in [frame, body] {
        out.calls += h.calls;
        out.dropped.extend(h.dropped);
        if out.failure.is_none() {
            out.failure = h.failure;
        }
        kept.extend(h.kept);
    }
    if out.failure.is_some() {
        log(&out);
        return out;
    }
    kept.sort_by_key(|c| c.part);
    let summary = StoredSummary {
        sentences: kept
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

/// Which parts one generation call writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    /// The headline, the framing and the sum-up.
    Frame,
    /// Where the talks agree and differ, and points to keep in mind.
    Body,
}

impl Half {
    fn instruction(self) -> &'static str {
        match self {
            Half::Frame => {
                "In this reply write only `headline`, `framing` and `sum_up`. Leave `agree`, \
                 `differ` and `keep_in_mind` empty; another call writes them."
            }
            Half::Body => {
                "In this reply write only `agree`, `differ` and `keep_in_mind`. Leave `framing` \
                 empty and `headline` and `sum_up` null; another call writes them."
            }
        }
    }

    fn owns(self, part: SummaryPart) -> bool {
        let frame = matches!(
            part,
            SummaryPart::Headline | SummaryPart::Framing | SummaryPart::SumUp
        );
        frame == (self == Half::Frame)
    }
}

/// What one half produced.
#[derive(Default)]
struct HalfOut {
    kept: Vec<Candidate>,
    dropped: Vec<Dropped>,
    failure: Option<Failure>,
    calls: usize,
}

/// One half: its generation call, the filters on the parts it owns, and their verifier calls
/// in parallel. Parts it does not own are ignored.
async fn half(
    llm: &dyn LlmClient,
    input: &SummaryInput,
    names: &[(TeacherId, String)],
    settings: &Settings,
    class: DataClass,
    which: Half,
) -> HalfOut {
    let mut out = HalfOut {
        calls: 1,
        ..HalfOut::default()
    };
    let request = LlmRequest {
        stage: LlmStage::Synth,
        system: SUMMARIZE_PROMPT.to_owned(),
        user: format!("{}\n{}", generation_message(input), which.instruction()),
        max_tokens: 1500,
    };
    let mut drafted = match tokio::time::timeout(
        settings.timeout,
        complete_json::<SummaryDraft>(llm, &request, class),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            out.failure = Some(failure_of("synth", e));
            return out;
        }
        Err(_) => {
            out.failure = Some(timeout_failure("synth", settings.timeout));
            return out;
        }
    };
    if which == Half::Frame {
        drafted.agree.clear();
        drafted.differ.clear();
        drafted.keep_in_mind.clear();
    } else {
        drafted.headline = None;
        drafted.framing.clear();
        drafted.sum_up = None;
    }
    let candidates = filter_draft(drafted, input, names, settings, &mut out.dropped);
    debug_assert!(candidates.iter().all(|c| which.owns(c.part)));

    out.calls += candidates.len();
    let verdicts = join_all(
        candidates
            .iter()
            .map(|c| verify(llm, c, input, settings, class)),
    )
    .await;
    for (candidate, verdict) in candidates.into_iter().zip(verdicts) {
        match verdict {
            Ok(v) if v.supported => out.kept.push(candidate),
            Ok(v) => out.dropped.push(Dropped {
                part: candidate.part,
                reason: DropReason::Unsupported,
                text: candidate.text,
                refs: candidate.refs,
                detail: v.unsupported_part,
            }),
            Err(failure) => {
                out.failure = Some(failure);
                out.kept.clear();
                return out;
            }
        }
    }
    out
}

fn failure_of(stage: &'static str, e: LlmError) -> Failure {
    match e {
        LlmError::CapReached { .. } => Failure::CapReached,
        other => Failure::Failed {
            stage,
            error: other.to_string(),
        },
    }
}

fn timeout_failure(stage: &'static str, after: Duration) -> Failure {
    Failure::Failed {
        stage,
        error: format!("no reply within {} s", after.as_secs_f32()),
    }
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

fn generation_message(input: &SummaryInput) -> String {
    let mut text = format!("Question: {}\n\nExcerpts:\n", input.query);
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

/// Filters 1 to 9, in order. Survivors go to the verifier.
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
