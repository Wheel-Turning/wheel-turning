//! Answer excerpts (Spec 05, Answer assembly, rule 8). A passage is the unit retrieval finds; the
//! page shows an excerpt of whole sentences cut for the question, which may reach into the
//! passages just before and after it in the same segment. This module holds the pure parts:
//! the window's sentences, the checks every chosen run must pass, the last-resort trimmed
//! passage, the span stored in `answer.payload`, and rebuilding its text verbatim.
//!
//! Word offsets count the whitespace-separated words of `passage.text`, which is how the index
//! step counts `passage.sentence_starts` (Spec 04, Chunking, embedding and indexing). Text is
//! always sliced from the stored passage text, never rebuilt from words, so it stays verbatim.

use serde::{Deserialize, Serialize};

use crate::ids::PassageId;

/// No excerpt is longer than this many words.
pub const MAX_EXCERPT_WORDS: usize = 250;

/// Which step chose an excerpt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chooser {
    /// The language model picked the sentences.
    Model,
    /// The reranker scored candidate spans.
    Reranker,
    /// The passage itself, trimmed to whole sentences where it could be.
    Passage,
}

impl Chooser {
    /// The value as stored and logged.
    pub fn as_str(self) -> &'static str {
        match self {
            Chooser::Model => "model",
            Chooser::Reranker => "reranker",
            Chooser::Passage => "passage",
        }
    }
}

/// What `answer.payload` keeps of an excerpt: never its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcerptSpan {
    /// The selected passage the excerpt stands for.
    pub passage_id: PassageId,
    /// The passages it covers, in talk order; the selected one is among them.
    pub covers: Vec<PassageId>,
    /// The word offset where it starts in the first covered passage.
    pub start: usize,
    /// The word offset, one past its last word, in the last covered passage.
    pub end: usize,
    /// Which step chose it.
    pub chooser: Chooser,
}

/// One passage of a window, as `servable_passage` serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowPassage {
    /// The passage.
    pub id: PassageId,
    /// Its verbatim text.
    pub text: String,
    /// Its place in the talk.
    pub ordinal: i32,
    /// When its first word starts.
    pub start_ms: i32,
    /// Word offsets where its sentences start, from the index step. Empty for a passage not
    /// yet indexed with boundaries.
    pub sentence_starts: Vec<i32>,
    /// The start time of each of those sentences.
    pub sentence_start_ms: Vec<i32>,
}

impl WindowPassage {
    /// Byte ranges of its words in `text`.
    pub fn word_ranges(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut from = None;
        for (i, c) in self.text.char_indices() {
            if c.is_whitespace() {
                if let Some(s) = from.take() {
                    out.push((s, i));
                }
            } else if from.is_none() {
                from = Some(i);
            }
        }
        if let Some(s) = from {
            out.push((s, self.text.len()));
        }
        out
    }

    /// Its number of words.
    pub fn word_count(&self) -> usize {
        self.text.split_whitespace().count()
    }

    /// Word offsets where its sentences start, sorted. A passage indexed before boundaries were
    /// stored falls back to punctuation: a word after one that ends a sentence, and the first
    /// word when it is capitalized.
    pub fn starts(&self) -> Vec<usize> {
        let n = self.word_count();
        if !self.sentence_starts.is_empty() {
            let mut v: Vec<usize> = self
                .sentence_starts
                .iter()
                .filter_map(|&s| usize::try_from(s).ok())
                .filter(|&s| s < n)
                .collect();
            v.sort_unstable();
            v.dedup();
            return v;
        }
        let words: Vec<&str> = self.text.split_whitespace().collect();
        (0..n)
            .filter(|&i| {
                if i == 0 {
                    words[0]
                        .chars()
                        .find(|c| c.is_alphanumeric())
                        .is_some_and(char::is_uppercase)
                } else {
                    ends_sentence(words[i - 1])
                }
            })
            .collect()
    }

