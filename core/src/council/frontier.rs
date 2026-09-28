//! SWAI — Frontier Auditor client (`swai-core::council::frontier`).
//!
//! Phase 35.2: a stateless, **read-only** review client that sends an
//! externally-configured frontier-model API key (Claude / GPT / Gemini) to a
//! third-party endpoint to review a diff, and parses the structured findings
//! back into SWAI's UI.
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
//!   pure functions and an injectable [`HttpTransport`] trait, so the client is
//!   fully unit-tested against mocked endpoint responses with no network.
//! - **Failure handling (Phase 35 §6).** A missing/revoked key surfaces as a
//!   distinct [`AuditorError::MissingKey`]; an unparseable or empty response is
//!   returned as an *inconclusive* result (never a silent "clean bill of
//!   health").

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Provider metadata
// ---------------------------------------------------------------------------

/// A frontier LLM provider the Auditor can review with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontierProvider {
    /// Anthropic Claude (`anthropic.com`).
    Anthropic,
    /// OpenAI GPT (`api.openai.com`).
    OpenAI,
    /// Google Gemini (`generativelanguage.googleapis.com`).
    Gemini,
}

impl FrontierProvider {
    /// Human-readable label for the UI.
    pub fn display_name(self) -> &'static str {
        match self {
            FrontierProvider::Anthropic => "Anthropic (Claude)",
            FrontierProvider::OpenAI => "OpenAI (GPT)",
            FrontierProvider::Gemini => "Google (Gemini)",
        }
    }

    /// The base endpoint URL. `model` is interpolated for Gemini, which keys
    /// the method off the path.
    pub fn endpoint(self, model: &str) -> String {
        match self {
            FrontierProvider::Anthropic => "https://api.anthropic.com/v1/messages".into(),
            FrontierProvider::OpenAI => "https://api.openai.com/v1/chat/completions".into(),
            FrontierProvider::Gemini => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
            ),
        }
    }

    /// The default model id to use when the caller does not specify one.
    pub fn default_model(self) -> &'static str {
        match self {
            FrontierProvider::Anthropic => "claude-sonnet-5",
            FrontierProvider::OpenAI => "gpt-4o",
            FrontierProvider::Gemini => "gemini-2.5-pro",
        }
    }

    /// Authentication headers for this provider. The API key is substituted in;
    /// it is never persisted or logged by this module.
    pub fn auth_headers(self, key: &str) -> Vec<(String, String)> {
        match self {
            FrontierProvider::Anthropic => vec![
                ("x-api-key".to_string(), key.to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ],
            FrontierProvider::OpenAI => vec![("Authorization".to_string(), format!("Bearer {key}"))],
            FrontierProvider::Gemini => vec![("x-goog-api-key".to_string(), key.to_string())],
        }
    }
}

// ---------------------------------------------------------------------------
// Audit scope + findings
// ---------------------------------------------------------------------------

/// What to review. Carries a human-readable title (commit message, phase name,
/// or manual audit label) plus the files to review.
///
/// Each file may carry either a unified diff OR its full content — the prompt
/// builder prefers the diff when present (Phase 35 §3: "the diff (or full
/// changed files if the diff is small)").
#[derive(Debug, Clone, Default)]
pub struct AuditScope {
    /// A short label for the review (shown to the auditor for context).
    pub title: String,
    /// Files to review.
    pub files: Vec<ChangedFile>,
}

impl AuditScope {
    /// Create an empty scope with the given title.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            files: Vec::new(),
        }
    }

    /// Add a file to review. `diff` is a unified-diff string; `content` is the
    /// full file text. At least one should be set.
    pub fn with_file(mut self, path: impl Into<String>, diff: Option<String>, content: Option<String>) -> Self {
        self.files.push(ChangedFile {
            path: path.into(),
            diff,
            content,
        });
        self
    }
}

/// A single file included in an audit scope.
#[derive(Debug, Clone, Default)]
pub struct ChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// Unified diff hunks, when reviewing a diff.
    pub diff: Option<String>,
    /// Full file content, when reviewing whole files.
    pub content: Option<String>,
}

