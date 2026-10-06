//! The segmenter (Spec 04, Diarization and segmentation): words and speaker turns in, labeled
//! spans and teacher-only segments out. Student speech never leaves this module: only the
//! teacher's words are returned, and only `talk` and `qa_teacher` become segments.
//!
//! It runs in two phases so it stays a pure function. [`plan`] aligns words to speakers, finds
//! sitting, labels each window by the rules, picks the teacher and returns the windows the
//! rules leave ambiguous. The caller has those labeled (by the language model in production)
//! and [`Plan::finish`] builds the result. Given the same inputs and labels the output is the
//! same.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wt_core::transcript::Word;

/// One diarized speaker turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// The diarizer's label, such as `S1`.
    pub speaker: String,
    /// Start, milliseconds.
    pub start_ms: i32,
    /// End, milliseconds.
    pub end_ms: i32,
}

/// `[segment]` in `config/pipeline.toml`. The window thresholds are starting points to tune on
/// the ten-video sample (decision of October 5, 2026).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SegmentSettings {
    /// A second speaker holding at least this share of speaking time sends the talk to review.
    pub multi_speaker_share: f64,
    /// A stretch without words at least this long is `sitting`.
    pub sitting_silence_s: i32,
    /// Window length for classification.
    pub window_s: i32,
    /// No span is shorter than this unless it touches the start or end of the audio.
    pub min_span_s: i32,
    /// A word overlapping no turn takes the nearest turn within this distance.
    pub align_nearest_ms: i32,
    /// Chant: fewer words per minute than this...
    pub chant_max_wpm: f64,
    /// ...with a type-token ratio below this...
    pub chant_max_ttr: f64,
    /// ...or at least this share of words inside chant phrases.
    pub chant_phrase_min_share: f64,
    /// Talk: at least this many words per minute...
    pub talk_min_wpm: f64,
    /// ...a type-token ratio of at least this...
    pub talk_min_ttr: f64,
    /// ...and under this share of words inside chant phrases.
    pub talk_max_chant_share: f64,
}

impl Default for SegmentSettings {
    fn default() -> Self {
        Self {
            multi_speaker_share: 0.35,
            sitting_silence_s: 120,
            window_s: 60,
            min_span_s: 20,
            align_nearest_ms: 500,
            chant_max_wpm: 60.0,
            chant_max_ttr: 0.35,
            chant_phrase_min_share: 0.30,
            talk_min_wpm: 90.0,
            talk_min_ttr: 0.35,
            talk_max_chant_share: 0.10,
        }
    }
}

/// A window's label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Label {
    /// Dharma talk.
    Talk,
    /// Chanting.
    Chant,
    /// Announcements.
    Announcement,
    /// Anything else.
    Other,
}

/// What a stretch of the recording is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    /// The teacher's talk.
    Talk,
    /// The teacher answering a student.
    QaTeacher,
    /// Chanting.
    Chant,
    /// Announcements.
    Announcement,
    /// Silent sitting.
    Sitting,
    /// Anything else.
    Other,
}

impl SpanKind {
    /// The `segment_kind` value.
    pub fn as_str(self) -> &'static str {
        match self {
            SpanKind::Talk => "talk",
            SpanKind::QaTeacher => "qa_teacher",
            SpanKind::Chant => "chant",
            SpanKind::Announcement => "announcement",
            SpanKind::Sitting => "sitting",
            SpanKind::Other => "other",
        }
    }
}

/// Why a talk cannot be segmented automatically.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Review {
    /// A second speaker holds too much speaking time to tell who the teacher is. The talk goes
    /// to `needs_review` with reason `multi_speaker`.
    #[error("a second speaker holds {share:.2} of speaking time")]
    MultiSpeaker {
        /// The second speaker's share.
        share: f64,
    },
    /// No speech was aligned to any speaker. Reason `no_speech`.
    #[error("no speech to segment")]
    NoSpeech,
}

impl Review {
    /// The talk's `state_reason`.
    pub fn reason(&self) -> &'static str {
        match self {
            Review::MultiSpeaker { .. } => "multi_speaker",
            Review::NoSpeech => "no_speech",
        }
    }
}

/// [`Plan::finish`] was not given a label for every ambiguous window.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no label for ambiguous window {0}")]
pub struct MissingLabel(pub usize);

