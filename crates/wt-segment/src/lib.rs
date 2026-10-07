//! The `Diarizer` client, segment labels and student-speech removal (Spec 04, Diarization and
//! segmentation).

pub mod diarize;
pub mod segmenter;

pub use diarize::{
    DiarizeError, Diarizer, FakeDiarizer, FixedLabeler, LabelError, RuleFallbackLabeler,
    SidecarDiarizer, WindowLabeler,
};

pub use segmenter::{
    Label, MissingLabel, Plan, Review, SegmentDraft, SegmentSettings, Segmentation, Span, SpanKind,
    Turn, Window, plan,
};