/// Severity of a single audit finding. Ordered least → most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    /// Trivial / cosmetic.
    Info,
    /// Minor, low impact.
    Low,
    /// Moderate impact.
    Medium,
    /// Serious, should be fixed.
    High,
    /// Critical / security-relevant.
    Critical,
}

impl FindingSeverity {
    /// Parse a free-text severity label from an auditor response. Falls back to
    /// [`FindingSeverity::Medium`] for unrecognized values.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            s if s.contains("critical") || s.contains("blocker") || s.contains("urgent") => {
                FindingSeverity::Critical
            }
            s if s.contains("high") || s.contains("serious") || s.contains("severity: high") => {
                FindingSeverity::High
            }
            s if s.contains("medium") || s.contains("med") || s.contains("moderate") => {
                FindingSeverity::Medium
            }
            s if s.contains("low") => FindingSeverity::Low,
            s if s.contains("info") || s.contains("informational") || s.contains("cosmetic") => {
                FindingSeverity::Info
            }
            _ => FindingSeverity::Medium,
        }
    }

    /// Numeric ordering (higher = more severe), handy for sorting/reporting.
    pub fn rank(self) -> u8 {
        match self {
            FindingSeverity::Info => 0,
            FindingSeverity::Low => 1,
            FindingSeverity::Medium => 2,
            FindingSeverity::High => 3,
            FindingSeverity::Critical => 4,
        }
    }
}

/// A single structured finding produced by an auditor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Severity of the finding.
    pub severity: FindingSeverity,
    /// Component / subsystem the finding relates to (e.g. "network", "parsing").
    pub component: String,
    /// Location hint (file:line, function name, etc.).
    pub location: String,
    /// Human-readable description of the issue.
    pub description: String,
    /// Suggested remediation, if the auditor provided one.
    #[serde(default)]
    pub suggestion: String,
}

/// The complete result of an audit call.
#[derive(Debug, Clone)]
pub struct AuditResult {
    /// Which provider produced this result.
    pub provider: FrontierProvider,
    /// Model id used.
    pub model: String,
    /// Structured findings (empty when `inconclusive`).
    pub findings: Vec<Finding>,
    /// True when the response could not be parsed into findings. An inconclusive
    /// result must be surfaced to the user as "audit inconclusive", NOT as a
    /// clean pass (Phase 35 §6).
    pub inconclusive: bool,
    /// Short excerpt of the raw response for debugging / manual review.
    pub raw_excerpt: String,
    /// Estimated input tokens (prompt size / ~4 tokens/char).
    pub input_tokens: usize,
    /// Estimated output tokens (derived from finding count).
    pub output_tokens: usize,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// HTTP transport abstraction (injectable for tests)
// ---------------------------------------------------------------------------

/// Abstraction over the HTTP POST used to reach a frontier endpoint.
///
/// Keeping this behind a trait lets the client be unit-tested with a mock
/// transport (no network) while the production path uses [`ReqwestTransport`].
pub trait HttpTransport: Send + Sync {
    /// Perform a POST and return the raw response body on success, or an error
    /// string on failure.
    fn post(&self, url: &str, headers: &[(String, String)], body: &str) -> Result<String, String>;
}

/// Production [`HttpTransport`] backed by `reqwest` (blocking).
pub struct ReqwestTransport {
    client: reqwest::blocking::Client,
}

impl ReqwestTransport {
    /// Build a transport with a 60-second request timeout.
    pub fn new() -> Result<Self, AuditorError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| AuditorError::Transport(e.to_string()))?;
        Ok(Self { client })
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new().expect("failed to build default ReqwestTransport")
    }
}

impl HttpTransport for ReqwestTransport {
    fn post(&self, url: &str, headers: &[(String, String)], body: &str) -> Result<String, String> {
        let mut req = self.client.post(url).body(body.to_string());
        for (name, value) in headers {
            req = req.header(name.as_str(), value.as_str());
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            let snippet = &text[..text.len().min(300)];
            return Err(format!("HTTP {status}: {snippet}"));
        }
        Ok(text)
    }
}