/// A window the rules could not label, to be labeled by the caller. Its text holds only the
/// teacher's words, so student speech is never sent anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Its index, the key for its label.
    pub index: usize,
    /// Start, milliseconds.
    pub start_ms: i32,
    /// End, milliseconds.
    pub end_ms: i32,
    /// The teacher's words in it.
    pub text: String,
}

/// A labeled stretch, kept in the transcript JSON's `spans` for the audit. Student turns are
/// never among them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// What it is.
    pub kind: SpanKind,
    /// Start, milliseconds.
    pub start_ms: i32,
    /// End, milliseconds.
    pub end_ms: i32,
    /// The first and one past the last teacher word in it, if it has any.
    pub words: Option<(usize, usize)>,
}

/// A `talk` or `qa_teacher` stretch that becomes a `segment` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentDraft {
    /// `Talk` or `QaTeacher`.
    pub kind: SpanKind,
    /// Start of its first word.
    pub start_ms: i32,
    /// End of its last word.
    pub end_ms: i32,
    /// Its words: the first and one past the last index into [`Segmentation::words`].
    pub words: (usize, usize),
    /// The words joined by spaces.
    pub text: String,
}

/// The segmenter's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Segmentation {
    /// The teacher's diarizer label.
    pub teacher: String,
    /// Only the teacher's words, in order. Nothing else is ever persisted.
    pub words: Vec<Word>,
    /// Labeled stretches for the audit.
    pub spans: Vec<Span>,
    /// What becomes `segment` rows.
    pub segments: Vec<SegmentDraft>,
    /// Student turns as time ranges, held in memory for the segmentation sample report.
    pub student_turns: Vec<(i32, i32)>,
    /// Words dropped because no turn was near them.
    pub unaligned_words: usize,
}

/// A window and its rule label, if the rules decided.
#[derive(Debug, Clone)]
struct RuleWindow {
    start_ms: i32,
    end_ms: i32,
    label: Option<Label>,
}

/// The state between the two phases.
#[derive(Debug, Clone)]
pub struct Plan {
    cfg: SegmentSettings,
    audio_ms: i32,
    /// Each aligned word with its speaker.
    aligned: Vec<(Word, String)>,
    unaligned: usize,
    teacher: String,
    sitting: Vec<(i32, i32)>,
    windows: Vec<RuleWindow>,
    ambiguous: Vec<Window>,
}

/// Lowercase letters and digits only, for counting and phrase matching.
fn norm(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn overlap(a: (i32, i32), b: (i32, i32)) -> i32 {
    (a.1.min(b.1) - a.0.max(b.0)).max(0)
}

fn distance(a: (i32, i32), b: (i32, i32)) -> i32 {
    if overlap(a, b) > 0 {
        0
    } else {
        (b.0 - a.1).max(a.0 - b.1).max(0)
    }
}

/// Step 1: each word takes the speaker of the turn it overlaps most, or the nearest turn within
/// `align_nearest_ms`; otherwise it is dropped.
fn align(words: &[Word], turns: &[Turn], near_ms: i32) -> (Vec<(Word, String)>, usize) {
    let mut aligned = Vec::new();
    let mut dropped = 0;
    for w in words {
        let span = (w.start_ms, w.end_ms.max(w.start_ms + 1));
        let best = turns
            .iter()
            .map(|t| (overlap(span, (t.start_ms, t.end_ms)), t))
            .filter(|(o, _)| *o > 0)
            .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.speaker.cmp(&a.1.speaker)))
            .map(|(_, t)| t)
            .or_else(|| {
                turns
                    .iter()
                    .map(|t| (distance(span, (t.start_ms, t.end_ms)), t))
                    .filter(|(d, _)| *d <= near_ms)
                    .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.speaker.cmp(&b.1.speaker)))
                    .map(|(_, t)| t)
            });
        match best {
            Some(t) => aligned.push((w.clone(), t.speaker.clone())),
            None => dropped += 1,
        }
    }
    (aligned, dropped)
}

/// Step 3: stretches without any word of at least `sitting_silence_s`.
fn sitting(words: &[Word], audio_ms: i32, min_ms: i32) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    let mut last_end = 0;
    for w in words {
        if w.start_ms - last_end >= min_ms {
            out.push((last_end, w.start_ms));
        }
        last_end = last_end.max(w.end_ms);
    }
    if audio_ms - last_end >= min_ms {
        out.push((last_end, audio_ms));
    }
    out
}

