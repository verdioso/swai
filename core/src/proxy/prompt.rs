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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_workspace_top_level_cwd() {
        let json = serde_json::json!({
            "cwd": "/mnt/orico/Documents/ApplicationsRAW/swai",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/mnt/orico/Documents/ApplicationsRAW/swai".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_system_string() {
        let json = serde_json::json!({
            "system": "You are Claude Code.\nWorking directory: /var/log\nOther info",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/var/log".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_system_array() {
        let json = serde_json::json!({
            "system": [
                {"type": "text", "text": "Environment context:\nWorking directory: /tmp\n"}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/tmp".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_user_message_xml_tag() {
        let json = serde_json::json!({
            "messages": [
                {"role": "user", "content": "<cwd>/tmp</cwd>\nFix this bug"}
            ]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/tmp".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_user_message_blocks() {
        let json = serde_json::json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Current working directory: /tmp\nProceed"}
                    ]
                }
            ]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/tmp".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_context_object() {
        let json = serde_json::json!({
            "context": {"cwd": "/tmp"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/tmp".into())
        );
    }
}
