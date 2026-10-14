//! The language-model `QuestionEditor` (Spec 04, Student questions): the student's words and the
//! start of the teacher's answer go to the extraction model, which returns the student's wording
//! lightly edited. The prompt is `wt-segment/prompts/student_question.md`; the checks that decide
//! whether the edit is stored run in `wt_segment::questions`.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use wt_core::config::LlmStage;
use wt_segment::questions::{PROMPT, user_message};
use wt_segment::{AnsweredTurn, EditError, QuestionEdit, QuestionEditor};

use crate::llm::{DataClass, LlmClient, LlmError, LlmRequest, complete_json};

/// The model's JSON answer: the edited question, and whether it removed identifying details.
#[derive(Debug, Deserialize, JsonSchema)]
struct Reply {
    /// The edited question.
    question: String,
    /// Whether the edit removed identifying details.
    removed_identifying_details: bool,
}

/// Edits student questions with the extraction model.
pub struct LlmQuestionEditor<'a> {
    /// The model client.
    client: &'a dyn LlmClient,
    /// The transcripts' data class, which the client checks before sending.
    class: DataClass,
    /// The model to ask.
    model: String,
}

impl<'a> LlmQuestionEditor<'a> {
    /// An editor for one talk; `class` is `Real` unless the talk is a fixture, and `model` is
    /// the model the client uses for the `question` stage.
    pub fn new(client: &'a dyn LlmClient, class: DataClass, model: impl Into<String>) -> Self {
        Self {
            client,
            class,
            model: model.into(),
        }
    }
}

#[async_trait]
impl QuestionEditor for LlmQuestionEditor<'_> {
    fn model(&self) -> &str {
        &self.model
    }

    async fn edit(&self, turn: &AnsweredTurn) -> Result<QuestionEdit, EditError> {
        let req = LlmRequest {
            stage: LlmStage::Question,
            system: PROMPT.into(),
            user: user_message(turn),
            max_tokens: 1_000,
        };
        let reply: Reply =
            complete_json(self.client, &req, self.class)
                .await
                .map_err(|e| match e {
                    LlmError::CapReached { .. } => EditError::CapReached(e.to_string()),
                    other => EditError::Failed(other.to_string()),
                })?;
        Ok(QuestionEdit {
            question: reply.question,
            removed_identifying_details: reply.removed_identifying_details,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FakeLlm;
    use serde_json::json;

    #[tokio::test]
    async fn the_edit_uses_the_question_stage_and_the_talks_data_class() {
        let fake = FakeLlm::new(vec![Ok(json!({
            "question": "How do I sit with pain?",
            "removed_identifying_details": true
        }))]);
        let editor = LlmQuestionEditor::new(&fake, DataClass::Real, "claude-haiku-4-5");
        let turn = AnsweredTurn {
            start_ms: 0,
            end_ms: 1,
            answer_start_ms: 2,
            student: "s1 s2".into(),
            answer: "t1".into(),
        };
        let edit = editor.edit(&turn).await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(edit.question, "How do I sit with pain?");
        assert!(edit.removed_identifying_details);
        assert_eq!(editor.model(), "claude-haiku-4-5");
        let seen = fake.seen.lock().map(|s| s.clone()).unwrap_or_default();
        assert_eq!(seen[0].0.stage, LlmStage::Question);
        assert_eq!(seen[0].1, DataClass::Real);
        assert!(seen[0].0.user.contains("s1 s2") && seen[0].0.user.contains("t1"));
    }
}
