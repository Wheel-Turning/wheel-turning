//! Spec 04, Chunking: golden tests on synthetic segments. Words are invented.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use wt_core::transcript::Word;
use wt_index::{ChunkSettings, Passage, chunk};

/// Sentences of the given lengths, one word every 400 ms, with `pauses` (word index, extra
/// milliseconds) inserted before chosen words.
fn segment(lengths: &[usize], pauses: &[(usize, i32)]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut at = 0;
    let mut n = 0;
    for &len in lengths {
        for k in 0..len {
            if let Some((_, extra)) = pauses.iter().find(|(i, _)| *i == n) {
                at += extra;
            }
            // Capitalized sentence starts, as the recognizer writes them: Unicode sentence
            // boundaries do not break before a lowercase word.
            let stem = if k == 0 { "W" } else { "w" };
            let text = if k + 1 == len {
                format!("{stem}{n}.")
            } else {
                format!("{stem}{n}")
            };
            words.push(Word::new(text, at, at + 300, 0.9));
            at += 400;
            n += 1;
        }
    }
    words
}

fn ranges(passages: &[Passage]) -> Vec<(usize, usize)> {
    passages.iter().map(|p| p.words).collect()
}

fn check_invariants(words: &[Word], passages: &[Passage], cfg: &ChunkSettings) {
    // Contiguous, complete and non-overlapping.
    let mut next = 0;
    for p in passages {
        assert_eq!(p.words.0, next, "{:?}", ranges(passages));
        next = p.words.1;
        assert_eq!(p.word_count, p.words.1 - p.words.0);
        assert_eq!(p.start_ms, words[p.words.0].start_ms);
        assert_eq!(p.end_ms, words[p.words.1 - 1].end_ms);
        assert!(p.word_count <= cfg.merge_cap_words);
    }
    assert_eq!(next, words.len());
    // Timestamps only move forward.
    assert!(passages.windows(2).all(|w| w[0].end_ms <= w[1].start_ms));
}

#[test]
fn passages_stay_within_bounds_and_the_output_is_golden() {
    let cfg = ChunkSettings::default();
    // Sentences of 30 words: passages close at 150 (five sentences), and the 60-word tail
    // is long enough to stand alone.
    let words = segment(&[30; 12], &[]);
    let passages = chunk(&words, &cfg);
    check_invariants(&words, &passages, &cfg);
    assert_eq!(ranges(&passages), [(0, 150), (150, 300), (300, 360)]);
    for p in &passages {
        assert!(
            (cfg.min_words..=cfg.max_words).contains(&p.word_count),
            "{:?}",
            p.words
        );
    }
    assert!(passages[0].text.starts_with("W0 w1") && passages[0].text.ends_with("w149."));
    assert_eq!(passages, chunk(&words, &cfg), "identical across runs");
    assert_eq!(passages[0].content_hash.len(), 64);
}

#[test]
fn a_short_tail_merges_within_the_cap_and_otherwise_stays() {
    let cfg = ChunkSettings::default();
    // 150 + 20: the 20-word tail merges, giving 170.
    let merged = segment(&[30, 30, 30, 30, 30, 20], &[]);
    let p = chunk(&merged, &cfg);
    check_invariants(&merged, &p, &cfg);
    assert_eq!(ranges(&p), [(0, 170)]);

    // A 210-word passage and a 55-word tail would make 265, over the cap: the tail stays.
    let unmergeable = segment(&[110, 100, 55], &[]);
    let p = chunk(&unmergeable, &cfg);
    check_invariants(&unmergeable, &p, &cfg);
    assert_eq!(ranges(&p), [(0, 210), (210, 265)]);
}

#[test]
fn long_sentences_split_at_the_maximum_and_pauses_break() {
    let cfg = ChunkSettings::default();
    // One 500-word sentence: no passage may pass 220.
    let long = segment(&[500], &[]);
    let p = chunk(&long, &cfg);
    check_invariants(&long, &p, &cfg);
    assert!(
        p.iter().all(|p| p.word_count <= cfg.max_words),
        "{:?}",
        ranges(&p)
    );

    // An eight-second pause after word 100 closes the passage there, even mid-sentence, and
    // the short stretch before it is not merged across the pause.
    let paused = segment(&[100, 100, 100], &[(100, 8_000)]);
    let p = chunk(&paused, &cfg);
    check_invariants(&paused, &p, &cfg);
    assert_eq!(p[0].words, (0, 100));
    assert!(
        p.iter().all(|p| p.words.0 >= 100 || p.words.1 <= 100),
        "nothing crosses the pause"
    );

    // A 1.2-second pause ends a sentence without punctuation, so a passage can close there.
    let words = segment(&[300], &[(140, 1_500)]);
    let p = chunk(&words, &cfg);
    check_invariants(&words, &p, &cfg);
    assert_eq!(p[0].words, (0, 140));
}

#[test]
fn an_empty_segment_has_no_passages() {
    assert!(chunk(&[], &ChunkSettings::default()).is_empty());
}
