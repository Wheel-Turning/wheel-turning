//! Sensitive questions in the web app (Spec 05, Sensitive questions; Spec 06, Rendering
//! sensitive questions): the model layer behind the spend cap, and which notice a page shows.
//! The classifier itself is [`wt_retrieve::Sensitivity`].

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use wt_core::config::LlmStage;
use wt_extract::{DataClass, LlmClient, LlmRequest, complete_json};
use wt_retrieve::{Level, ModelCheck};

/// The model layer's prompt.
pub const PROMPT: &str = include_str!("../prompts/sensitive.md");

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Category {
    None,
    Medical,
    Distress,
    Acute,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Reply {
    /// One of the four categories.
    category: Category,
}

/// The model layer as a Haiku-class call through the shared client, so the monthly cap and the
/// real-data switch apply: a refused or failed call leaves the other layers' level standing.
pub struct LlmSensitivity {
    llm: Arc<dyn LlmClient>,
}

impl LlmSensitivity {
    /// Wraps the site's client.
    pub fn new(llm: Arc<dyn LlmClient>) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl ModelCheck for LlmSensitivity {
    async fn level(&self, question: &str) -> Result<Level, String> {
        let req = LlmRequest {
            stage: LlmStage::Sensitive,
            system: PROMPT.to_owned(),
            user: format!("<question>\n{question}\n</question>"),
            max_tokens: 64,
        };
        let reply: Reply = complete_json(self.llm.as_ref(), &req, DataClass::Real)
            .await
            .map_err(|e| e.to_string())?;
        Ok(match reply.category {
            Category::None => Level::None,
            Category::Medical => Level::Medical,
            Category::Distress => Level::Distress,
            Category::Acute => Level::Acute,
        })
    }
}

/// The notice an answer page shows above the question, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Banner {
    /// Spec 09's distress banner.
    Distress,
    /// Spec 09's medical banner.
    Medical,
    /// The acute notice's text, above the curated query's answer.
    Support,
}

impl Banner {
    /// The banner for a stored answer.
    pub fn for_answer(category: &str, curated: bool) -> Option<Self> {
        if curated {
            return Some(Banner::Support);
        }
        match Level::parse(category) {
            Some(Level::Distress) => Some(Banner::Distress),
            Some(Level::Medical) => Some(Banner::Medical),
            // An acute question has no answer row; fail high if one ever appears.
            Some(Level::Acute) => Some(Banner::Support),
            Some(Level::None) => None,
            None => Some(Banner::Distress),
        }
    }

    /// For the template.
    pub fn is_support(&self) -> bool {
        *self == Banner::Support
    }

    /// For the template.
    pub fn is_distress(&self) -> bool {
        *self == Banner::Distress
    }
}

/// What the answer page leaves out for a stored answer's category: `(summary, giving)`.
pub fn shows(category: &str, curated: bool) -> (bool, bool) {
    if curated {
        return (false, false);
    }
    // An unknown category fails high, as distress.
    let level = Level::parse(category).unwrap_or(Level::Distress);
    (level.shows_summary(), level.shows_giving())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;
    use wt_extract::{FakeLlm, UnavailableLlm};

    #[test]
    fn notices_and_omissions_follow_spec_05s_table() {
        assert_eq!(shows("none", false), (true, true));
        assert_eq!(shows("medical", false), (false, true));
        assert_eq!(shows("distress", false), (false, false));
        assert_eq!(shows("none", true), (false, false));
        assert_eq!(shows("garbled", false), (false, false));
        assert_eq!(Banner::for_answer("none", false), None);
        assert_eq!(Banner::for_answer("medical", false), Some(Banner::Medical));
        assert_eq!(
            Banner::for_answer("distress", false),
            Some(Banner::Distress)
        );
        assert_eq!(Banner::for_answer("none", true), Some(Banner::Support));
    }

    #[tokio::test]
    async fn the_model_layer_maps_the_reply_and_reports_a_refusal() {
        let llm = FakeLlm::new(vec![Ok(json!({"category": "acute"}))]);
        let m = LlmSensitivity::new(Arc::new(llm));
        assert_eq!(m.level("a synthetic question").await.unwrap(), Level::Acute);

        let m = LlmSensitivity::new(Arc::new(UnavailableLlm));
        assert!(m.level("a synthetic question").await.is_err());
    }
}