// ---------------------------------------------------------------------------
// Read-only review client
// ---------------------------------------------------------------------------

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
        Self::with_transport(provider, model, Arc::new(ReqwestTransport::new().expect(
            "failed to build default ReqwestTransport",
        )))
    }

    /// Create a client with an explicitly injected transport (used by tests).
    pub fn with_transport(provider: FrontierProvider, model: impl Into<String>, transport: Arc<dyn HttpTransport>) -> Self {
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

/// Build the provider-specific request: `(url, headers, body)`.
///
/// This is a pure function so it can be unit-tested without a network.
pub fn build_request_body(
    provider: FrontierProvider,
    key: &str,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> (String, Vec<(String, String)>, String) {
    match provider {
        FrontierProvider::Anthropic => {
            let body = json!({
                "model": model,
                "max_tokens": 4096,
                "system": system_prompt,
                "messages": [{"role": "user", "content": user_prompt}],
            });
            let mut headers = provider.auth_headers(key);
            headers.push(("content-type".to_string(), "application/json".to_string()));
            (provider.endpoint(model), headers, body.to_string())
        }
        FrontierProvider::OpenAI => {
            let body = json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_prompt},
                ],
                "response_format": {"type": "json_object"},
            });
            let mut headers = provider.auth_headers(key);
            headers.push(("content-type".to_string(), "application/json".to_string()));
            (provider.endpoint(model), headers, body.to_string())
        }
        FrontierProvider::Gemini => {
            let body = json!({
                "contents": [{"role": "user", "parts": [{"text": user_prompt}]}],
                "systemInstruction": {"parts": [{"text": system_prompt}]},
                "generationConfig": {"responseMimeType": "application/json"},
            });
            let mut headers = provider.auth_headers(key);
            headers.push(("content-type".to_string(), "application/json".to_string()));
            (provider.endpoint(model), headers, body.to_string())
        }
    }
}

