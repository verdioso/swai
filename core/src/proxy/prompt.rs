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
pub fn extract_prompt_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let messages = json_val.get("messages").and_then(|m| m.as_array())?;
    if messages.is_empty() {
        return None;
    }

    if !has_tool_activity(messages) {
        for msg in messages.iter().rev() {
            if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                if let Some(text) = extract_message_text(msg) {
                    return Some(text);
                }
            }
        }
        return None;
    }

    // Active agentic loop: preserve original task goal and all tool calls / results.
    let initial_goal = messages
        .iter()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(extract_message_text)
        .unwrap_or_else(|| "Complete the assigned development task.".into());

    let mut history = String::new();
    let mut skipped_initial = false;

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "user" && !skipped_initial {
            skipped_initial = true;
            continue;
        }

        if role == "assistant" {
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|tc| tc.as_array()) {
                for tc in tool_calls {
                    let fn_name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .unwrap_or("");
                    history.push_str(&format!("[Tool Call: {}({})]\n", fn_name, args));
                }
            } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                for b in arr {
                    if b.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                        let input = b.get("input").map(|i| i.to_string()).unwrap_or_default();
                        history.push_str(&format!("[Tool Call: {}({})]\n", name, input));
                    }
                }
            }
        } else if role == "tool" || role == "function" {
            let content = match msg.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            history.push_str(&format!(
                "[Tool Result]:\n{}\n\n",
                truncate_tool_str(&content, 16000)
            ));
        } else if role == "user" {
            if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                for b in arr {
                    if b.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        let content = match b.get("content") {
                            Some(serde_json::Value::String(s)) => s.clone(),
                            Some(serde_json::Value::Array(sub_arr)) => {
                                let mut text_buf = String::new();
                                for item in sub_arr {
                                    if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                                        text_buf.push_str(t);
                                        text_buf.push('\n');
                                    }
                                }
                                text_buf
                            }
                            Some(v) => v.to_string(),
                            None => String::new(),
                        };
                        history.push_str(&format!(
                            "[Tool Result]:\n{}\n\n",
                            truncate_tool_str(&content, 16000)
                        ));
                    } else if let Some(text) = b.get("text").and_then(|t| t.as_str()) {
                        history.push_str(&format!("[User]: {}\n", text.trim()));
                    }
                }
            } else if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                history.push_str(&format!("[User]: {}\n", s.trim()));
            }
        }
    }

    if history.trim().is_empty() {
        Some(initial_goal)
    } else {
        Some(format!(
            "Task Goal:\n{}\n\nExecution History & Tool Results:\n{}\nInstructions:\nProceed with the next atomic step required to complete the task goal.",
            initial_goal.trim(),
            history.trim()
        ))
    }
}
