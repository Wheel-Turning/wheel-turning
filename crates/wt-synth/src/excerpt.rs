//! The model step of excerpt selection (Spec 05, Answer assembly, rule 8, step 1). The model
//! sees the question and the window's sentences, numbered, and returns sentence numbers as JSON,
//! never text (CLAUDE.md rule 5). Code checks the run; anything outside the window, off the
//! selected passage or outside the limits is refused, and the next step decides.

use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use wt_core::config::LlmStage;
use wt_core::excerpt::{Limits, Window};
use wt_extract::{DataClass, LlmClient, LlmError, LlmRequest, complete_json};

/// The excerpt prompt.
pub const EXCERPT_PROMPT: &str = include_str!("../prompts/excerpt.md");

/// The model's reply: the first and last sentence of the run, numbered from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ExcerptReply {
    /// The first sentence.
    pub start: i64,
    /// The last sentence.
    pub end: i64,
}

/// Why the model step gave no run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelMiss {
    /// The window holds no whole sentence that touches the passage.
    NoSentences,
    /// The month's cap is reached.
    CapReached,
    /// The call failed or timed out.
    Failed(String),
    /// The reply named a run the checks refuse.
    Refused {
        /// As returned.
        start: i64,
        /// As returned.
        end: i64,
    },
}

impl ModelMiss {
    /// The reason as logged.
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelMiss::NoSentences => "no_sentences",
            ModelMiss::CapReached => "cap_reached",
            ModelMiss::Failed(_) => "failed",
            ModelMiss::Refused { .. } => "refused",
        }
    }
}

/// The user message: the question, the numbered sentences and which belong to the passage.
pub fn message(query: &str, window: &Window) -> Option<String> {
    let sentences = window.sentences();
    let (a, b) = window.selected_range();
    let inside: Vec<usize> = (0..sentences.len())
        .filter(|&i| sentences[i].0 < b && sentences[i].1 > a)
        .collect();
    let (first, last) = (inside.first()?, inside.last()?);
    let mut text = format!("Question: {query}\n\nSentences:\n");
    for (i, (s, e)) in sentences.iter().enumerate() {
        text.push_str(&format!(
            "[{}] ({} words) {}\n",
            i + 1,
            e - s,
            window.text(*s, *e)
        ));
    }
    text.push_str(&format!(
        "\nThe found passage covers sentences {} to {}.\n",
        first + 1,
        last + 1
    ));
    Some(text)
}

/// Asks the model for a run of the window's sentences and checks it. Returns window-wide word
/// offsets.
pub async fn choose(
    llm: &dyn LlmClient,
    query: &str,
    window: &Window,
    limits: Limits,
    timeout: Duration,
    class: DataClass,
) -> Result<(usize, usize), ModelMiss> {
    let user = message(query, window).ok_or(ModelMiss::NoSentences)?;
    let request = LlmRequest {
        stage: LlmStage::Excerpt,
        system: EXCERPT_PROMPT.to_owned(),
        user,
        max_tokens: 50,
    };
    let reply =
        match tokio::time::timeout(timeout, complete_json::<ExcerptReply>(llm, &request, class))
            .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(LlmError::CapReached { .. })) => return Err(ModelMiss::CapReached),
            Ok(Err(e)) => return Err(ModelMiss::Failed(e.to_string())),
            Err(_) => {
                return Err(ModelMiss::Failed(format!(
                    "no reply within {} s",
                    timeout.as_secs_f32()
                )));
            }
        };
    let index = |n: i64| usize::try_from(n).ok().and_then(|n| n.checked_sub(1));
    let refused = ModelMiss::Refused {
        start: reply.start,
        end: reply.end,
    };
    let (Some(first), Some(last)) = (index(reply.start), index(reply.end)) else {
        return Err(refused);
    };
    window.check_run(first, last, limits).ok_or(refused)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;
    use uuid::Uuid;
    use wt_core::excerpt::WindowPassage;
    use wt_core::ids::PassageId;
    use wt_extract::FakeLlm;

    fn window() -> Window {
        let p = |n: u128, text: &str, starts: &[i32]| WindowPassage {
            id: PassageId::from_uuid(Uuid::from_u128(n)),
            text: text.into(),
            ordinal: 0,
            start_ms: 0,
            sentence_starts: starts.to_vec(),
            sentence_start_ms: vec![0; starts.len()],
        };
        Window {
            passages: vec![
                p(1, "First we sit down. Then we", &[0, 4]),
                p(
                    2,
                    "notice the breath going out. It is enough. Later",
                    &[5, 8],
                ),
                p(3, "we stand up and walk slowly.", &[]),
            ],
            selected: 1,
        }
    }

    fn limits() -> Limits {
        Limits {
            min_words: 3,
            max_words: 250,
        }
    }

    #[tokio::test]
    async fn a_valid_reply_becomes_a_run_and_the_message_holds_no_reply_text() {
        let llm = FakeLlm::new(vec![Ok(json!({"start": 2, "end": 3}))]);
        let w = window();
        let got = choose(
            &llm,
            "breath?",
            &w,
            limits(),
            Duration::from_secs(5),
            DataClass::Synthetic,
        )
        .await
        .unwrap();
        assert_eq!(
            w.text(got.0, got.1),
            "Then we notice the breath going out. It is enough."
        );
        let seen = llm.seen.lock().unwrap();
        assert_eq!(seen[0].0.stage, LlmStage::Excerpt);
        assert!(
            seen[0]
                .0
                .user
                .contains("[2] (7 words) Then we notice the breath going out.")
        );
        assert!(seen[0].0.user.contains("covers sentences 2 to 4"));
    }

    #[tokio::test]
    async fn replies_outside_the_window_or_the_passage_are_refused() {
        for (start, end) in [(0, 1), (1, 1), (3, 9), (3, 2)] {
            let llm = FakeLlm::new(vec![Ok(json!({"start": start, "end": end}))]);
            let got = choose(
                &llm,
                "q",
                &window(),
                limits(),
                Duration::from_secs(5),
                DataClass::Synthetic,
            )
            .await;
            assert_eq!(
                got,
                Err(ModelMiss::Refused { start, end }),
                "{start}..{end}"
            );
        }
    }

    #[tokio::test]
    async fn a_run_over_the_limit_is_refused() {
        let llm = FakeLlm::new(vec![Ok(json!({"start": 1, "end": 4}))]);
        let l = Limits {
            min_words: 3,
            max_words: 10,
        };
        let got = choose(
            &llm,
            "q",
            &window(),
            l,
            Duration::from_secs(5),
            DataClass::Synthetic,
        )
        .await;
        assert!(matches!(got, Err(ModelMiss::Refused { .. })));
    }
}
