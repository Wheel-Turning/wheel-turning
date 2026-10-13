//! What retrieval reports while it runs, for the live progress display (Spec 06, Live progress
//! while a question runs). Reporting never changes what retrieval computes.

/// A retrieval stage starting, with the count the stage before it produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The query is embedded and the searches start.
    Searching,
    /// The searches are done and the reranker starts.
    Weighing {
        /// Candidates read back from the fused list.
        candidates: usize,
    },
    /// Reranking is done and selection starts.
    Choosing {
        /// Candidates at or above the relevance threshold.
        shortlisted: usize,
    },
}

/// Hears each [`Step`] as it starts. Called inline, so an implementation must not block.
pub trait Progress: Send + Sync {
    /// `step` has started.
    fn step(&self, step: Step);
}

/// Reports nowhere: the plain query path and the evaluation run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Silent;

impl Progress for Silent {
    fn step(&self, _: Step) {}
}
