//! SWAI — Prompt and multi-turn agentic context extraction for proxy requests.

use std::path::Path;

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
pub fn extract_prompt_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let messages = json_val.get("messages").and_then(|m| m.as_array())?;

    // Check if there are tool results or tool calls in history
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
                        let output = block
                            .get("content")
                            .and_then(|c| c.as_str())
                            .or_else(|| block.get("text").and_then(|t| t.as_str()))
                            .unwrap_or("(success)");
                        let capped = if output.len() > 4096 { &output[..4096] } else { output };
                        tool_history.push(format!("Result:\n{}\n---", capped));
                    }
                }
                if !has_tool_result && initial_user_prompt.is_none() {
                    if let Some(text) = extract_message_text(msg) {
                        initial_user_prompt = Some(text);
                    }
                }
            } else if initial_user_prompt.is_none() {
                if let Some(text) = extract_message_text(msg) {
                    initial_user_prompt = Some(text);
                }
            }
        } else if role == "assistant" {
            if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                        let input = block.get("input").unwrap_or(&serde_json::Value::Null);
                        let target = input.get("path")
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

    if !tool_history.is_empty() && initial_user_prompt.is_some() {
        let base = initial_user_prompt.unwrap();
        let history_str = tool_history.join("\n");
        return Some(format!(
            "{}\n\nExecution History & Tool Results:\n---\n{}",
            base, history_str
        ));
    }

    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
            if let Some(text) = extract_message_text(msg) {
                return Some(text);
            }
        }
    }
    None
}


fn is_plausible_abs_path(s: &str) -> bool {
    if s.starts_with('/') {
        return true;
    }
    if s.len() >= 3 && s.as_bytes()[1] == b':' {
        let b = s.as_bytes()[2];
        if b == b'/' || b == b'\\' {
            return true;
        }
    }
    false
}

fn clean_path_candidate(raw: &str) -> &str {
    raw.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim_matches('\r')
        .trim_end_matches([',', ';', '.'])
        .trim()
}

