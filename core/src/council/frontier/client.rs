//! The read-only frontier review client.

use std::sync::Arc;

use super::error::AuditorError;
use super::parse::parse_findings;
use super::provider::FrontierProvider;
use super::request::{build_request_body, extract_response_text};
use super::scope::{AuditScope, AuditResult};
use super::transport::{HttpTransport, ReqwestTransport};

/// A stateless, read-only frontier-model review client.
///
/// A single call to [`FrontierAuditor::audit`] builds a review-only prompt,
/// POSTs it once to the provider, and parses the structured findings back. The
/// client holds no state between calls and never executes tools or writes
/// files. Construct one with [`FrontierAuditor::new`] (or `with_transport` to
/// inject a mock for testing).
pub struct FrontierAuditor {
    provider: FrontierProvider,
    model: String,
    transport: Arc<dyn HttpTransport>,
}

impl FrontierAuditor {
    /// Create a client for the given provider and model, using the production
    /// `reqwest` transport.
    pub fn new(provider: FrontierProvider, model: impl Into<String>) -> Self {
        Self::with_transport(
            provider,
            model,
            Arc::new(ReqwestTransport::new().expect("failed to build default ReqwestTransport")),
        )
    }

    /// Create a client with an explicitly injected transport (used by tests).
    pub fn with_transport(
        provider: FrontierProvider,
        model: impl Into<String>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            transport,
        }
    }

    /// The provider this client talks to.
    pub fn provider(&self) -> FrontierProvider {
        self.provider
    }

    /// The model id this client uses.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// **Strict read-only guarantee.** Always `true`: the auditor constructs a
    /// single-shot review request with no tool definitions and explicitly
    /// instructs the model not to use tools. It never executes tools, never
    /// touches the filesystem, and never retains state between calls.
    pub fn is_read_only(&self) -> bool {
        true
    }

    /// Build the review-scoped system prompt. It constrains the model to
    /// read-only review of security, correctness, and error-handling issues and
    /// forbids tool use and style-only suggestions.
    pub fn system_prompt(&self) -> String {
        SYSTEM_PROMPT.to_string()
    }

    /// Build the user prompt for a given audit scope (diff / files + constraints).
    pub fn user_prompt(&self, scope: &AuditScope) -> String {
        build_user_prompt(scope)
    }

    /// Run an audit. `key` is the frontier API key resolved from the keyring by
    /// the caller; it is used only for the duration of this call.
    pub fn audit(&self, key: &str, scope: &AuditScope) -> Result<AuditResult, AuditorError> {
        if key.trim().is_empty() {
            return Err(AuditorError::MissingKey);
        }

        let system = self.system_prompt();
        let user = self.user_prompt(scope);

        let (url, headers, body) =
            build_request_body(self.provider, key, &self.model, &system, &user);

        let response = self
            .transport
            .post(&url, &headers, &body)
            .map_err(AuditorError::Transport)?;
        if response.trim().is_empty() {
            return Err(AuditorError::EmptyResponse);
        }

        let text = extract_response_text(self.provider, &response)
            .ok_or_else(|| AuditorError::Parse("could not extract text from response".into()))?;

        Ok(parse_findings(self.provider, &self.model, &text))
    }
}

/// The canonical review-scoped system prompt. Kept as a const so it can be
/// asserted on in tests and reused verbatim by the UI/CLI front ends.
pub const SYSTEM_PROMPT: &str = "\
You are a read-only code auditor reviewing a diff for a software project. \
Your job is to find real problems; you have NO tools and must not attempt to \
use any. Do not edit files, do not run commands, and do not emit tool calls \
or tool-shaped output.

Review ONLY for:
1. Security & permission issues (injection, unsafe file/permission handling, \
exposed secrets, path traversal).
2. Correctness / logic errors (off-by-one, wrong conditions, null/unwrap risk).
3. Error handling gaps (swallowed errors, missing cleanup, resource leaks).

Do NOT suggest style, formatting, or purely subjective changes unless they are \
actual bugs. Be concise and specific.

Output ONLY a JSON object on its own line, in exactly this shape:
{\"findings\": [{\"severity\": \"critical|high|medium|low|info\", \"component\": \"<subsystem>\", \"location\": \"<file:line or function>\", \"description\": \"<one or two sentences>\", \"suggestion\": \"<remediation, optional>\"}]}
If you find no real issues, output {\"findings\": []}.";

/// Build the user prompt that pairs the system prompt with the actual diff.
pub fn build_user_prompt(scope: &AuditScope) -> String {
    let mut out = String::new();
    if !scope.title.trim().is_empty() {
        out.push_str(&format!("Review this change:\n{}\n\n", scope.title));
    }
    out.push_str("For each file below, review the provided diff (or full content) for the issues listed in your instructions.\n\n");

    for file in &scope.files {
        out.push_str(&format!("### FILE: {}\n", file.path));
        match (&file.diff, &file.content) {
            (Some(diff), _) => {
                out.push_str(diff);
            }
            (None, Some(content)) => {
                out.push_str(content);
            }
            (None, None) => {
                out.push_str("(no diff or content provided)");
            }
        }
        out.push('\n');
    }

    out.push_str(
        "Return the JSON object described in your instructions. Do not include \
         anything outside the JSON object.",
    );
    out
}
