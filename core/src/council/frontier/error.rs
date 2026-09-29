//! Audit error types.

/// Errors that can occur during an audit.
#[derive(Debug, thiserror::Error)]
pub enum AuditorError {
    /// The API key was missing or empty. The caller should surface this
    /// clearly rather than silently skipping the audit (Phase 35 §6).
    #[error("auditor API key is missing or empty")]
    MissingKey,
    /// The HTTP transport failed (network error, non-2xx status, etc.).
    #[error("request failed: {0}")]
    Transport(String),
    /// The response body was empty.
    #[error("auditor returned an empty response")]
    EmptyResponse,
    /// The response could not be understood at all (not even to extract text).
    #[error("failed to parse auditor response: {0}")]
    Parse(String),
}
