//! Spec 04, Glossary correction: variants become canonical forms with valid timing, near misses
//! are suggested and never applied, and the version follows the content.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use wt_core::transcript::Word;
use wt_registry::glossary::{Glossary, GlossaryTerm, TermKind};

fn term(canonical: &str, kind: TermKind, variants: &[&str]) -> GlossaryTerm {
    GlossaryTerm {
        canonical: canonical.into(),
        kind,
        variants: variants.iter().map(|v| (*v).to_owned()).collect(),
    }
}

fn glossary() -> Glossary {
    Glossary::new(&[
        term("Dogen", TermKind::Name, &["doe gen", "dogan"]),
        term(
            "shikantaza",
            TermKind::Term,
            &["she can taza", "shikan taza"],
        ),
        term(
            "Genjokoan",
            TermKind::TextTitle,
            &["gen joe koan", "genjo koan"],
        ),
    ])
    .unwrap()
}

/// Words a quarter second apart, each 200 ms long.
fn words(text: &str) -> Vec<Word> {
    text.split_whitespace()
        .enumerate()
        .map(|(i, w)| {
            let start = i32::try_from(i).unwrap() * 250;
            Word::new(w, start, start + 200, 0.8)
        })
        .collect()
}

fn text(words: &[Word]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn variants_become_canonical_and_multi_word_timing_holds() {
    let g = glossary();
    let input = words("as doe gen said, she can taza is just sitting in the genjo koan.");
    let out = g.apply(&input);
    assert_eq!(
        text(&out),
        "as Dogen said, shikantaza is just sitting in the Genjokoan."
    );

    let dogen = out.iter().find(|w| w.text == "Dogen").unwrap();
    assert_eq!(
        (dogen.start_ms, dogen.end_ms),
        (input[1].start_ms, input[2].end_ms)
    );
    let shikantaza = out.iter().find(|w| w.text == "shikantaza").unwrap();
    assert_eq!(
        (shikantaza.start_ms, shikantaza.end_ms),
        (input[4].start_ms, input[6].end_ms)
    );
    assert!(
        out.windows(2).all(|w| w[0].end_ms <= w[1].start_ms),
        "monotone timing"
    );

    // A canonical form in the wrong case is corrected; a word merely containing a variant is not.
    assert_eq!(
        text(&g.apply(&words("dogen and dogenism"))),
        "Dogen and dogenism"
    );
    assert_eq!(
        g.apply(&words("nothing to change here")),
        words("nothing to change here")
    );
}

#[test]
fn near_misses_are_suggested_and_never_applied() {
    let g = glossary();
    let english = |w: &str| ["sitting", "practice"].contains(&w);
    let input = words("practice shikantasa while sitting and genjokoun");
    let suggestions = g.near_misses(&input, &english);
    let heard: Vec<_> = suggestions
        .iter()
        .map(|s| (s.heard.as_str(), s.canonical.as_str()))
        .collect();
    assert_eq!(
        heard,
        [("shikantasa", "shikantaza"), ("genjokoun", "Genjokoan")]
    );
    assert!(suggestions[0].context.contains("practice shikantasa while"));
    assert_eq!(g.apply(&input), input, "suggestions change nothing");
}

#[test]
fn the_version_follows_the_content_only() {
    let a = glossary();
    let reordered = Glossary::new(&[
        term(
            "Genjokoan",
            TermKind::TextTitle,
            &["genjo koan", "gen joe koan"],
        ),
        term(
            "shikantaza",
            TermKind::Term,
            &["shikan taza", "she can taza"],
        ),
        term("Dogen", TermKind::Name, &["dogan", "doe gen"]),
    ])
    .unwrap();
    assert_eq!(a.version(), reordered.version(), "order does not matter");
    let more = Glossary::new(&[
        term("Dogen", TermKind::Name, &["doe gen", "dogan", "dough gen"]),
        term(
            "shikantaza",
            TermKind::Term,
            &["she can taza", "shikan taza"],
        ),
        term(
            "Genjokoan",
            TermKind::TextTitle,
            &["gen joe koan", "genjo koan"],
        ),
    ])
    .unwrap();
    assert_ne!(a.version(), more.version(), "a new variant changes it");
    assert_eq!(a.version().len(), 64);
}

#[test]
fn each_corrected_word_knows_its_source_words() {
    let g = glossary();
    let input = words("as doe gen said");
    let mapped = g.apply_mapped(&input);
    let sources: Vec<(String, (usize, usize))> =
        mapped.into_iter().map(|(w, src)| (w.text, src)).collect();
    assert_eq!(
        sources,
        [
            ("as".to_owned(), (0, 1)),
            ("Dogen".to_owned(), (1, 3)),
            ("said".to_owned(), (3, 4)),
        ]
    );
}
