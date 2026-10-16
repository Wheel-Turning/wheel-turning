//! The language-model client, citation extraction and citation resolution (Spec 04,
//! Extraction and citation resolution).

// Private items carry docs too, as the public ones do through `missing_docs`.
#![warn(clippy::missing_docs_in_private_items)]

pub mod extract;
pub mod labeler;
pub mod llm;
pub mod question;
pub mod resolve;

pub use labeler::LlmLabeler;
pub use question::LlmQuestionEditor;

pub use llm::{
    AlertError, Alerter, AwsSns, ClaudeClient, ClaudeSettings, DataClass, FakeLlm, FakeSns, Ledger,
    LlmClient, LlmError, LlmRequest, LogAlerter, PgLedger, PriceTable, SnsAlerter, SnsPublisher,
    UnavailableLlm, alerter, api_schema, complete_json,
};
