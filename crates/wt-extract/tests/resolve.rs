//! Spec 04, citation resolution on a fixture set with a hand list in memory. Similarity is a
//! simple trigram measure here; Postgres uses `pg_trgm` in production.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use async_trait::async_trait;
use wt_extract::LlmError;
use wt_extract::extract::{CitationKind, RawCitation, StagedCitation};
use wt_extract::resolve::{HandList, normalize, resolve};

struct Locator {
    id: &'static str,
    key: &'static str,
    label: &'static str,
    aliases: &'static [&'static str],
}

const HAND_LIST: &[Locator] = &[
    Locator {
        id: "L1",
        key: "genjokoan:1",
        label: "Genjokoan, paragraph 1",
        aliases: &["when all dharmas are the buddha dharma"],
    },
    Locator {
        id: "L5",
        key: "genjokoan:5",
        label: "Genjokoan, paragraph 5",
        aliases: &["to study the buddha way is to study the self"],
    },
    Locator {
        id: "B1",
        key: "blue-cliff-record:1",
        label: "Blue Cliff Record, case 1",
        aliases: &["vast emptiness nothing holy"],
    },
];

fn trigrams(s: &str) -> BTreeSet<String> {
    let padded = format!("  {} ", s.to_lowercase());
    let chars: Vec<char> = padded.chars().collect();
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// Shared trigrams over all distinct trigrams, as `pg_trgm` computes similarity.
fn similarity(a: &str, b: &str) -> f32 {
    let (x, y) = (trigrams(a), trigrams(b));
    let union = x.union(&y).count();
    if union == 0 {
        return 0.0;
    }
    x.intersection(&y).count() as f32 / union as f32
}

struct Memory;

#[async_trait]
impl HandList for Memory {
    async fn exact(&self, text: &str) -> Result<Option<String>, LlmError> {
        Ok(HAND_LIST
            .iter()
            .find(|l| normalize(l.label) == text || l.aliases.iter().any(|a| normalize(a) == text))
            .map(|l| l.id.to_owned()))
    }
    async fn most_similar(&self, text: &str) -> Result<Option<(String, f32)>, LlmError> {
        Ok(HAND_LIST
            .iter()
            .map(|l| {
                let best = std::iter::once(l.label)
                    .chain(l.aliases.iter().copied())
                    .map(|c| similarity(&normalize(c), text))
                    .fold(0.0, f32::max);
                (l.id.to_owned(), best)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1)))
    }
    async fn by_key(&self, key: &str) -> Result<Option<String>, LlmError> {
        Ok(HAND_LIST
            .iter()
            .find(|l| l.key == key)
            .map(|l| l.id.to_owned()))
    }
}

fn cite(text: &str, work: Option<&str>, place: Option<&str>) -> StagedCitation {
    StagedCitation {
        raw: RawCitation {
            text: text.into(),
            work_hint: work.map(str::to_owned),
            locator_hint: place.map(str::to_owned),
            kind: CitationKind::Quotation,
        },
        w0: 0,
        w1: 1,
        resolution: None,
    }
}

#[tokio::test]
async fn resolved_only_from_the_hand_list_probable_from_series_and_hints() {
    let cases = [
        // (citation, series locator, expected confidence, method, locator)
        (
            cite(
                "to study the Buddha way is to study the self",
                Some("Genjokoan"),
                None,
            ),
            None,
            "resolved",
            Some("hand_list"),
            Some("L5"),
        ),
        (
            cite(
                "Vast emptiness, nothing holy",
                Some("Blue Cliff Record"),
                None,
            ),
            None,
            "resolved",
            Some("hand_list"),
            Some("B1"),
        ),
        (
            cite("as Dogen says here in this passage", None, None),
            Some("genjokoan:1"),
            "probable",
            Some("series_label"),
            Some("L1"),
        ),
        (
            cite(
                "the teacher points us to it",
                Some("Genjokoan"),
                Some("paragraph five"),
            ),
            None,
            "probable",
            Some("llm"),
            Some("L5"),
        ),
        // A hint that matches nothing hand-listed: unresolved, and no locator is invented.
        (
            cite(
                "the moon in the dewdrop",
                Some("Mountains and Waters Sutra"),
                Some("section 12"),
            ),
            None,
            "unresolved",
            None,
            None,
        ),
        // A generic citation outside a mapped series part: unresolved.
        (
            cite("in this passage he is clear", None, None),
            None,
            "unresolved",
            None,
            None,
        ),
        // A generic citation whose series part is not in the hand list: unresolved.
        (
            cite("this passage", None, None),
            Some("genjokoan:9"),
            "unresolved",
            None,
            None,
        ),
    ];
    let mut resolved_right = 0;
    let mut resolved_total = 0;
    for (c, series, confidence, method, locator) in cases {
        let r = resolve(&c, "", series, &Memory).await.unwrap();
        assert_eq!(r.confidence, confidence, "{}", c.raw.text);
        assert_eq!(r.method.as_deref(), method, "{}", c.raw.text);
        assert_eq!(r.work_locator_id.as_deref(), locator, "{}", c.raw.text);
        if r.confidence == "resolved" {
            resolved_total += 1;
            resolved_right += usize::from(r.work_locator_id.as_deref() == locator);
        }
    }
    assert_eq!(
        resolved_right, resolved_total,
        "precision of resolved is 100 percent"
    );
}
