//! SWAI — Prompt and multi-turn agentic context extraction for proxy requests.

fn extract_message_text(msg: &serde_json::Value) -> Option<String> {
    if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
        let t = s.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
        let mut out = String::new();
        for block in arr {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                out.push_str(text);
                out.push('\n');
            } else if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn has_tool_activity(messages: &[serde_json::Value]) -> bool {
    messages.iter().any(|msg| {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "tool" || role == "function" {
            return true;
        }
        if msg
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map_or(false, |a| !a.is_empty())
        {
            return true;
        }
        if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
            return arr.iter().any(|b| {
                let ty = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                ty == "tool_result" || ty == "tool_use"
            });
        }
        false
    })
}

fn truncate_tool_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}\n[... content truncated for brevity ...]", truncated)
    }
}

pub fn extract_system_prompt_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let messages = json_val.get("messages").and_then(|m| m.as_array())?;
    
    // First try to find a message with role == "system"
    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
            if let Some(text) = extract_message_text(msg) {
                return Some(text);
            }
        }
    }
    
    // Fallback: Claude/Anthropic APIs sometimes place the system prompt at the top level
    if let Some(sys) = json_val.get("system") {
        if let Some(s) = sys.as_str() {
            return Some(s.to_string());
        }
        if let Some(arr) = sys.as_array() {
            let mut out = String::new();
            for block in arr {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    out.push_str(text);
                    out.push('\n');
                }
            }
            let trimmed = out.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

/// Extract the user's prompt or multi-turn agentic context from a chat completions JSON body.
/// Extract the last user message prompt from a chat completions JSON body.
pub fn extract_prompt_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let messages = json_val.get("messages").and_then(|m| m.as_array())?;
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
            if let Some(text) = extract_message_text(msg) {
                return Some(text);
            }
        }
    }
    None
}

pub fn extract_workspace_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let messages = json_val.get("messages").and_then(|m| m.as_array())?;
    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
            if let Some(text) = super::prompt::extract_message_text(msg) {
                if let Some(idx) = text.find("Working directory: ") {
                    let start = idx + "Working directory: ".len();
                    let end = text[start..].find('\n').map(|i| start + i).unwrap_or(text.len());
                    return Some(text[start..end].trim().to_string());
                }
            }
        }
    }
    None
}
