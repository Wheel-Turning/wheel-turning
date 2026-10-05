//! The persisted form of an answer (`answer.payload`, Spec 05): mode, counts, theme text with
//! passage ids, and ordered passage ids. Never passage text. Takedown reads and rewrites it to
//! strip themes that cite removed passages.

use serde::{Deserialize, Serialize};

use crate::ids::PassageId;

/// One theme as stored: its one-line description and the passages that support it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTheme {
    /// The theme sentence.
    pub description: String,
    /// The supporting passages.
    pub passage_ids: Vec<PassageId>,
}

/// The JSON stored in `answer.payload`. Unknown fields are kept so older and newer builds can
/// read each other's answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerPayload {
    /// The themes shown above the passages.
    #[serde(default)]
    pub themes: Vec<StoredTheme>,
    /// The passages in the order they were shown.
    #[serde(default)]
    pub passage_ids: Vec<PassageId>,
    /// Everything else (mode, counts) as written.
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl AnswerPayload {
    /// Removes every theme that cites any of `removed`, and the removed passages from the order.
    /// Returns how many themes were dropped.
    pub fn strip_passages(&mut self, removed: &[PassageId]) -> usize {
        let before = self.themes.len();
        self.themes
            .retain(|theme| !theme.passage_ids.iter().any(|id| removed.contains(id)));
        self.passage_ids.retain(|id| !removed.contains(id));
        before - self.themes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn stripping_drops_themes_that_cite_removed_passages() {
        let keep = PassageId::from(Uuid::new_v4());
        let gone = PassageId::from(Uuid::new_v4());
        let mut payload: AnswerPayload = serde_json::from_value(serde_json::json!({
            "mode": "standard",
            "themes": [
                {"description": "A", "passage_ids": [keep, gone]},
                {"description": "B", "passage_ids": [keep]}
            ],
            "passage_ids": [keep, gone]
        }))
        .unwrap();
        assert_eq!(payload.strip_passages(&[gone]), 1);
        assert_eq!(payload.themes.len(), 1);
        assert_eq!(payload.themes[0].description, "B");
        assert_eq!(payload.passage_ids, vec![keep]);
        assert_eq!(payload.rest["mode"], "standard");
    }
}