pub fn extract_workspace_from_str(s: &str) -> Option<String> {
    let mut fallback = None;

    // 1. Tag-based matches: <cwd>...</cwd>, <working_directory>...</working_directory>
    for (open_tag, close_tag) in &[
        ("<cwd>", "</cwd>"),
        ("<working_directory>", "</working_directory>"),
        ("<current_dir>", "</current_dir>"),
        ("<directory>", "</directory>"),
    ] {
        if let Some(start_idx) = s.find(open_tag) {
            let start = start_idx + open_tag.len();
            if let Some(end_idx) = s[start..].find(close_tag) {
                let candidate = clean_path_candidate(&s[start..start + end_idx]);
                if !candidate.is_empty() {
                    if Path::new(candidate).is_dir() {
                        return Some(candidate.to_string());
                    }
                    if fallback.is_none() && is_plausible_abs_path(candidate) {
                        fallback = Some(candidate.to_string());
                    }
                }
            }
        }
    }

    // 2. Prefix-based line matches (Claude Code env block, prompt header, etc.)
    let prefixes = [
        "Working directory: ",
        "working directory: ",
        "Working Directory: ",
        "Current working directory: ",
        "current working directory: ",
        "Current directory: ",
        "current directory: ",
        "Workspace: ",
        "workspace: ",
        "cwd: ",
        "CWD: ",
    ];

    for prefix in &prefixes {
        let mut search_from = 0;
        while let Some(idx) = s[search_from..].find(prefix) {
            let start = search_from + idx + prefix.len();
            let end = s[start..].find('\n').map(|i| start + i).unwrap_or(s.len());
            let candidate = clean_path_candidate(&s[start..end]);
            if !candidate.is_empty() {
                if Path::new(candidate).is_dir() {
                    return Some(candidate.to_string());
                }
                if fallback.is_none() && is_plausible_abs_path(candidate) {
                    fallback = Some(candidate.to_string());
                }
            }
            search_from = start;
            if search_from >= s.len() {
                break;
            }
        }
    }

    // 3. Embedded JSON key pattern: "cwd": "..." or "working_directory": "..."
    for key in &["\"cwd\"", "\"working_directory\"", "\"workspace\""] {
        if let Some(idx) = s.find(key) {
            let after = &s[idx + key.len()..];
            if let Some(colon_idx) = after.find(':') {
                let after_colon = after[colon_idx + 1..].trim_start();
                if let Some(stripped) = after_colon.strip_prefix('"') {
                    if let Some(quote_idx) = stripped.find('"') {
                        let candidate = clean_path_candidate(&stripped[..quote_idx]);
                        if !candidate.is_empty() {
                            if Path::new(candidate).is_dir() {
                                return Some(candidate.to_string());
                            }
                            if fallback.is_none() && is_plausible_abs_path(candidate) {
                                fallback = Some(candidate.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    fallback
}

pub fn extract_workspace_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let mut fallback = None;

    let dir_keys = [
        "cwd",
        "working_directory",
        "workingDirectory",
        "workspace",
        "workspace_root",
        "workspaceRoot",
        "project_dir",
        "projectDir",
        "project_path",
        "projectPath",
    ];

    // 1. Direct top-level fields
    for key in &dir_keys {
        if let Some(dir) = json_val.get(*key).and_then(|v| v.as_str()) {
            let candidate = clean_path_candidate(dir);
            if !candidate.is_empty() {
                if Path::new(candidate).is_dir() {
                    return Some(candidate.to_string());
                }
                if fallback.is_none() && is_plausible_abs_path(candidate) {
                    fallback = Some(candidate.to_string());
                }
            }
        }
    }

    // 2. Nested objects at root: context, metadata, environment
    for obj_key in &["context", "metadata", "environment"] {
        if let Some(obj) = json_val.get(*obj_key).and_then(|v| v.as_object()) {
            for key in &dir_keys {
                if let Some(dir) = obj.get(*key).and_then(|v| v.as_str()) {
                    let candidate = clean_path_candidate(dir);
                    if !candidate.is_empty() {
                        if Path::new(candidate).is_dir() {
                            return Some(candidate.to_string());
                        }
                        if fallback.is_none() && is_plausible_abs_path(candidate) {
                            fallback = Some(candidate.to_string());
                        }
                    }
                }
            }
        }
    }

    // 3. Root "system" property (Claude/Anthropic APIs)
    if let Some(sys) = json_val.get("system") {
        if let Some(s) = sys.as_str() {
            if let Some(ws) = extract_workspace_from_str(s) {
                if Path::new(&ws).is_dir() {
                    return Some(ws);
                }
                if fallback.is_none() {
                    fallback = Some(ws);
                }
            }
        }
        if let Some(arr) = sys.as_array() {
            for block in arr {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if let Some(ws) = extract_workspace_from_str(text) {
                        if Path::new(&ws).is_dir() {
                            return Some(ws);
                        }
                        if fallback.is_none() {
                            fallback = Some(ws);
                        }
                    }
                }
            }
        }
    }

    // 4. Messages array (both system and user/assistant messages)
    if let Some(messages) = json_val.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            for key in &dir_keys {
                if let Some(dir) = msg.get(*key).and_then(|v| v.as_str()) {
                    let candidate = clean_path_candidate(dir);
                    if !candidate.is_empty() {
                        if Path::new(candidate).is_dir() {
                            return Some(candidate.to_string());
                        }
                        if fallback.is_none() && is_plausible_abs_path(candidate) {
                            fallback = Some(candidate.to_string());
                        }
                    }
                }
            }
            if let Some(ctx) = msg.get("context").and_then(|c| c.as_object()) {
                for key in &dir_keys {
                    if let Some(dir) = ctx.get(*key).and_then(|v| v.as_str()) {
                        let candidate = clean_path_candidate(dir);
                        if !candidate.is_empty() {
                            if Path::new(candidate).is_dir() {
                                return Some(candidate.to_string());
                            }
                            if fallback.is_none() && is_plausible_abs_path(candidate) {
                                fallback = Some(candidate.to_string());
                            }
                        }
                    }
                }
            }

            if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                if let Some(ws) = extract_workspace_from_str(s) {
                    if Path::new(&ws).is_dir() {
                        return Some(ws);
                    }
                    if fallback.is_none() {
                        fallback = Some(ws);
                    }
                }
            } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                for block in arr {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        if let Some(ws) = extract_workspace_from_str(text) {
                            if Path::new(&ws).is_dir() {
                                return Some(ws);
                            }
                            if fallback.is_none() {
                                fallback = Some(ws);
                            }
                        }
                    }
                    if let Some(ctx) = block.get("context").and_then(|c| c.as_object()) {
                        for key in &dir_keys {
                            if let Some(dir) = ctx.get(*key).and_then(|v| v.as_str()) {
                                let candidate = clean_path_candidate(dir);
                                if !candidate.is_empty() {
                                    if Path::new(candidate).is_dir() {
                                        return Some(candidate.to_string());
                                    }
                                    if fallback.is_none() && is_plausible_abs_path(candidate) {
                                        fallback = Some(candidate.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fallback
}