/// Extract the model's textual answer from a provider-specific response body.
/// Returns `None` if no text can be found.
pub fn extract_response_text(provider: FrontierProvider, response: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(response).ok()?;
    match provider {
        FrontierProvider::Anthropic => {
            // {"content": [{"type":"text","text":"..."}]}
            let parts: Vec<String> = value
                .get("content")?
                .as_array()?
                .iter()
                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                .map(str::to_string)
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        FrontierProvider::OpenAI => value
            .get("choices")?
            .as_array()?
            .first()?
            .get("message")?
            .get("content")
            .and_then(|c| c.as_str())
            .map(str::to_string),
        FrontierProvider::Gemini => {
            let parts: Vec<String> = value
                .get("candidates")?
                .as_array()?
                .first()?
                .get("content")?
                .get("parts")?
                .as_array()?
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .map(str::to_string)
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
    }
}

/// Parse an auditor's textual response into structured findings.
///
/// If the response contains a parseable JSON structure with findings, they are
/// returned and `inconclusive` is `false`. Otherwise the result is
/// `inconclusive: true` with an empty findings list (the caller must treat this
/// as "audit inconclusive", never as a pass), and a short raw excerpt is kept.
pub fn parse_findings(provider: FrontierProvider, model: &str, response: &str) -> AuditResult {
    let raw_excerpt = &response[..response.len().min(300)];
    let value = extract_json_object(response);
    let findings: Vec<Finding> = match &value {
        Some(v) => extract_findings_from_value(v),
        None => Vec::new(),
    };
    let inconclusive = findings.is_empty() && !is_empty_findings_array(&value);
    let input_tokens = estimate_tokens(response);
    let output_tokens = if findings.is_empty() {
        0
    } else {
        (findings.len() * 350).min(2000)
    };

    AuditResult {
        provider,
        model: model.to_string(),
        findings,
        inconclusive,
        raw_excerpt: raw_excerpt.to_string(),
        input_tokens,
        output_tokens,
    }
}

/// Whether the parsed JSON is an explicit empty findings array (i.e. the model
/// honestly reported "no issues") vs. a genuine failure to parse.
fn is_empty_findings_array(value: &Option<serde_json::Value>) -> bool {
    match value {
        Some(v) => {
            let empty_arr = v.as_array().map(|a| a.is_empty()).unwrap_or(false);
            let empty_findings = v
                .get("findings")
                .and_then(|f| f.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(false);
            empty_arr || empty_findings
        }
        None => false,
    }
}

/// Pull findings out of a parsed JSON value, tolerating a few shapes:
/// - `{"findings": [...]}`
/// - a bare `[...]` array of findings
/// - a single finding object
/// - a finding object nested inside a string (e.g. Anthropic's
///   `content[].text` wrapping the JSON)
fn extract_findings_from_value(v: &serde_json::Value) -> Vec<Finding> {
    // Direct array of findings.
    if let Some(arr) = v.as_array() {
        return arr.iter().map(finding_from_object).collect();
    }
    // {"findings": [...]}
    if let Some(arr) = v.get("findings").and_then(|f| f.as_array()) {
        return arr.iter().map(finding_from_object).collect();
    }
    // A finding-like object at the top level.
    if is_finding_like(v) {
        return vec![finding_from_object(v)];
    }
    // Recurse into nested objects/arrays.
    let mut out: Vec<Finding> = Vec::new();
    let mut stack: Vec<&serde_json::Value> = vec![v];
    while let Some(node) = stack.pop() {
        if let Some(obj) = node.as_object() {
            for value in obj.values() {
                if is_finding_like(value) {
                    out.push(finding_from_object(value));
                } else {
                    stack.push(value);
                }
            }
        } else if let Some(arr) = node.as_array() {
            for value in arr {
                if is_finding_like(value) {
                    out.push(finding_from_object(value));
                } else {
                    stack.push(value);
                }
            }
        } else if let Some(s) = node.as_str() {
            // The JSON may be embedded as a string (e.g. Anthropic text
            // blocks). Try to parse it out.
            if let Some(inner) = extract_json_object(s) {
                out.extend(extract_findings_from_value(&inner));
            }
        }
    }
    out
}

/// Whether a JSON value looks like a structured finding (has any of the
/// canonical finding fields). Used to distinguish real findings from other
/// objects that may appear in a response.
fn is_finding_like(v: &serde_json::Value) -> bool {
    v.get("description").is_some()
        || v.get("severity").is_some()
        || v.get("component").is_some()
        || v.get("location").is_some()
}

/// Map a JSON object to a `Finding`, filling defaults for missing fields.
fn finding_from_object(v: &serde_json::Value) -> Finding {
    let field = |key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Finding {
        severity: FindingSeverity::parse(&field("severity")),
        component: non_empty_or_unknown(&field("component")),
        location: non_empty_or_unknown(&field("location")),
        description: non_empty_or_unknown(&field("description")),
        suggestion: field("suggestion"),
    }
}

/// Replace an empty/whitespace field with a sensible default.
fn non_empty_or_unknown(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        "unknown".to_string()
    } else {
        t.to_string()
    }
}

/// Estimate token count from a string (roughly 4 chars/token).
fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() + 3) / 4
}

