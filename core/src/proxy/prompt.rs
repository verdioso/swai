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

/// Extract the last user message prompt from a chat completions JSON body.
fn extract_tool_result_text(block: &serde_json::Value) -> String {
    if let Some(s) = block.get("content").and_then(|c| c.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = block.get("content").and_then(|c| c.as_array()) {
        let mut out = String::new();
        for item in arr {
            if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                out.push_str(t);
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    block.get("text").and_then(|t| t.as_str()).unwrap_or("(success)").to_string()
}

pub fn clean_user_prompt(prompt: &str) -> String {
    let mut s = prompt.trim();
    while let Some(start) = s.find("<system-reminder>") {
        if let Some(end) = s[start..].find("</system-reminder>") {
            let after = &s[start + end + 18..];
            s = after.trim();
        } else {
            break;
        }
    }
    s.to_string()
}

/// Extract the user prompt and execution history from chat completions JSON body.
pub fn extract_prompt_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let messages = json_val.get("messages").and_then(|m| m.as_array())?;

    let mut tool_history = Vec::new();
    let mut initial_user_prompt = None;

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or_default();
        if role == "user" {
            if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                let mut has_tool_result = false;
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        has_tool_result = true;
                        let output = extract_tool_result_text(block);
                        let capped = if output.len() > 4096 { &output[..4096] } else { &output };
                        tool_history.push(format!("Result:\n{}\n---", capped));
                    }
                }
                if !has_tool_result && initial_user_prompt.is_none() {
                    if let Some(text) = extract_message_text(msg) {
                        let cleaned = clean_user_prompt(&text);
                        if !cleaned.is_empty() {
                            initial_user_prompt = Some(cleaned);
                        }
                    }
                }
            } else if initial_user_prompt.is_none() {
                if let Some(text) = extract_message_text(msg) {
                    let cleaned = clean_user_prompt(&text);
                    if !cleaned.is_empty() {
                        initial_user_prompt = Some(cleaned);
                    }
                }
            }
        } else if role == "assistant" {
            if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                        let input = block.get("input").unwrap_or(&serde_json::Value::Null);
                        let target = input.get("path")
                            .or_else(|| input.get("file_path"))
                            .or_else(|| input.get("target"))
                            .or_else(|| input.get("file"))
                            .or_else(|| input.get("command"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        tool_history.push(format!("Tool: {} | Target: {}", name, target));
                    }
                }
            }
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|tc| tc.as_array()) {
                for tc in tool_calls {
                    let func = tc.get("function");
                    let name = func.and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("tool");
                    let args_str = func.and_then(|f| f.get("arguments")).and_then(|a| a.as_str()).unwrap_or("");
                    let target = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args_str) {
                        parsed.get("path")
                            .or_else(|| parsed.get("file_path"))
                            .or_else(|| parsed.get("target"))
                            .or_else(|| parsed.get("file"))
                            .or_else(|| parsed.get("command"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    } else {
                        String::new()
                    };
                    tool_history.push(format!("Tool: {} | Target: {}", name, target));
                }
            }
        } else if role == "tool" {
            let output = msg.get("content").and_then(|c| c.as_str()).unwrap_or("(success)");
            let capped = if output.len() > 4096 { &output[..4096] } else { output };
            tool_history.push(format!("Result:\n{}\n---", capped));
        }
    }

    if let Some(base) = initial_user_prompt {
        if !tool_history.is_empty() {
            let history_str = tool_history.join("\n");
            return Some(format!(
                "{}\n\nExecution History & Tool Results:\n---\n{}",
                base, history_str
            ));
        }
        return Some(base);
    }

    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
            if let Some(text) = extract_message_text(msg) {
                let cleaned = clean_user_prompt(&text);
                if !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        }
    }
    None
}

pub use super::workspace::{extract_workspace_from_body, extract_workspace_from_str};


