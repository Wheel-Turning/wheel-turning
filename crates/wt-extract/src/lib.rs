//! The language-model client, citation extraction and citation resolution (Spec 04,
//! Extraction and citation resolution).

pub mod extract;
pub mod labeler;
pub mod llm;
pub mod question;
pub mod resolve;

pub use labeler::LlmLabeler;
pub use question::LlmQuestionEditor;

pub use llm::{
    Alerter, ClaudeClient, ClaudeSettings, DataClass, FakeLlm, Ledger, LlmClient, LlmError,
    LlmRequest, LogAlerter, PgLedger, PriceTable, UnavailableLlm, api_schema, complete_json,
};
