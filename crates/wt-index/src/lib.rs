//! The chunker and the index step (Spec 04, Chunking, embedding and indexing). This crate and
//! the takedown routine are the only code that reads the `passage` table directly.

pub mod chunker;

pub use chunker::{ChunkSettings, Passage, chunk};