    /// When the word at `offset` is spoken, as near as the stored sentence starts tell: the
    /// start of the sentence holding it, or of the passage.
    pub fn ms_at(&self, offset: usize) -> i32 {
        self.sentence_starts
            .iter()
            .zip(&self.sentence_start_ms)
            .filter(|(s, _)| usize::try_from(**s).is_ok_and(|s| s <= offset))
            .map(|(_, ms)| *ms)
            .max()
            .unwrap_or(self.start_ms)
            .max(self.start_ms)
    }
}

/// Whether a word ends a sentence: it ends in `.`, `?`, `!` or an ellipsis, before any closing
/// quotation marks or brackets.
pub fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', '\u{201d}', '\u{2019}', ')', ']'])
        .ends_with(['.', '?', '!', '\u{2026}'])
}

/// The bounds every excerpt must keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// `excerpt_min_words`.
    pub min_words: usize,
    /// The excerpt limit, [`MAX_EXCERPT_WORDS`].
    pub max_words: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            min_words: 25,
            max_words: MAX_EXCERPT_WORDS,
        }
    }
}

/// The selected passage and its neighbors in the same segment that still serve, in talk order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// The passages, in order.
    pub passages: Vec<WindowPassage>,
    /// Which of them is the selected passage.
    pub selected: usize,
}

impl Window {
    /// A window holding the passage alone.
    pub fn alone(passage: WindowPassage) -> Self {
        Self {
            passages: vec![passage],
            selected: 0,
        }
    }

    /// The window-wide offset of each passage's first word.
    fn offsets(&self) -> Vec<usize> {
        let mut at = 0;
        self.passages
            .iter()
            .map(|p| {
                let o = at;
                at += p.word_count();
                o
            })
            .collect()
    }

    /// Words in the whole window.
    pub fn len(&self) -> usize {
        self.passages.iter().map(WindowPassage::word_count).sum()
    }

    /// Whether the window holds no words.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The selected passage's words, as window-wide offsets.
    pub fn selected_range(&self) -> (usize, usize) {
        let offsets = self.offsets();
        let from = offsets.get(self.selected).copied().unwrap_or(0);
        let n = self
            .passages
            .get(self.selected)
            .map_or(0, WindowPassage::word_count);
        (from, from + n)
    }

