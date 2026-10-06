//! The chunker (Spec 04, Chunking, embedding and indexing): a pure function from the corrected
//! words of one segment to passages. Passages never cross a segment boundary, never overlap,
//! and the output is identical across runs.

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use wt_core::records::sha256_hex;
use wt_core::transcript::Word;

/// `[chunk]` in `config/pipeline.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChunkSettings {
    /// A pause at least this long between words ends a sentence.
    pub sentence_pause_ms: i32,
    /// A passage closes once it reaches this many words.
    pub target_words: usize,
    /// No passage grows past this many words.
    pub max_words: usize,
    /// A pause at least this long closes a passage and starts a new stretch.
    pub hard_break_ms: i32,
    /// A stretch's last passage shorter than this merges into the one before it...
    pub min_words: usize,
    /// ...when the result stays within this many words.
    pub merge_cap_words: usize,
}

impl Default for ChunkSettings {
    fn default() -> Self {
        Self {
            sentence_pause_ms: 1200,
            target_words: 140,
            max_words: 220,
            hard_break_ms: 8000,
            min_words: 60,
            merge_cap_words: 260,
        }
    }
}

/// One passage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passage {
    /// Its words: the first and one past the last index into the segment's words.
    pub words: (usize, usize),
    /// The words joined by spaces.
    pub text: String,
    /// Start of the first word.
    pub start_ms: i32,
    /// End of the last word.
    pub end_ms: i32,
    /// Number of words.
    pub word_count: usize,
    /// SHA-256 over the text and offsets.
    pub content_hash: String,
}

/// Word ranges of sentences: Unicode sentence boundaries over the joined text, plus a pause of
/// `sentence_pause_ms`.
fn sentences(words: &[Word], cfg: &ChunkSettings) -> Vec<(usize, usize)> {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(words.len());
    for w in words {
        if !text.is_empty() {
            text.push(' ');
        }
        starts.push(text.len());
        text.push_str(w.text.trim());
    }
    // Word indexes at which a Unicode sentence starts.
    let mut breaks: Vec<usize> = text
        .split_sentence_bound_indices()
        .map(|(byte, _)| starts.partition_point(|&s| s < byte))
        .filter(|&i| i > 0 && i < words.len())
        .collect();
    for i in 1..words.len() {
        if words[i].start_ms - words[i - 1].end_ms >= cfg.sentence_pause_ms {
            breaks.push(i);
        }
    }
    breaks.sort_unstable();
    breaks.dedup();
    let mut out = Vec::with_capacity(breaks.len() + 1);
    let mut from = 0;
    for b in breaks {
        out.push((from, b));
        from = b;
    }
    if from < words.len() {
        out.push((from, words.len()));
    }
    out
}

/// Splits `words` into passages.
pub fn chunk(words: &[Word], cfg: &ChunkSettings) -> Vec<Passage> {
    let max = cfg.max_words.max(1);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    // Indexes into `ranges` where a stretch starts, after a hard break.
    let mut stretch_starts = vec![0];
    let mut open: Option<(usize, usize)> = None;

    for (s_from, s_to) in sentences(words, cfg) {
        // A hard break before this sentence closes the passage and starts a stretch.
        if s_from > 0 && words[s_from].start_ms - words[s_from - 1].end_ms >= cfg.hard_break_ms {
            if let Some(p) = open.take() {
                ranges.push(p);
            }
            stretch_starts.push(ranges.len());
        }
        // A sentence longer than the maximum is cut into pieces of the maximum.
        let mut from = s_from;
        while from < s_to {
            let to = (from + max).min(s_to);
            match open {
                Some((a, b)) if b - a + (to - from) > max => {
                    ranges.push((a, b));
                    open = Some((from, to));
                }
                Some((a, _)) => open = Some((a, to)),
                None => open = Some((from, to)),
            }
            if let Some((a, b)) = open
                && b - a >= cfg.target_words
            {
                ranges.push((a, b));
                open = None;
            }
            from = to;
        }
    }
    if let Some(p) = open {
        ranges.push(p);
    }

    // Each stretch's last passage merges into the one before it when short, within the cap.
    stretch_starts.push(ranges.len());
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for pair in stretch_starts.windows(2) {
        let stretch = &ranges[pair[0]..pair[1]];
        let mut kept: Vec<(usize, usize)> = stretch.to_vec();
        if kept.len() >= 2 {
            let (a, b) = kept[kept.len() - 1];
            let (pa, _) = kept[kept.len() - 2];
            if b - a < cfg.min_words && b - pa <= cfg.merge_cap_words {
                kept.pop();
                if let Some(last) = kept.last_mut() {
                    last.1 = b;
                }
            }
        }
        merged.extend(kept);
    }

    merged
        .into_iter()
        .filter(|(a, b)| b > a)
        .map(|(a, b)| {
            let text = words[a..b]
                .iter()
                .map(|w| w.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let (start_ms, end_ms) = (words[a].start_ms, words[b - 1].end_ms);
            let content_hash =
                sha256_hex(format!("{start_ms}\u{1f}{end_ms}\u{1f}{text}").as_bytes());
            Passage {
                words: (a, b),
                text,
                start_ms,
                end_ms,
                word_count: b - a,
                content_hash,
            }
        })
        .collect()
}
