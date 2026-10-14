//! Citation resolution (Spec 04, Extraction and citation resolution). In order of precedence:
//! a hand-list match is `resolved`; a generic citation in a labeled series part, mapped by
//! `config/series.toml`, is `probable`; a model hint that fuzzy-matches a hand-listed locator is
//! `probable`; a work named without a place in it resolves to the work's work-level locator
//! (Spec 08, Work-level citations); anything else is `unresolved`. The model never creates a
//! locator: every locator comes from the hand list.

use std::collections::BTreeSet;

use async_trait::async_trait;

use crate::extract::{Resolution, StagedCitation};
use crate::llm::LlmError;

/// Similarity at which a hand-list match counts as `resolved`.
pub const HAND_LIST_SIMILARITY: f32 = 0.85;
/// Similarity at which a model hint counts as `probable`.
pub const HINT_SIMILARITY: f32 = 0.7;

/// A work-level locator with the names that identify its work: its title and aliases, already
/// past the stoplist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkNames {
    /// The work-level locator.
    pub id: String,
    /// The work's title and aliases.
    pub names: Vec<String>,
}

/// The hand list of locators. `exact`, `most_similar` and `by_key` see only passage-level
/// locators; `works` lists the work-level ones.
#[async_trait]
pub trait HandList: Send + Sync {
    /// A locator whose normalized label or alias equals `text`, already normalized.
    async fn exact(&self, text: &str) -> Result<Option<String>, LlmError>;
    /// The locator most similar to `text`, with the similarity.
    async fn most_similar(&self, text: &str) -> Result<Option<(String, f32)>, LlmError>;
    /// The locator with this canonical key, such as `genjokoan:5`.
    async fn by_key(&self, key: &str) -> Result<Option<String>, LlmError>;
    /// Every work-level locator with its work's names.
    async fn works(&self) -> Result<Vec<WorkNames>, LlmError>;
}

/// Whether a work name may identify a work: not blank once normalized, and not a phrase on
/// `[citations] work_alias_stoplist`, compared normalized.
pub fn usable_work_name(name: &str, stoplist: &[String]) -> bool {
    let name = normalize(name);
    !name.is_empty() && !stoplist.iter().any(|s| normalize(s) == name)
}

/// Why a new work title or alias would be a poor work name, if it would: on the stoplist, so it
/// never matches, or shorter than three characters.
pub fn work_name_warning(name: &str, stoplist: &[String]) -> Option<String> {
    if !usable_work_name(name, stoplist) {
        Some(format!(
            "work name {name:?} is on work_alias_stoplist and never matches"
        ))
    } else if normalize(name).chars().count() < 3 {
        Some(format!(
            "work name {name:?} is shorter than three characters"
        ))
    } else {
        None
    }
}

/// The work-level locators with a name that appears as whole words in `text`.
fn works_named<'a>(works: &'a [WorkNames], text: &str) -> BTreeSet<&'a str> {
    let hay = format!(" {} ", normalize(text));
    works
        .iter()
        .filter(|w| {
            w.names
                .iter()
                .map(|n| normalize(n))
                .any(|n| !n.is_empty() && hay.contains(&format!(" {n} ")))
        })
        .map(|w| w.id.as_str())
        .collect()
}

/// Phrases that point at "the text we are reading" without naming it.
const GENERIC: &[&str] = &[
    "this passage",
    "this text",
    "this fascicle",
    "this section",
    "this chapter",
    "this verse",
    "this case",
    "this koan",
    "this line",
    "says here",
    "here he says",
    "here she says",
    "as dogen says here",
];

/// Whether a citation points at the series' text without naming a place in it.
pub fn is_generic(c: &StagedCitation) -> bool {
    if c.raw.locator_hint.is_some() {
        return false;
    }
    let text: String = c
        .raw
        .text
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    GENERIC.iter().any(|g| text.contains(g))
}

/// Lowercase, punctuation as spaces, spaces collapsed: the form text is matched in. Hand lists
/// compare their labels and aliases in the same form.
pub fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A resolution with its confidence, how it was reached, and the locator it found.
fn resolution(confidence: &str, method: Option<&str>, id: Option<String>) -> Resolution {
    Resolution {
        confidence: confidence.into(),
        method: method.map(str::to_owned),
        work_locator_id: id,
    }
}

/// Resolves one citation. `context` is the words around it; `series_locator` is the locator
/// `config/series.toml` maps the talk's series part to, if any.
pub async fn resolve(
    c: &StagedCitation,
    context: &str,
    series_locator: Option<&str>,
    hand_list: &dyn HandList,
) -> Result<Resolution, LlmError> {
    let hints = [c.raw.work_hint.as_deref(), c.raw.locator_hint.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    // 1. Hand list: the text, the hints or the context, exactly or at 0.85.
    let candidates: Vec<String> = [c.raw.text.as_str(), hints.as_str(), context]
        .into_iter()
        .map(normalize)
        .filter(|s| !s.is_empty())
        .collect();
    for text in &candidates {
        if let Some(id) = hand_list.exact(text).await? {
            return Ok(resolution("resolved", Some("hand_list"), Some(id)));
        }
    }
    for text in &candidates {
        if let Some((id, sim)) = hand_list.most_similar(text).await?
            && sim >= HAND_LIST_SIMILARITY
        {
            return Ok(resolution("resolved", Some("hand_list"), Some(id)));
        }
    }
    // 2. Series label: a generic citation in a mapped series part.
    if let Some(key) = series_locator
        && is_generic(c)
        && let Some(id) = hand_list.by_key(key).await?
    {
        return Ok(resolution("probable", Some("series_label"), Some(id)));
    }
    // 3. Model hint: both hints, fuzzy-matching a hand-listed locator.
    if c.raw.work_hint.is_some()
        && c.raw.locator_hint.is_some()
        && let Some((id, sim)) = hand_list.most_similar(&normalize(&hints)).await?
        && sim >= HINT_SIMILARITY
    {
        return Ok(resolution("probable", Some("llm"), Some(id)));
    }
    // 4. Work only: a work named in the citation is `resolved`, one named only around it or in
    // the model's hint is `probable`, and two works named is left unresolved.
    let works = hand_list.works().await?;
    let in_text = works_named(&works, &c.raw.text);
    let (confidence, ids) = if in_text.is_empty() {
        let mut around = works_named(&works, context);
        around.extend(works_named(
            &works,
            c.raw.work_hint.as_deref().unwrap_or_default(),
        ));
        ("probable", around)
    } else {
        ("resolved", in_text)
    };
    if ids.len() == 1
        && let Some(id) = ids.first()
    {
        return Ok(resolution(
            confidence,
            Some("work_name"),
            Some((*id).to_owned()),
        ));
    }
    // 5. Nothing matches: kept with its raw text and no locator.
    Ok(resolution("unresolved", None, None))
}