/// Extract a JSON object or array from a response string, tolerating markdown
/// code fences and surrounding prose. Returns `None` if no JSON is found.
fn extract_json_object(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();

    // Fast path: the whole string is valid JSON.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(v);
    }

    // Otherwise, find the first `{` or `[` and extend to its matching close
    // bracket, skipping over string contents so brackets inside strings don't
    // confuse the matcher.
    let bytes = trimmed.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{' || b == b'[')?;
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                let slice = &trimmed[start..=start + i];
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(slice) {
                    return Some(v);
                }
                break;
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A transport that records the request and returns a canned response.
    struct MockTransport {
        request: Arc<Mutex<Option<(String, Vec<(String, String)>, String)>>>,
        response: Arc<Mutex<String>>,
        error: Arc<Mutex<Option<String>>>,
    }

    impl MockTransport {
        fn new(response: &str) -> Self {
            Self {
                request: Arc::new(Mutex::new(None)),
                response: Arc::new(Mutex::new(response.to_string())),
                error: Arc::new(Mutex::new(None)),
            }
        }
        fn with_error(err: &str) -> Self {
            Self {
                request: Arc::new(Mutex::new(None)),
                response: Arc::new(Mutex::new(String::new())),
                error: Arc::new(Mutex::new(Some(err.to_string()))),
            }
        }
        fn last_request(&self) -> Option<(String, Vec<(String, String)>, String)> {
            self.request.lock().unwrap().clone()
        }
    }

    impl HttpTransport for MockTransport {
        fn post(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &str,
        ) -> Result<String, String> {
            *self.request.lock().unwrap() =
                Some((url.to_string(), headers.to_vec(), body.to_string()));
            if let Some(err) = self.error.lock().unwrap().clone() {
                return Err(err);
            }
            Ok(self.response.lock().unwrap().clone())
        }
    }

    fn sample_scope() -> AuditScope {
        AuditScope::new("Add network retry")
            .with_file(
                "src/net.rs",
                Some("@@ -1,3 +1,4 @@\n+let x = 1\n".to_string()),
                None,
            )
            .with_file("src/main.rs", None, Some("fn main() {}\n".to_string()))
    }

    /// A canned Anthropic-style response with two findings.
    fn anthropic_response() -> String {
        serde_json::json!({
            "content": [{"type": "text", "text": r#"{
                "findings": [
                    {"severity": "high", "component": "network", "location": "src/net.rs:42", "description": "Connection error is ignored.", "suggestion": "Propagate the error."},
                    {"severity": "info", "component": "main", "location": "src/main.rs:1", "description": "Consider adding a doc comment."}
                ]
            }"#.to_string()}]
        })
        .to_string()
    }

    #[test]
    fn test_provider_metadata() {
        assert_eq!(
            FrontierProvider::Anthropic.endpoint("claude-sonnet-5"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            FrontierProvider::OpenAI.endpoint("gpt-4o"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            FrontierProvider::Gemini.endpoint("gemini-2.5-pro"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
        assert_eq!(FrontierProvider::Anthropic.default_model(), "claude-sonnet-5");
        assert_eq!(FrontierProvider::OpenAI.default_model(), "gpt-4o");
        assert_eq!(FrontierProvider::Gemini.default_model(), "gemini-2.5-pro");

        let anthropic_headers = FrontierProvider::Anthropic.auth_headers("sk-abc");
        assert_eq!(anthropic_headers[0], ("x-api-key".to_string(), "sk-abc".to_string()));

        let openai_headers = FrontierProvider::OpenAI.auth_headers("sk-abc");
        assert_eq!(
            openai_headers[0],
            ("Authorization".to_string(), "Bearer sk-abc".to_string())
        );

        let gemini_headers = FrontierProvider::Gemini.auth_headers("key123");
        assert_eq!(
            gemini_headers[0],
            ("x-goog-api-key".to_string(), "key123".to_string())
        );
    }

    #[test]
    fn test_severity_parsing() {
        assert_eq!(FindingSeverity::parse("critical"), FindingSeverity::Critical);
        assert_eq!(FindingSeverity::parse("BLOCKER"), FindingSeverity::Critical);
        assert_eq!(FindingSeverity::parse("high"), FindingSeverity::High);
        assert_eq!(FindingSeverity::parse("medium"), FindingSeverity::Medium);
        assert_eq!(FindingSeverity::parse("low"), FindingSeverity::Low);
        assert_eq!(FindingSeverity::parse("info"), FindingSeverity::Info);
        assert_eq!(FindingSeverity::parse("nonsense"), FindingSeverity::Medium);
        // Ordering check.
        assert!(FindingSeverity::Critical.rank() > FindingSeverity::Info.rank());
    }

    #[test]
    fn test_system_prompt_is_read_only_and_forbids_tools() {
        let auditor = FrontierAuditor::new(FrontierProvider::Anthropic, "claude-sonnet-5");
        let sp = auditor.system_prompt();
        assert!(sp.to_lowercase().contains("read-only"));
        assert!(sp.to_lowercase().contains("no tools"));
        assert!(sp.to_lowercase().contains("do not emit tool calls"));
        // Read-only guarantee is structural.
        assert!(auditor.is_read_only());
    }

    #[test]
    fn test_user_prompt_includes_scope_and_constraints() {
        let auditor = FrontierAuditor::new(FrontierProvider::OpenAI, "gpt-4o");
        let up = auditor.user_prompt(&sample_scope());
        assert!(up.contains("src/net.rs"));
        assert!(up.contains("src/main.rs"));
        assert!(up.contains("Add network retry"));
    }

    #[test]
    fn test_build_request_body_anthropic() {
        let (url, headers, body) = build_request_body(
            FrontierProvider::Anthropic,
            "sk-key",
            "claude-sonnet-5",
            "SYS",
            "USER",
        );
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert!(headers.iter().any(|(k, v)| k == "x-api-key" && v == "sk-key"));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "claude-sonnet-5");
        assert_eq!(v["system"], "SYS");
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"], "USER");
        // Read-only: no tool definitions in the request.
        assert!(v.get("tools").is_none());
    }

    #[test]
    fn test_build_request_body_openai() {
        let (url, headers, body) = build_request_body(
            FrontierProvider::OpenAI,
            "sk-key",
            "gpt-4o",
            "SYS",
            "USER",
        );
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert!(headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer sk-key"));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][1]["role"], "user");
        assert!(v.get("tools").is_none());
    }

    #[test]
    fn test_build_request_body_gemini() {
        let (url, headers, body) = build_request_body(
            FrontierProvider::Gemini,
            "gkey",
            "gemini-2.5-pro",
            "SYS",
            "USER",
        );
        assert!(url.contains("gemini-2.5-pro:generateContent"));
        assert!(headers.iter().any(|(k, v)| k == "x-goog-api-key" && v == "gkey"));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("systemInstruction").is_some());
        // Gemini has no top-level tools field either.
        assert!(v.get("tools").is_none());
    }

    #[test]
    fn test_extract_response_text_per_provider() {
        let anth = serde_json::json!({"content":[{"type":"text","text":"hello"}]}).to_string();
        assert_eq!(extract_response_text(FrontierProvider::Anthropic, &anth).unwrap(), "hello");

        let oai = serde_json::json!({"choices":[{"message":{"content":"world"}}]}).to_string();
        assert_eq!(extract_response_text(FrontierProvider::OpenAI, &oai).unwrap(), "world");

        let gem = serde_json::json!({"candidates": [{"content": {"parts": [{"text": "moon"}]}}]}).to_string();
        assert_eq!(extract_response_text(FrontierProvider::Gemini, &gem).unwrap(), "moon");

        // Missing text -> None.
        let empty = serde_json::json!({"content":[]}).to_string();
        assert!(extract_response_text(FrontierProvider::Anthropic, &empty).is_none());
    }

    #[test]
    fn test_parse_findings_from_anthropic_response() {
        let result = parse_findings(FrontierProvider::Anthropic, "claude-sonnet-5", &anthropic_response());
        assert!(!result.inconclusive);
        assert_eq!(result.findings.len(), 2);
        assert_eq!(result.findings[0].severity, FindingSeverity::High);
        assert_eq!(result.findings[0].component, "network");
        assert_eq!(result.findings[0].location, "src/net.rs:42");
        assert!(result.findings[0].description.contains("Connection error"));
        assert_eq!(result.findings[1].severity, FindingSeverity::Info);
    }

    #[test]
    fn test_parse_findings_tolerates_code_fences() {
        let fenced = "Sure! Here is the review:\n```json\n{\"findings\":[{\"severity\":\"critical\",\"component\":\"auth\",\"location\":\"login.rs:10\",\"description\":\"SQL injection possible\"}]}\n```";
        let result = parse_findings(FrontierProvider::OpenAI, "gpt-4o", fenced);
        assert!(!result.inconclusive);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, FindingSeverity::Critical);
    }

    #[test]
    fn test_parse_findings_inconclusive_when_no_json() {
        let result = parse_findings(FrontierProvider::Gemini, "gemini-2.5-pro", "I found nothing worth reporting.");
        assert!(result.inconclusive);
        assert!(result.findings.is_empty());
        // Excerpt retained for debugging.
        assert!(result.raw_excerpt.contains("worth reporting"));
    }

    #[test]
    fn test_parse_findings_empty_findings_array_is_not_inconclusive() {
        // A model honestly reporting no issues should NOT be "inconclusive".
        let result = parse_findings(FrontierProvider::Anthropic, "claude", r#"{"findings": []}"#);
        assert!(!result.inconclusive);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn test_parse_findings_defaults_missing_severity() {
        let resp = r#"{"findings":[{"component":"net","description":"weird thing"}]}"#;
        let result = parse_findings(FrontierProvider::OpenAI, "gpt", resp);
        assert_eq!(result.findings[0].severity, FindingSeverity::Medium);
        assert_eq!(result.findings[0].location, "unknown");
    }

    #[test]
    fn test_audit_missing_key_errors() {
        let t = Arc::new(MockTransport::new("{}"));
        let auditor = FrontierAuditor::with_transport(FrontierProvider::Anthropic, "claude", t.clone());
        let err = auditor.audit("   ", &sample_scope()).unwrap_err();
        assert!(matches!(err, AuditorError::MissingKey));
        // No request should have been made.
        assert!(t.last_request().is_none());
    }

    #[test]
    fn test_audit_transport_error_propagates() {
        let t = Arc::new(MockTransport::with_error("connection refused"));
        let auditor = FrontierAuditor::with_transport(FrontierProvider::OpenAI, "gpt", t.clone());
        let err = auditor.audit("sk-key", &sample_scope()).unwrap_err();
        assert!(matches!(err, AuditorError::Transport(_)));
        assert!(t.last_request().is_some());
    }

    #[test]
    fn test_audit_end_to_end_parses_findings() {
        let t = Arc::new(MockTransport::new(&anthropic_response()));
        let auditor = FrontierAuditor::with_transport(FrontierProvider::Anthropic, "claude-sonnet-5", t.clone());
        let result = auditor.audit("sk-key", &sample_scope()).unwrap();
        assert!(!result.inconclusive);
        assert_eq!(result.findings.len(), 2);
        // The transport received a request with the API key header.
        let (_url, headers, _body) = t.last_request().unwrap();
        assert!(headers.iter().any(|(k, v)| k == "x-api-key" && v == "sk-key"));
    }

    #[test]
    fn test_audit_empty_response_is_error() {
        let t = Arc::new(MockTransport::new("   "));
        let auditor = FrontierAuditor::with_transport(FrontierProvider::Gemini, "gemini", t.clone());
        let err = auditor.audit("gkey", &sample_scope()).unwrap_err();
        assert!(matches!(err, AuditorError::EmptyResponse));
    }

    #[test]
    fn test_read_only_request_has_no_tools_field() {
        // Across all providers, the built request must not carry tool definitions.
        for provider in [
            FrontierProvider::Anthropic,
            FrontierProvider::OpenAI,
            FrontierProvider::Gemini,
        ] {
            let (_url, _headers, body) =
                build_request_body(provider, "key", provider.default_model(), "SYS", "USER");
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert!(v.get("tools").is_none(), "{provider:?} request must not define tools");
        }
    }

    #[test]
    fn test_extract_json_object_finds_json_in_prose() {
        let text = "Here's my answer: {\"findings\": [{\"severity\": \"low\", \"description\": \"x\"}]} done.";
        let v = extract_json_object(text).unwrap();
        assert_eq!(v["findings"][0]["severity"], "low");
    }
}
