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

/// One unpunctuated stretch of `n` words, one every 400 ms, with `pauses` inserted.
fn unpunctuated(n: usize, pauses: &[(usize, i32)]) -> Vec<Word> {
    let mut at = 0;
    (0..n)
        .map(|i| {
            if let Some((_, extra)) = pauses.iter().find(|(j, _)| *j == i) {
                at += extra;
            }
            let w = Word::new(format!("w{i}"), at, at + 400, 0.9);
            at += 400;
            w
        })
        .collect()
}

#[test]
fn sentence_boundaries_never_change_the_passages() {
    let words = segment(&[30, 250, 12, 80], &[(30, 900)]);
    let wide = ChunkSettings {
        boundary_max_words: 10_000,
        ..ChunkSettings::default()
    };
    let a = chunk(&words, &ChunkSettings::default());
    let b = chunk(&words, &wide);
    let strip = |ps: Vec<Passage>| -> Vec<(String, String)> {
        ps.into_iter().map(|p| (p.text, p.content_hash)).collect()
    };
    assert_eq!(strip(a), strip(b));
}

#[test]
fn a_long_unpunctuated_stretch_splits_at_its_longest_pause() {
    // 100 words without punctuation, touching gaps everywhere but a pause before word 47 and
    // a shorter one before word 20.
    let words = unpunctuated(100, &[(47, 600), (20, 300)]);
    let starts = wt_index::chunker::sentence_boundaries(&words, &ChunkSettings::default());
    assert_eq!(starts[0], 0);
    assert!(starts.contains(&47), "{starts:?}");
    assert!(starts.contains(&20), "{starts:?}");
    let mut ends = starts[1..].to_vec();
    ends.push(100);
    for (s, e) in starts.iter().zip(&ends) {
        assert!(e - s <= 40, "{s}..{e} in {starts:?}");
    }
}

#[test]
fn without_any_pause_a_long_stretch_splits_every_30_words() {
    let words = unpunctuated(100, &[]);
    let starts = wt_index::chunker::sentence_boundaries(&words, &ChunkSettings::default());
    assert_eq!(starts, vec![0, 30, 60, 90]);
}

#[test]
fn passages_store_sentence_starts_as_word_offsets_with_their_times() {
    let words = segment(&[30; 12], &[]);
    let passages = chunk(&words, &ChunkSettings::default());
    for p in &passages {
        let n = i32::try_from(p.text.split_whitespace().count()).unwrap();
        assert_eq!(p.sentence_starts.len(), p.sentence_start_ms.len());
        assert!(p.sentence_starts.windows(2).all(|w| w[0] < w[1]));
        assert!(p.sentence_starts.iter().all(|&s| (0..n).contains(&s)));
        for (s, ms) in p.sentence_starts.iter().zip(&p.sentence_start_ms) {
            let word = p
                .text
                .split_whitespace()
                .nth(usize::try_from(*s).unwrap())
                .unwrap();
            assert!(word.starts_with('W'), "{word} starts a sentence");
            let j = p.words.0 + usize::try_from(*s).unwrap();
            assert_eq!(*ms, words[j].start_ms);
        }
    }
    // Passages built from whole sentences start on one.
    assert!(
        passages
            .iter()
            .all(|p| p.sentence_starts.first() == Some(&0))
    );
}
