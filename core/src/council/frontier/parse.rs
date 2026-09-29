//! Response parsing into structured findings.

use super::provider::FrontierProvider;
use super::scope::{AuditResult, Finding, FindingSeverity};

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
pub(crate) fn extract_json_object(text: &str) -> Option<serde_json::Value> {
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
