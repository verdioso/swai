//! SWAI — Frontier Auditor client (`swai-core::council::frontier`).
//!
//! Phase 35.2: a stateless, **read-only** review client that sends an
//! externally-configured frontier-model API key (Claude / GPT / Gemini) to a
//! third-party endpoint to review a diff, and parses the structured findings
//! back into SWAI's UI.
//!
//! This module is split across several files by concern:
//! - [`provider`]: frontier LLM provider metadata (endpoints, auth headers).
//! - [`scope`]: audit scope, changed files, findings, and severities.
//! - [`error`]: audit error types.
//! - [`transport`]: the injectable HTTP transport abstraction.
//! - [`client`]: the stateless [`FrontierAuditor`] review client.
//! - [`request`]: request-building and response-text extraction.
//! - [`parse`]: response parsing into structured findings.
//!
//! Design goals (see Phase 35 spec):
//! - **Read-only isolation.** The client constructs a single, stateless HTTP
//!   request containing only the diff plus a review-scoped system prompt. It
//!   never executes tools, never writes files, and never holds state between
//!   calls. The built request payload deliberately contains no `tools`
//!   definition and the system prompt explicitly forbids tool use, so even a
//!   misconfigured key can only "waste API calls," not write files.
//! - **No plaintext keys in this module.** The key is passed in by the caller
//!   (resolved from the OS keyring by `swai_core::keyring`) and is only used
//!   for the duration of a single `audit()` call.
//! - **Testability.** All request-building and response-parsing logic lives in
//!   pure functions and an injectable [`transport::HttpTransport`] trait, so the
//!   client is fully unit-tested against mocked endpoint responses with no
//!   network.
//! - **Failure handling (Phase 35 §6).** A missing/revoked key surfaces as a
//!   distinct [`error::AuditorError::MissingKey`]; an unparseable or empty
//!   response is returned as an *inconclusive* result (never a silent "clean
//!   bill of health").

pub mod client;
pub mod error;
pub mod parse;
pub mod provider;
pub mod request;
pub mod scope;
pub mod transport;

#[cfg(test)]
mod tests;

pub use client::{FrontierAuditor, SYSTEM_PROMPT};
pub use error::AuditorError;
// `extract_json_object` is a parsing helper; re-exported crate-locally so the
// test module (which lives under this crate root) can reach it via `super::*`.
#[cfg(test)]
pub(crate) use parse::extract_json_object;
pub use parse::parse_findings;
pub use provider::FrontierProvider;
pub use request::{build_request_body, extract_response_text};
pub use scope::{AuditResult, AuditScope, ChangedFile, Finding, FindingSeverity};
pub use transport::{HttpTransport, ReqwestTransport};
