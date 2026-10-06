//! The `Diarizer` client, segment labels and student-speech removal (Spec 04, Diarization and
//! segmentation).

pub mod segmenter;

pub use segmenter::{
    Label, MissingLabel, Plan, Review, SegmentDraft, SegmentSettings, Segmentation, Span, SpanKind,
    Turn, Window, plan,
};
