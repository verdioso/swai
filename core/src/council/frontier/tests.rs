//! Unit tests for the frontier auditor client.

use super::*;
use std::sync::{Arc, Mutex};

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
