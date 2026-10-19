//! Sensitive questions in the web app (Spec 05, Sensitive questions; Spec 06, Rendering
//! sensitive questions): which notice a page shows, and the triage tally. The classifier is
//! [`wt_retrieve::Sensitivity`] and the gate [`wt_retrieve::sensitive::triage`].

use sqlx::PgPool;
use wt_retrieve::sensitive::Layers;
use wt_retrieve::{Level, Triaged};

/// Counts what each layer said about one question in `triage_tally`, with no link to it. A
/// failure is logged and changes nothing the tester sees.
pub async fn tally(pool: &PgPool, layers: Layers, degraded: bool, gate: Triaged, level: Level) {
    const UNANSWERED: &str = "unanswered";
    let embedding = match layers.embedding {
        Some(l) => l.as_str(),
        None if degraded => "down",
        None => "skipped",
    };
    // A declined classification has no verdict but sets a level: `declined`, `out_of_scope`.
    let unanswered = if gate.declined {
        "declined"
    } else {
        UNANSWERED
    };
    let v = gate.verdict;
    let t = wt_db::triage::Tally {
        rules: layers.rules.as_str(),
        embedding,
        risk: v.map_or(unanswered, |v| v.risk.as_str()),
        gate_level: gate.level().map_or(UNANSWERED, Level::as_str),
        relevance: v.map_or(UNANSWERED, |v| v.relevance.as_str()),
        confidence: v.map_or(UNANSWERED, |v| v.confidence.as_str()),
        level: level.as_str(),
    };
    if let Err(e) = wt_db::triage::count(pool, &t).await {
        tracing::warn!(error = %e, "counting the triage verdicts");
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
            // Acute and out-of-scope questions have no answer row; fail high if one appears.
            Some(Level::Acute | Level::OutOfScope) => Some(Banner::Support),
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
}