    /// Window-wide offsets where an excerpt may start or end: every sentence start, and the
    /// window's end when its last word ends a sentence.
    pub fn boundaries(&self) -> Vec<usize> {
        let offsets = self.offsets();
        let mut out: Vec<usize> = self
            .passages
            .iter()
            .zip(&offsets)
            .flat_map(|(p, o)| p.starts().into_iter().map(move |s| s + o))
            .collect();
        let total = self.len();
        let last_ends = self
            .passages
            .last()
            .and_then(|p| p.text.split_whitespace().last())
            .is_some_and(ends_sentence);
        if last_ends {
            out.push(total);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The window's whole sentences as window-wide word ranges. Words before the first
    /// boundary or after the last belong to sentences the window does not hold whole.
    pub fn sentences(&self) -> Vec<(usize, usize)> {
        self.boundaries().windows(2).map(|w| (w[0], w[1])).collect()
    }

    /// The text of window-wide words `start..end`, sliced verbatim from each passage and joined
    /// by a space where it crosses from one passage to the next.
    pub fn text(&self, start: usize, end: usize) -> String {
        let mut parts = Vec::new();
        for (p, o) in self.passages.iter().zip(self.offsets()) {
            let ranges = p.word_ranges();
            let (s, e) = (start.max(o), end.min(o + ranges.len()));
            if s < e {
                parts.push(&p.text[ranges[s - o].0..ranges[e - o - 1].1]);
            }
        }
        parts.join(" ")
    }

    /// The span to store for window-wide words `start..end`.
    pub fn span(&self, start: usize, end: usize, chooser: Chooser) -> ExcerptSpan {
        let offsets = self.offsets();
        let mut covers = Vec::new();
        let (mut first, mut last_end) = (0, 0);
        for (p, o) in self.passages.iter().zip(&offsets) {
            let n = p.word_count();
            if start < o + n && end > *o {
                if covers.is_empty() {
                    first = start.saturating_sub(*o);
                }
                last_end = end.min(o + n) - o;
                covers.push(p.id);
            }
        }
        ExcerptSpan {
            passage_id: self.passages.get(self.selected).map_or_else(
                || {
                    covers
                        .first()
                        .copied()
                        .unwrap_or(PassageId::from_uuid(uuid::Uuid::nil()))
                },
                |p| p.id,
            ),
            covers,
            start: first,
            end: last_end,
            chooser,
        }
    }

    /// Checks a run of sentences, `first..=last` by index into [`Window::sentences`]: inside the
    /// window, overlapping the selected passage, and within the limits. Returns its words as
    /// window-wide offsets.
    pub fn check_run(&self, first: usize, last: usize, limits: Limits) -> Option<(usize, usize)> {
        let sentences = self.sentences();
        if first > last || last >= sentences.len() {
            return None;
        }
        let (start, end) = (sentences[first].0, sentences[last].1);
        let (a, b) = self.selected_range();
        let words = end - start;
        (start < b && end > a && words >= limits.min_words && words <= limits.max_words)
            .then_some((start, end))
    }

    /// The last resort: the selected passage trimmed to its first and last whole sentences when
    /// that keeps at least `min_words`, else the passage as it is, cut back to the excerpt
    /// limit at a boundary where it can be.
    pub fn trimmed(&self, limits: Limits) -> (usize, usize) {
        let (a, b) = self.selected_range();
        let bounds = self.boundaries();
        let s = bounds.iter().copied().find(|&x| x >= a && x < b);
        let e = bounds.iter().rev().copied().find(|&x| x <= b && x > a);
        let (start, mut end) = match (s, e) {
            (Some(s), Some(e)) if e > s && e - s >= limits.min_words => (s, e),
            _ => (a, b),
        };
        if end - start > limits.max_words {
            let cap = start + limits.max_words;
            end = bounds
                .iter()
                .rev()
                .copied()
                .find(|&x| x > start && x <= cap && x - start >= limits.min_words)
                .unwrap_or(cap);
        }
        (start, end)
    }

    /// The sentences the reranker step scores, by index into [`Window::sentences`]: those
    /// overlapping the selected passage and up to two on each side of them.
    pub fn sentences_near_selected(&self) -> Vec<usize> {
        let sentences = self.sentences();
        let (a, b) = self.selected_range();
        let inside: Vec<usize> = (0..sentences.len())
            .filter(|&i| sentences[i].0 < b && sentences[i].1 > a)
            .collect();
        match (inside.first(), inside.last()) {
            (Some(&f), Some(&l)) => {
                (f.saturating_sub(2)..=(l + 2).min(sentences.len() - 1)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// The reranker step's candidates: runs of consecutive scored sentences, `first..=last`,
    /// that overlap the selected passage and keep the limits, at most `max_spans` of them. Each
    /// starts from one of the best-scoring sentences inside the passage and grows outward,
    /// toward the better-scoring neighbor, one sentence at a time. `scores` holds a score for
    /// each sentence of [`Window::sentences_near_selected`].
    pub fn candidate_runs(
        &self,
        scores: &[(usize, f32)],
        limits: Limits,
        max_spans: usize,
    ) -> Vec<(usize, usize)> {
        let sentences = self.sentences();
        let (a, b) = self.selected_range();
        let score = |i: usize| scores.iter().find(|(j, _)| *j == i).map(|(_, s)| *s);
        let mut seeds: Vec<(usize, f32)> = scores
            .iter()
            .copied()
            .filter(|(i, _)| sentences.get(*i).is_some_and(|s| s.0 < b && s.1 > a))
            .collect();
        seeds.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (seed, _) in seeds {
            let (mut first, mut last) = (seed, seed);
            loop {
                if out.len() >= max_spans {
                    return out;
                }
                if self.check_run(first, last, limits).is_some() && !out.contains(&(first, last)) {
                    out.push((first, last));
                }
                let left = first.checked_sub(1).and_then(|i| score(i).map(|s| (i, s)));
                let right = score(last + 1).map(|s| (last + 1, s));
                let next = match (left, right) {
                    (Some(l), Some(r)) => {
                        if r.1 > l.1 {
                            r
                        } else {
                            l
                        }
                    }
                    (Some(l), None) => l,
                    (None, Some(r)) => r,
                    (None, None) => break,
                };
                let (nf, nl) = (first.min(next.0), last.max(next.0));
                if sentences[nl].1 - sentences[nf].0 > limits.max_words {
                    break;
                }
                (first, last) = (nf, nl);
            }
        }
        out
    }

    /// Whether window-wide words `start..end` begin, and whether they end, in the middle of a
    /// sentence.
    pub fn mid_sentence(&self, start: usize, end: usize) -> (bool, bool) {
        let bounds = self.boundaries();
        (!bounds.contains(&start), !bounds.contains(&end))
    }

    /// The window-wide offsets of a stored span, when every passage it covers is in the window
    /// in order.
    pub fn locate(&self, span: &ExcerptSpan) -> Option<(usize, usize)> {
        let first = span.covers.first()?;
        let i = self.passages.iter().position(|p| p.id == *first)?;
        let ids: Vec<PassageId> = self.passages[i..]
            .iter()
            .take(span.covers.len())
            .map(|p| p.id)
            .collect();
        if ids != span.covers {
            return None;
        }
        let offsets = self.offsets();
        let last = i + span.covers.len() - 1;
        let start = offsets[i] + span.start.min(self.passages[i].word_count());
        let end = offsets[last] + span.end.min(self.passages[last].word_count());
        (end > start).then_some((start, end))
    }
}

/// An excerpt as the page shows it, rebuilt from serving passages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    /// Its verbatim text.
    pub text: String,
    /// When its first word is spoken.
    pub start_ms: i32,
    /// The ordinal of the first passage it covers, for the transcript anchor.
    pub first_ordinal: i32,
    /// The first covered passage's character offset where the excerpt starts, and for each
    /// covered passage the character offset in the excerpt where its part begins, so footnote
    /// spans inside the excerpt keep their places.
    pub parts: Vec<ExcerptPart>,
    /// Words.
    pub words: usize,
    /// The span it was built from, or `None` for a whole passage shown as it is.
    pub span: Option<ExcerptSpan>,
}

/// One covered passage's part of an excerpt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExcerptPart {
    /// The passage.
    pub passage_id: PassageId,
    /// Where its part starts in the passage text, in characters.
    pub passage_from: usize,
    /// Where it ends in the passage text, in characters.
    pub passage_to: usize,
    /// Where it starts in the excerpt text, in characters.
    pub excerpt_from: usize,
}

impl Excerpt {
    /// Builds the excerpt for window-wide words `start..end`.
    pub fn build(window: &Window, start: usize, end: usize, span: Option<ExcerptSpan>) -> Self {
        let mut parts = Vec::new();
        let mut at = 0usize;
        let mut first = None;
        let mut offset = 0usize;
        for p in &window.passages {
            let ranges = p.word_ranges();
            let (s, e) = (start.max(offset), end.min(offset + ranges.len()));
            if s < e {
                let (bf, bt) = (ranges[s - offset].0, ranges[e - offset - 1].1);
                let chars = |b: usize| p.text[..b].chars().count();
                let (cf, ct) = (chars(bf), chars(bt));
                if !parts.is_empty() {
                    at += 1;
                }
                parts.push(ExcerptPart {
                    passage_id: p.id,
                    passage_from: cf,
                    passage_to: ct,
                    excerpt_from: at,
                });
                at += ct - cf;
                if first.is_none() {
                    first = Some((p.ms_at(s - offset), p.ordinal));
                }
            }
            offset += ranges.len();
        }
        let (start_ms, first_ordinal) = first.unwrap_or_else(|| {
            window
                .passages
                .get(window.selected)
                .map_or((0, 0), |p| (p.start_ms, p.ordinal))
        });
        Excerpt {
            text: window.text(start, end),
            start_ms,
            first_ordinal,
            parts,
            words: end.saturating_sub(start),
            span,
        }
    }

    /// The whole passage, for an answer stored before excerpts or a span that no longer fits.
    pub fn whole(passage: &WindowPassage) -> Self {
        let window = Window::alone(passage.clone());
        Self::build(&window, 0, window.len(), None)
    }

    /// Rebuilds a stored span from the serving rows of the passages it covers, in order. `None`
    /// when a covered passage no longer serves or the offsets no longer fit: the page then
    /// shows the passage itself.
    pub fn rebuild(span: &ExcerptSpan, rows: &[WindowPassage]) -> Option<Self> {
        let covered: Vec<WindowPassage> = span
            .covers
            .iter()
            .map(|id| rows.iter().find(|r| r.id == *id).cloned())
            .collect::<Option<_>>()?;
        let selected = covered.iter().position(|p| p.id == span.passage_id)?;
        let window = Window {
            passages: covered,
            selected,
        };
        let (start, end) = window.locate(span)?;
        Some(Self::build(&window, start, end, Some(span.clone())))
    }

    /// Maps a character span of `passage`'s text into the excerpt, when it falls inside it.
    pub fn map_span(&self, passage: PassageId, from: usize, to: usize) -> Option<(usize, usize)> {
        let part = self.parts.iter().find(|p| p.passage_id == passage)?;
        (from >= part.passage_from && to <= part.passage_to && from < to).then(|| {
            (
                part.excerpt_from + from - part.passage_from,
                part.excerpt_from + to - part.passage_from,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> PassageId {
        PassageId::from_uuid(Uuid::from_u128(n))
    }

    /// A passage whose sentences start where `text` has a capital after a full stop, stored as
    /// the index step would.
    fn passage(n: u128, text: &str, starts: &[i32]) -> WindowPassage {
        WindowPassage {
            id: id(n),
            text: text.into(),
            ordinal: i32::try_from(n).unwrap(),
            start_ms: i32::try_from(n).unwrap() * 100_000,
            sentence_starts: starts.to_vec(),
            sentence_start_ms: starts
                .iter()
                .map(|s| i32::try_from(n).unwrap() * 100_000 + s * 400)
                .collect(),
        }
    }

    fn window() -> Window {
        // The selected passage starts mid-sentence and ends mid-sentence.
        Window {
            passages: vec![
                passage(1, "We sit. When the mind wanders we", &[0, 2]),
                passage(2, "come back to the breath. That is all. And then", &[5, 8]),
                passage(3, "we begin again with care.", &[]),
            ],
            selected: 1,
        }
    }

    #[test]
    fn sentences_run_across_passages_and_leave_out_partial_ones() {
        let w = window();
        // Words: We(0) sit.(1) When(2)..we(6) come(7)..breath.(11) That(12)..all.(14)
        // And(15) then(16) we(17)..care.(21): the last passage has no stored starts and its
        // first word is lower case, so "And then we begin again with care." is one sentence.
        assert_eq!(w.boundaries(), vec![0, 2, 12, 15, 22]);
        assert_eq!(w.sentences(), vec![(0, 2), (2, 12), (12, 15), (15, 22)]);
        assert_eq!(
            w.text(2, 12),
            "When the mind wanders we come back to the breath."
        );
    }

    #[test]
    fn a_span_records_covered_passages_and_rebuilds_verbatim() {
        let w = window();
        let span = w.span(2, 15, Chooser::Model);
        assert_eq!(span.covers, vec![id(1), id(2)]);
        assert_eq!((span.start, span.end), (2, 8));
        assert_eq!(span.passage_id, id(2));
        let rebuilt = Excerpt::rebuild(&span, &w.passages).unwrap();
        assert_eq!(
            rebuilt.text,
            "When the mind wanders we come back to the breath. That is all."
        );
        assert_eq!(
            rebuilt.start_ms,
            100_000 + 2 * 400,
            "the first word's sentence"
        );
        assert_eq!(rebuilt.first_ordinal, 1);
        // Without the neighbor it cannot be rebuilt.
        assert!(Excerpt::rebuild(&span, &w.passages[1..]).is_none());
    }

    #[test]
    fn runs_outside_the_window_or_limits_or_the_passage_are_refused() {
        let w = window();
        let l = Limits {
            min_words: 3,
            max_words: 12,
        };
        assert_eq!(w.check_run(1, 1, l), Some((2, 12)));
        assert_eq!(
            w.check_run(0, 0, l),
            None,
            "does not touch the selected passage"
        );
        assert_eq!(w.check_run(1, 2, l), None, "over the limit");
        assert_eq!(w.check_run(2, 9, l), None, "outside the window");
        assert_eq!(w.check_run(2, 1, l), None, "backwards");
    }

    #[test]
    fn the_trimmed_passage_drops_its_partial_sentences() {
        let w = window();
        let l = Limits {
            min_words: 2,
            max_words: 250,
        };
        // The selected passage is words 7..17; its whole sentences run 12..15.
        assert_eq!(w.trimmed(l), (12, 15));
        assert_eq!(w.mid_sentence(12, 15), (false, false));
        assert_eq!(w.mid_sentence(7, 17), (true, true));
        // Too short once trimmed: the passage as it is.
        let l = Limits {
            min_words: 5,
            max_words: 250,
        };
        assert_eq!(w.trimmed(l), (7, 17));
    }

    #[test]
    fn a_long_passage_is_cut_to_the_limit_at_a_boundary() {
        let text: Vec<String> = (0..40).map(|i| format!("w{i}.")).collect();
        let p = passage(1, &text.join(" "), &(0..40).step_by(10).collect::<Vec<_>>());
        let w = Window::alone(p);
        let l = Limits {
            min_words: 5,
            max_words: 25,
        };
        assert_eq!(w.trimmed(l), (0, 20));
    }

    #[test]
    fn reranker_candidates_grow_from_the_best_sentence_inside_the_passage() {
        let w = window();
        assert_eq!(w.sentences_near_selected(), vec![0, 1, 2, 3]);
        let l = Limits {
            min_words: 3,
            max_words: 13,
        };
        // Sentence 2 scores best inside the passage; it grows toward sentence 1 (0.6 over 0.1).
        let scores = [(0, 0.2), (1, 0.6), (2, 0.9), (3, 0.1)];
        let runs = w.candidate_runs(&scores, l, 8);
        assert_eq!(runs[0], (2, 2));
        assert_eq!(runs[1], (1, 2));
        assert!(runs.iter().all(|r| w.check_run(r.0, r.1, l).is_some()));
        assert_eq!(w.candidate_runs(&scores, l, 1).len(), 1);
    }

    #[test]
    fn footnote_spans_map_into_the_excerpt() {
        let w = window();
        let e = Excerpt::build(&w, 2, 15, Some(w.span(2, 15, Chooser::Model)));
        // "back to the breath" in passage 2 starts at character 5.
        let p2 = &w.passages[1].text;
        let from = p2.find("back").unwrap();
        let to = from + "back to the breath".len();
        let (a, b) = e.map_span(id(2), from, to).unwrap();
        assert_eq!(&e.text[a..b], "back to the breath");
        // "And then" lies outside the excerpt.
        let from = p2.find("And").unwrap();
        assert_eq!(e.map_span(id(2), from, from + 3), None);
    }

    #[test]
    fn punctuation_stands_in_for_missing_boundaries() {
        let p = passage(1, "and so. We sit! Then rest", &[]);
        assert_eq!(p.starts(), vec![2, 4]);
        assert_eq!(p.ms_at(3), p.start_ms);
    }

    #[test]
    fn the_span_serializes_with_its_chooser() {
        let span = window().span(12, 15, Chooser::Reranker);
        let v = serde_json::to_value(&span).unwrap();
        assert_eq!(v["chooser"], "reranker");
        assert_eq!(serde_json::from_value::<ExcerptSpan>(v).unwrap(), span);
    }
}
