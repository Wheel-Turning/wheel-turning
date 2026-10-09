//! Aligning a source's own transcript to recognized words (Spec 04, Aligned source transcripts).
//! The source text keeps its wording; the recognizer's words, which are never stored, lend it
//! their times. A banded longest-common-subsequence alignment on normalized tokens matches the
//! two in order; `[mm:ss]` markers in the source anchor the words they precede; unmatched words
//! take times interpolated between their matched neighbors.

use std::sync::LazyLock;

use regex::Regex;
use wt_core::transcript::Word;

/// Below this share of source words matched, the text and the recording probably differ.
pub const MIN_ALIGNMENT_SHARE: f32 = 0.6;

static MARKER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^\[(\d{1,2}):(\d{2})(?::(\d{2}))?\]$").ok());
/// Stage directions such as `[laughs]` or `[Tape turned over.]`, but not time markers.
static DIRECTION: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\[[^\]\d][^\]]*\]").ok());
/// A speaker label opening a paragraph, such as `Student:` or `Q:`.
static LABEL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^(?:[A-Z][A-Za-z.' -]{0,30}|Q|A):\s+").ok());

/// A word of the source text, with a time marker if one stood before it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceWord {
    /// The word as written.
    pub text: String,
    /// The marker before it, in milliseconds.
    pub marker_ms: Option<i32>,
}

fn marker_ms(token: &str) -> Option<i32> {
    let c = MARKER.as_ref()?.captures(token)?;
    let a: i32 = c[1].parse().ok()?;
    let b: i32 = c[2].parse().ok()?;
    let ms = match c.get(3) {
        Some(s) => (a * 3600 + b * 60 + s.as_str().parse::<i32>().ok()?) * 1000,
        None => (a * 60 + b) * 1000,
    };
    Some(ms)
}

/// The source text's words: markers attached to the word after them, stage directions,
/// paragraph speaker labels and `@` tags removed.
pub fn source_words(text: &str) -> Vec<SourceWord> {
    labeled_source_words(text)
        .into_iter()
        .map(|(w, _)| w)
        .collect()
}

/// The same words, each with the speaker label of its paragraph, if the paragraph opened with
/// one (`SR:`, `Student A:`, `Q:`), for the text-only path (Spec 04, Text-only source
/// transcripts).
pub fn labeled_source_words(text: &str) -> Vec<(SourceWord, Option<String>)> {
    let mut out = Vec::new();
    let mut pending: Option<i32> = None;
    for paragraph in text.split(
        "

",
    ) {
        let mut p = paragraph.trim().to_owned();
        if let Some(re) = DIRECTION.as_ref() {
            p = re.replace_all(&p, " ").into_owned();
        }
        let mut label = None;
        if let Some(re) = LABEL.as_ref()
            && let Some(m) = re.find(&p)
        {
            label = Some(
                m.as_str()
                    .trim_end()
                    .trim_end_matches(':')
                    .trim()
                    .to_owned(),
            );
            p = p[m.end()..].to_owned();
        }
        for token in p.split_whitespace() {
            if let Some(ms) = marker_ms(token) {
                pending = Some(ms);
                continue;
            }
            if token.starts_with('@') {
                continue;
            }
            out.push((
                SourceWord {
                    text: token.to_owned(),
                    marker_ms: pending.take(),
                },
                label.clone(),
            ));
        }
    }
    out
}

fn key(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The aligned words and how many matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Aligned {
    /// The source's words, each with a time.
    pub words: Vec<Word>,
    /// The share of source words matched to a recognized word.
    pub share: f32,
}

/// Matched index pairs of a banded longest common subsequence.
fn lcs(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let band = n.abs_diff(m) + 300;
    // Row i covers columns centre(i) - band ..= centre(i) + band.
    let centre = |i: usize| -> usize { i * m / n };
    let width = 2 * band + 1;
    let lo = |i: usize| centre(i).saturating_sub(band);
    let mut len = vec![0_u32; (n + 1) * width];
    // Directions: 0 none, 1 diagonal, 2 up, 3 left.
    let mut dir = vec![0_u8; (n + 1) * width];
    let at = |i: usize, j: usize| -> Option<usize> {
        let l = lo(i);
        (j >= l && j < l + width).then(|| i * width + (j - l))
    };
    for i in 1..=n {
        let l = lo(i).max(1);
        let h = (lo(i) + width - 1).min(m);
        for j in l..=h {
            let Some(here) = at(i, j) else { continue };
            let up = at(i - 1, j).map_or(0, |k| len[k]);
            let left = at(i, j - 1).map_or(0, |k| len[k]);
            if a[i - 1] == b[j - 1] && !a[i - 1].is_empty() {
                let diag = at(i - 1, j - 1).map_or(0, |k| len[k]);
                len[here] = diag + 1;
                dir[here] = 1;
            } else if up >= left {
                len[here] = up;
                dir[here] = 2;
            } else {
                len[here] = left;
                dir[here] = 3;
            }
        }
    }
    // Trace back from the best cell in the last row.
    let mut best_j = 0;
    let mut best = 0;
    for j in lo(n).max(1)..=(lo(n) + width - 1).min(m) {
        if let Some(k) = at(n, j)
            && len[k] >= best
        {
            best = len[k];
            best_j = j;
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (n, best_j);
    while i > 0 && j > 0 {
        let Some(k) = at(i, j) else { break };
        match dir[k] {
            1 => {
                pairs.push((i - 1, j - 1));
                i -= 1;
                j -= 1;
            }
            2 => i -= 1,
            3 => j -= 1,
            _ => break,
        }
    }
    pairs.reverse();
    pairs
}

/// Aligns `source` to the recognized `timing` words.
pub fn align(source: &str, timing: &[Word]) -> Aligned {
    let words = source_words(source);
    let a: Vec<String> = words.iter().map(|w| key(&w.text)).collect();
    let b: Vec<String> = timing.iter().map(|w| key(&w.text)).collect();
    let pairs = lcs(&a, &b);
    let n = words.len();
    let mut start: Vec<Option<i32>> = vec![None; n];
    let mut end: Vec<Option<i32>> = vec![None; n];
    let mut prob = vec![0.0_f32; n];
    for &(i, j) in &pairs {
        start[i] = Some(timing[j].start_ms);
        end[i] = Some(timing[j].end_ms);
        prob[i] = timing[j].prob;
    }
    // A marker anchors a word nothing matched.
    for (i, w) in words.iter().enumerate() {
        if start[i].is_none()
            && let Some(ms) = w.marker_ms
        {
            start[i] = Some(ms);
            end[i] = Some(ms);
        }
    }
    let last_ms = timing.last().map_or(0, |w| w.end_ms);
    // Interpolate between known neighbours.
    let known: Vec<usize> = (0..n).filter(|&i| start[i].is_some()).collect();
    let mut out = Vec::with_capacity(n);
    let mut k = 0;
    for i in 0..n {
        while k < known.len() && known[k] < i {
            k += 1;
        }
        let (s, e) = match (start[i], end[i]) {
            (Some(s), Some(e)) => (s, e),
            _ => {
                let prev = known[..k].last().copied();
                let next = known.get(k).copied();
                let (p_idx, p_ms) = prev.map_or((None, 0), |p| (Some(p), end[p].unwrap_or(0)));
                let (n_idx, n_ms) =
                    next.map_or((None, last_ms), |q| (Some(q), start[q].unwrap_or(last_ms)));
                let from = p_idx.map_or(0, |p| p + 1);
                let to = n_idx.unwrap_or(n);
                let span = to.saturating_sub(from).max(1);
                let pos = i - from;
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let step = (n_ms - p_ms).max(0) / span as i32;
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let s = p_ms + step * pos as i32;
                (s, s + step.max(1))
            }
        };
        out.push(Word::new(words[i].text.clone(), s, e.max(s), prob[i]));
    }
    #[allow(clippy::cast_precision_loss)]
    let share = if n == 0 {
        0.0
    } else {
        pairs.len() as f32 / n as f32
    };
    Aligned { words: out, share }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(text: &str, every_ms: i32) -> Vec<Word> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, w)| {
                let i = i32::try_from(i).unwrap();
                Word::new(w, i * every_ms, i * every_ms + every_ms - 50, 0.9)
            })
            .collect()
    }

    #[test]
    fn reads_markers_directions_labels_and_tags() {
        let w = source_words(
            "[00:00]\n\nGood morning. [laughs] The pond\n\n[01:02]\n\nStudent: Is it still?\n\nNo. @Text_v004 @Score_JJ",
        );
        let texts: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Good", "morning.", "The", "pond", "Is", "it", "still?", "No."
            ]
        );
        assert_eq!(w[0].marker_ms, Some(0));
        assert_eq!(w[4].marker_ms, Some(62_000));
        assert_eq!(w[1].marker_ms, None);
    }

    #[test]
    fn each_word_keeps_its_paragraphs_label() {
        let w = labeled_source_words(
            "Good morning.

Student A: Is it still?

SR: No.",
        );
        let labels: Vec<Option<&str>> = w.iter().map(|(_, l)| l.as_deref()).collect();
        assert_eq!(
            labels,
            vec![
                None,
                None,
                Some("Student A"),
                Some("Student A"),
                Some("Student A"),
                Some("SR")
            ]
        );
    }

    #[test]
    fn source_words_take_the_times_of_the_words_they_match() {
        // The recognizer misheard two words and added one; the source has one it missed.
        let source = "The pond is still and clear this quiet morning by the hall";
        let heard = timing(
            "the pond is steel and clear um this quiet mourning by hall",
            400,
        );
        let a = align(source, &heard);
        assert_eq!(a.words.len(), 12);
        let texts: Vec<&str> = a.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts[3], "still", "the source's wording is kept");
        assert_eq!(a.words[1].start_ms, 400, "pond matched");
        assert_eq!(
            a.words[7].start_ms, 3200,
            "quiet matched after the inserted um"
        );
        // "still" sits between "is" (800) and "and" (1600).
        assert!(
            a.words[3].start_ms > 800 && a.words[3].start_ms < 1600,
            "{:?}",
            a.words[3]
        );
        assert!(a.words.windows(2).all(|p| p[0].start_ms <= p[1].start_ms));
        assert!((a.share - 9.0 / 12.0).abs() < 1e-6, "{}", a.share);
    }

    #[test]
    fn a_different_recording_aligns_poorly() {
        let a = align(
            "Entirely different words about mountains rivers and clouds far away",
            &timing("the pond is still and clear this quiet morning", 400),
        );
        assert!(a.share < MIN_ALIGNMENT_SHARE, "{}", a.share);
    }

    #[test]
    fn a_long_text_aligns_in_its_band() {
        let words: Vec<String> = (0..20_000).map(|i| format!("w{}", i % 997)).collect();
        let source = words.join(" ");
        let heard = timing(&source, 300);
        let a = align(&source, &heard);
        assert!(a.share > 0.99, "{}", a.share);
        assert_eq!(a.words[15_000].start_ms, 15_000 * 300);
    }
}