/// Shares of the window's words that fall inside one of the chant phrases.
fn chant_share(tokens: &[String], phrases: &[Vec<String>]) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    let mut covered = vec![false; tokens.len()];
    for phrase in phrases.iter().filter(|p| !p.is_empty()) {
        if phrase.len() > tokens.len() {
            continue;
        }
        for start in 0..=tokens.len() - phrase.len() {
            if tokens[start..start + phrase.len()] == phrase[..] {
                covered[start..start + phrase.len()].fill(true);
            }
        }
    }
    covered.iter().filter(|c| **c).count() as f64 / tokens.len() as f64
}

fn rule_label(
    tokens: &[String],
    minutes: f64,
    phrases: &[Vec<String>],
    cfg: &SegmentSettings,
) -> Option<Label> {
    if tokens.is_empty() {
        return Some(Label::Other);
    }
    let wpm = tokens.len() as f64 / minutes.max(1.0 / 60.0);
    let distinct: BTreeSet<&String> = tokens.iter().collect();
    let ttr = distinct.len() as f64 / tokens.len() as f64;
    let chant = chant_share(tokens, phrases);
    if (wpm < cfg.chant_max_wpm && ttr < cfg.chant_max_ttr) || chant >= cfg.chant_phrase_min_share {
        Some(Label::Chant)
    } else if wpm >= cfg.talk_min_wpm && ttr >= cfg.talk_min_ttr && chant < cfg.talk_max_chant_share
    {
        Some(Label::Talk)
    } else {
        None
    }
}

