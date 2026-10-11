//! Words with timing, shared by transcription, segmentation, glossary correction and chunking
//! (Spec 04, Transcription).

use serde::{Deserialize, Serialize};

/// The line every displayed student question carries (Spec 04, Student questions, step 5).
pub const STUDENT_QUESTION_LABEL: &str =
    "Question lightly edited by machine; names and identifying details removed.";

/// Who a student question is attributed to: never a name.
pub const STUDENT_ATTRIBUTION: &str = "Student";

/// What a student gap shows when no edited question is stored.
pub const STUDENT_GAP_LABEL: &str = "A student asked a question";

/// One recognized word.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// The word as recognized or corrected.
    pub text: String,
    /// Start, milliseconds from the start of the audio.
    pub start_ms: i32,
    /// End, milliseconds from the start of the audio.
    pub end_ms: i32,
    /// The recognizer's probability, 0 to 1.
    pub prob: f32,
}

impl Word {
    /// A word.
    pub fn new(text: impl Into<String>, start_ms: i32, end_ms: i32, prob: f32) -> Self {
        Self {
            text: text.into(),
            start_ms,
            end_ms,
            prob,
        }
    }

    /// The middle of the word, which decides the window it belongs to.
    pub fn mid_ms(&self) -> i32 {
        self.start_ms + (self.end_ms - self.start_ms) / 2
    }
}
