//! The `Diarizer` client, segment labels and student-speech removal (Spec 04, Diarization and
//! segmentation).

pub mod diarize;
pub mod questions;
pub mod scope;
pub mod segmenter;

pub use diarize::{
    DiarizeError, Diarizer, FakeDiarizer, FixedLabeler, LabelError, RuleFallbackLabeler,
    SidecarDiarizer, WindowLabeler,
};

pub use questions::{EditError, EditedQuestion, FakeEditor, QuestionEdit, QuestionEditor};

pub use segmenter::{
    AnsweredTurn, Label, MissingLabel, Plan, Review, SegmentDraft, SegmentSettings, Segmentation,
    Span, SpanKind, Turn, Window, plan,
};