/// Phase one. `audio_ms` is the recording's length and `chant_phrases` the glossary's chant
/// phrases. Returns why the talk needs a person instead, when it does.
pub fn plan(
    words: &[Word],
    turns: &[Turn],
    audio_ms: i32,
    chant_phrases: &[String],
    cfg: &SegmentSettings,
) -> Result<Plan, Review> {
    let mut words: Vec<Word> = words.to_vec();
    words.sort_by_key(|w| (w.start_ms, w.end_ms));
    let audio_ms = audio_ms.max(words.last().map_or(0, |w| w.end_ms));
    let (aligned, unaligned) = align(&words, turns, cfg.align_nearest_ms);
    if aligned.is_empty() {
        return Err(Review::NoSpeech);
    }
    let sitting = sitting(&words, audio_ms, cfg.sitting_silence_s.saturating_mul(1000));
    let phrases: Vec<Vec<String>> = chant_phrases
        .iter()
        .map(|p| {
            p.split_whitespace()
                .map(norm)
                .filter(|t| !t.is_empty())
                .collect()
        })
        .collect();

    // Step 4, rules: windows of `window_s` over the time outside sitting.
    let mut windows = Vec::new();
    let mut cursor = 0;
    let window_ms = cfg.window_s.max(1).saturating_mul(1000);
    let mut gaps = sitting.clone();
    gaps.push((audio_ms, audio_ms));
    for (gap_start, gap_end) in gaps {
        let mut start = cursor;
        while start < gap_start {
            let end = (start + window_ms).min(gap_start);
            let tokens: Vec<String> = aligned
                .iter()
                .filter(|(w, _)| (start..end).contains(&w.mid_ms()))
                .map(|(w, _)| norm(&w.text))
                .filter(|t| !t.is_empty())
                .collect();
            let minutes = f64::from(end - start) / 60_000.0;
            windows.push(RuleWindow {
                start_ms: start,
                end_ms: end,
                label: rule_label(&tokens, minutes, &phrases, cfg),
            });
            start = end;
        }
        cursor = gap_end;
    }

    // Step 2: the teacher holds the most speaking time outside windows the rules call chant.
    let chant: Vec<(i32, i32)> = windows
        .iter()
        .filter(|w| w.label == Some(Label::Chant))
        .map(|w| (w.start_ms, w.end_ms))
        .collect();
    let mut time: BTreeMap<&str, i64> = BTreeMap::new();
    for t in turns {
        let length = i64::from((t.end_ms - t.start_ms).max(0));
        let chanted: i64 = chant
            .iter()
            .map(|c| i64::from(overlap((t.start_ms, t.end_ms), *c)))
            .sum();
        *time.entry(t.speaker.as_str()).or_default() += (length - chanted).max(0);
    }
    let mut ranked: Vec<(&str, i64)> = time.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let total: i64 = ranked.iter().map(|(_, t)| t).sum();
    let Some((teacher, _)) = ranked.first() else {
        return Err(Review::NoSpeech);
    };
    if let Some((_, second)) = ranked.get(1)
        && total > 0
    {
        let share = *second as f64 / total as f64;
        if share >= cfg.multi_speaker_share {
            return Err(Review::MultiSpeaker { share });
        }
    }
    let teacher = (*teacher).to_owned();

    let ambiguous = windows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.label.is_none())
        .map(|(index, w)| Window {
            index,
            start_ms: w.start_ms,
            end_ms: w.end_ms,
            text: aligned
                .iter()
                .filter(|(word, s)| {
                    *s == teacher && (w.start_ms..w.end_ms).contains(&word.mid_ms())
                })
                .map(|(word, _)| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect();

    Ok(Plan {
        cfg: cfg.clone(),
        audio_ms,
        aligned,
        unaligned,
        teacher,
        sitting,
        windows,
        ambiguous,
    })
}

/// A run of windows with one label, before smoothing.
#[derive(Debug, Clone)]
struct Run {
    label: Label,
    start_ms: i32,
    end_ms: i32,
}

impl Plan {
    /// The windows the rules could not label.
    pub fn ambiguous(&self) -> &[Window] {
        &self.ambiguous
    }

    /// The teacher's diarizer label.
    pub fn teacher(&self) -> &str {
        &self.teacher
    }

    /// Phase two: with a label for every ambiguous window, smooths the labels, separates
    /// student turns and builds the spans and segments.
    pub fn finish(self, labels: &BTreeMap<usize, Label>) -> Result<Segmentation, MissingLabel> {
        let mut labeled = Vec::with_capacity(self.windows.len());
        for (i, w) in self.windows.iter().enumerate() {
            let label = match w.label {
                Some(l) => l,
                None => *labels.get(&i).ok_or(MissingLabel(i))?,
            };
            labeled.push((w.start_ms, w.end_ms, label));
        }
        let runs = self.smooth(&labeled);
        Ok(self.build(&runs))
    }

    /// Merges windows into runs and relabels any run shorter than `min_span_s` that does not
    /// touch the start or end of the audio, taking its neighbor's label.
    fn smooth(&self, labeled: &[(i32, i32, Label)]) -> Vec<Run> {
        let mut runs: Vec<Run> = Vec::new();
        for &(start, end, label) in labeled {
            match runs.last_mut() {
                Some(r) if r.label == label && r.end_ms == start => r.end_ms = end,
                _ => runs.push(Run {
                    label,
                    start_ms: start,
                    end_ms: end,
                }),
            }
        }
        let min_ms = self.cfg.min_span_s.saturating_mul(1000);
        let adjacent = |runs: &[Run], a: usize, b: usize| runs[a].end_ms == runs[b].start_ms;
        loop {
            // A short run inside the audio with a neighbor right next to it.
            let short = (0..runs.len()).find(|&i| {
                let r = &runs[i];
                r.end_ms - r.start_ms < min_ms
                    && r.start_ms > 0
                    && r.end_ms < self.audio_ms
                    && ((i > 0 && adjacent(&runs, i - 1, i))
                        || (i + 1 < runs.len() && adjacent(&runs, i, i + 1)))
            });
            let Some(i) = short else { break };
            let before = (i > 0 && adjacent(&runs, i - 1, i)).then(|| i - 1);
            let after = (i + 1 < runs.len() && adjacent(&runs, i, i + 1)).then_some(i + 1);
            let len = |r: &Run| r.end_ms - r.start_ms;
            let label = match (before, after) {
                (Some(b), Some(a)) if len(&runs[a]) > len(&runs[b]) => runs[a].label,
                (Some(b), _) => runs[b].label,
                (None, Some(a)) => runs[a].label,
                (None, None) => break,
            };
            runs[i].label = label;
            let mut merged: Vec<Run> = Vec::new();
            for r in runs.drain(..) {
                match merged.last_mut() {
                    Some(m) if m.label == r.label && m.end_ms == r.start_ms => m.end_ms = r.end_ms,
                    _ => merged.push(r),
                }
            }
            runs = merged;
        }
        runs
    }

    fn build(self, runs: &[Run]) -> Segmentation {
        let run_of = |ms: i32| {
            runs.iter()
                .position(|r| (r.start_ms..r.end_ms).contains(&ms))
        };
        let first_talk = runs
            .iter()
            .find(|r| r.label == Label::Talk)
            .map(|r| r.start_ms);

        // Step 5: walk the words in order. Non-teacher speech after the first talk, in a talk
        // or other stretch, is a student turn; the teacher's words that follow it answer it.
        let mut words: Vec<Word> = Vec::new();
        let mut kinds: Vec<(SpanKind, Option<usize>)> = Vec::new();
        let mut student_turns: Vec<(i32, i32)> = Vec::new();
        // The run in which the teacher is answering a student, if any.
        let mut answering: Option<Option<usize>> = None;
        let mut last_was_student = false;
        for (word, speaker) in &self.aligned {
            let run = run_of(word.mid_ms());
            let label = run.map_or(Label::Other, |r| runs[r].label);
            let in_qa_scope = matches!(label, Label::Talk | Label::Other);
            if *speaker != self.teacher {
                if in_qa_scope && first_talk.is_some_and(|t| word.mid_ms() >= t) {
                    match student_turns.last_mut() {
                        Some(last) if last_was_student => last.1 = last.1.max(word.end_ms),
                        _ => student_turns.push((word.start_ms, word.end_ms)),
                    }
                    last_was_student = true;
                    answering = Some(run);
                } else {
                    last_was_student = false;
                }
                continue;
            }
            last_was_student = false;
            // An answer lasts until the next student turn or the end of its stretch.
            if answering.is_some_and(|r| r != run) || !in_qa_scope {
                answering = None;
            }
            let kind = match label {
                Label::Talk | Label::Other if answering.is_some() => SpanKind::QaTeacher,
                Label::Talk => SpanKind::Talk,
                Label::Chant => SpanKind::Chant,
                Label::Announcement => SpanKind::Announcement,
                Label::Other => SpanKind::Other,
            };
            words.push(word.clone());
            kinds.push((kind, run));
        }

        // Runs of teacher words with one kind inside one labeled stretch.
        let mut spans: Vec<Span> = Vec::new();
        let mut segments: Vec<SegmentDraft> = Vec::new();
        let mut i = 0;
        while i < words.len() {
            let (kind, run) = kinds[i];
            let mut j = i + 1;
            while j < words.len() && kinds[j] == (kind, run) {
                j += 1;
            }
            let (start_ms, end_ms) = (
                words[i].start_ms,
                words[j - 1].end_ms.max(words[i].start_ms + 1),
            );
            spans.push(Span {
                kind,
                start_ms,
                end_ms,
                words: Some((i, j)),
            });
            if matches!(kind, SpanKind::Talk | SpanKind::QaTeacher) {
                segments.push(SegmentDraft {
                    kind,
                    start_ms,
                    end_ms,
                    words: (i, j),
                    text: words[i..j]
                        .iter()
                        .map(|w| w.text.trim())
                        .filter(|t| !t.is_empty())
                        .collect::<Vec<_>>()
                        .join(" "),
                });
            }
            i = j;
        }
        // Stretches with no teacher words, such as sitting or a chant others lead.
        for &(start_ms, end_ms) in &self.sitting {
            spans.push(Span {
                kind: SpanKind::Sitting,
                start_ms,
                end_ms,
                words: None,
            });
        }
        for (r, run) in runs.iter().enumerate() {
            if !kinds.iter().any(|(_, k)| *k == Some(r)) {
                spans.push(Span {
                    kind: match run.label {
                        Label::Talk => SpanKind::Talk,
                        Label::Chant => SpanKind::Chant,
                        Label::Announcement => SpanKind::Announcement,
                        Label::Other => SpanKind::Other,
                    },
                    start_ms: run.start_ms,
                    end_ms: run.end_ms,
                    words: None,
                });
            }
        }
        spans.sort_by_key(|s| (s.start_ms, s.end_ms));

        Segmentation {
            teacher: self.teacher,
            words,
            spans,
            segments,
            student_turns,
            unaligned_words: self.unaligned,
        }
    }
}
