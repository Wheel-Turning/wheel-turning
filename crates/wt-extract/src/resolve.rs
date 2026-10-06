//! Citation resolution (Spec 04, Extraction and citation resolution). In order of precedence:
//! a hand-list match is `resolved`; a generic citation in a labeled series part, mapped by
//! `config/series.toml`, is `probable`; a model hint that fuzzy-matches a hand-listed locator is
//! `probable`; anything else is `unresolved`. The model never creates a locator: every locator
//! comes from the hand list.

use async_trait::async_trait;

use crate::extract::{Resolution, StagedCitation};
use crate::llm::LlmError;

/// Similarity at which a hand-list match counts as `resolved`.
pub const HAND_LIST_SIMILARITY: f32 = 0.85;
/// Similarity at which a model hint counts as `probable`.
pub const HINT_SIMILARITY: f32 = 0.7;

/// The hand list of locators.
#[async_trait]
pub trait HandList: Send + Sync {
    /// A locator whose normalized label or alias equals `text`, already normalized.
    async fn exact(&self, text: &str) -> Result<Option<String>, LlmError>;
    /// The locator most similar to `text`, with the similarity.
    async fn most_similar(&self, text: &str) -> Result<Option<(String, f32)>, LlmError>;
    /// The locator with this canonical key, such as `genjokoan:5`.
    async fn by_key(&self, key: &str) -> Result<Option<String>, LlmError>;
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
    // 4. Nothing matches: kept with its raw text and no locator.
    Ok(resolution("unresolved", None, None))
}
