//! The language-model `WindowLabeler` (Spec 04, Diarization and segmentation, step 4): labels
//! the windows the rules leave ambiguous, with a one-sentence reason each, kept in the job log.
//! Each window's text holds only the teacher's words.

use std::collections::BTreeMap;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use wt_core::config::LlmStage;
use wt_segment::{Label, LabelError, Window, WindowLabeler};

use crate::llm::{DataClass, LlmClient, LlmError, LlmRequest, complete_json};

/// The labeler's instructions (Spec 04): four labels and a reason for each window.
const SYSTEM: &str = "You label stretches of a recorded Zen service. Each window is about a \
minute of the teacher's words, transcribed automatically. Label each window as one of: talk \
(the teacher teaching), chant (liturgy recited or sung), announcement (practical notices such \
as schedules, thanks or reminders), or other (anything else, such as silence, instructions for \
sitting, or unclear speech). Give a one-sentence reason for each label. Label every window you \
are given, by its index.";

/// The model's JSON answer: one label per window.
#[derive(Debug, Deserialize, JsonSchema)]
struct Labels {
    /// One label per window given.
    windows: Vec<WindowLabel>,
}

/// One window's label and the reason the model gave.
#[derive(Debug, Deserialize, JsonSchema)]
struct WindowLabel {
    /// The window's index as given.
    index: usize,
    /// Its label.
    label: LabelValue,
    /// Why, in one sentence.
    reason: String,
}

/// The labels the model may give.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum LabelValue {
    /// The teacher teaching.
    Talk,
    /// Liturgy recited or sung.
    Chant,
    /// Practical notices such as schedules, thanks or reminders.
    Announcement,
    /// Anything else.
    Other,
}

impl From<LabelValue> for Label {
    fn from(v: LabelValue) -> Self {
        match v {
            LabelValue::Talk => Label::Talk,
            LabelValue::Chant => Label::Chant,
            LabelValue::Announcement => Label::Announcement,
            LabelValue::Other => Label::Other,
        }
    }
}

/// Labels ambiguous windows with the extraction model.
pub struct LlmLabeler<'a> {
    /// The model client.
    client: &'a dyn LlmClient,
    /// The transcripts' data class, which the client checks before sending.
    class: DataClass,
}

impl<'a> LlmLabeler<'a> {
    /// A labeler for one talk; `class` is `Real` unless the talk is a fixture.
    pub fn new(client: &'a dyn LlmClient, class: DataClass) -> Self {
        Self { client, class }
    }
}

#[async_trait]
impl WindowLabeler for LlmLabeler<'_> {
    async fn label(&self, windows: &[Window]) -> Result<BTreeMap<usize, Label>, LabelError> {
        let mut user = String::new();
        for w in windows {
            user.push_str(&format!(
                "Window {} ({}s to {}s):\n{}\n\n",
                w.index,
                w.start_ms / 1000,
                w.end_ms / 1000,
                if w.text.is_empty() {
                    "(no words)"
                } else {
                    &w.text
                }
            ));
        }
        let req = LlmRequest {
            stage: LlmStage::Classify,
            system: SYSTEM.into(),
            user,
            max_tokens: u32::try_from(200 + 80 * windows.len())
                .unwrap_or(16_000)
                .min(16_000),
        };
        let out: Labels =
            complete_json(self.client, &req, self.class)
                .await
                .map_err(|e| match e {
                    LlmError::CapReached { .. } => LabelError::CapReached(e.to_string()),
                    other => LabelError::Failed(other.to_string()),
                })?;
        let wanted: Vec<usize> = windows.iter().map(|w| w.index).collect();
        let mut labels = BTreeMap::new();
        for w in out
            .windows
            .into_iter()
            .filter(|w| wanted.contains(&w.index))
        {
            tracing::info!(window = w.index, label = ?w.label, reason = %w.reason, "window labeled");
            labels.insert(w.index, Label::from(w.label));
        }
        if let Some(missing) = wanted.iter().find(|i| !labels.contains_key(i)) {
            return Err(LabelError::Failed(format!(
                "the model left window {missing} unlabeled"
            )));
        }
        Ok(labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FakeLlm;
    use serde_json::json;

    fn window(index: usize) -> Window {
        Window {
            index,
            start_ms: 60_000,
            end_ms: 120_000,
            text: "fixture words".into(),
        }
    }

    #[tokio::test]
    async fn labels_come_back_by_index_and_gaps_are_errors() {
        let fake = FakeLlm::new(vec![Ok(json!({"windows": [
            {"index": 3, "label": "announcement", "reason": "schedule notices"},
            {"index": 7, "label": "talk", "reason": "teaching"},
            {"index": 99, "label": "chant", "reason": "not asked for"}
        ]}))]);
        let labeler = LlmLabeler::new(&fake, DataClass::Synthetic);
        let labels = labeler
            .label(&[window(3), window(7)])
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(labels.get(&3), Some(&Label::Announcement));
        assert_eq!(labels.get(&7), Some(&Label::Talk));
        assert_eq!(labels.len(), 2, "unasked windows are ignored");
        let seen = fake.seen.lock().map(|s| s.clone()).unwrap_or_default();
        assert_eq!(seen[0].0.stage, LlmStage::Classify);
        assert_eq!(seen[0].1, DataClass::Synthetic);

        let short = FakeLlm::new(vec![Ok(json!({"windows": [
            {"index": 3, "label": "talk", "reason": "teaching"}
        ]}))]);
        let err = LlmLabeler::new(&short, DataClass::Synthetic)
            .label(&[window(3), window(7)])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("window 7"), "{err}");
    }
}
