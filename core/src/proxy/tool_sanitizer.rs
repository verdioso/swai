//! SWAI — Tool call argument sanitization and multi-file dump prevention.

use serde_json::Value;

/// Strip outer markdown code fences from a JSON or text string.
pub fn strip_markdown_fences(s: &str) -> &str {
    let t = s.trim();
    if t.starts_with("```") {
        if let Some(first_nl) = t.find('\n') {
            let rest = &t[first_nl + 1..];
            if let Some(end) = rest.rfind("```") {
                return rest[..end].trim();
            }
        }
    }
    t
}

/// Check if a string looks like a relative file path or file name with extension.
pub fn is_likely_filepath(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.contains(' ') || s.len() < 3 {
        return false;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return false;
    }
    let has_slash = s.contains('/') || s.contains('\\');
    let has_ext = s.ends_with(".rs")
        || s.ends_with(".toml")
        || s.ends_with(".json")
        || s.ends_with(".html")
        || s.ends_with(".js")
        || s.ends_with(".ts")
        || s.ends_with(".py")
        || s.ends_with(".css")
        || s.ends_with(".md")
        || s.ends_with(".yaml")
        || s.ends_with(".yml");

    (has_slash && (has_ext || s.starts_with("src/") || s.starts_with("core/"))) || has_ext
}

/// Extract a file path candidate from a header comment line.
pub fn extract_file_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let candidate = if let Some(rest) = trimmed.strip_prefix("### File:") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("## File:") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("// File:") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("//") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix('#') {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("/*").and_then(|r| r.strip_suffix("*/")) {
        rest.trim()
    } else {
        return None;
    };

    let clean = candidate.trim_matches(|c: char| c == '`' || c == '*' || c == '"' || c == '\'');
    if is_likely_filepath(clean) {
        Some(clean.to_string())
    } else {
        None
    }
}

/// Find the byte offset of a secondary file delimiter in bundled content.
pub fn find_second_file_delimiter(content: &str) -> Option<usize> {
    let mut byte_offset = 0;
    let mut has_seen_content = false;
    let mut is_first_line = true;

    for line in content.split_inclusive('\n') {
        let trimmed = line.trim();

        if is_first_line {
            is_first_line = false;
            if extract_file_header(trimmed).is_some() || trimmed.is_empty() {
                byte_offset += line.len();
                continue;
            }
        }

        if !trimmed.is_empty() && extract_file_header(trimmed).is_none() {
            has_seen_content = true;
        }

        if has_seen_content {
            if trimmed == "rust" || trimmed == "sh" || trimmed == "bash" || trimmed == "python" || trimmed == "toml" {
                return Some(byte_offset);
            }
            if trimmed.starts_with("```rust")
                || trimmed.starts_with("```bash")
                || trimmed.starts_with("```sh")
                || trimmed.starts_with("```toml")
            {
                return Some(byte_offset);
            }
            if trimmed.starts_with("Run the tests")
                || trimmed.starts_with("To run the tests")
                || trimmed.starts_with("Run cargo test")
            {
                return Some(byte_offset);
            }
            if trimmed.starts_with("## Step ")
                || trimmed.starts_with("### Step ")
                || trimmed.starts_with("# Step 2")
                || trimmed.starts_with("Step 2")
                || trimmed.starts_with("Step 3")
                || trimmed.starts_with("## 2.")
                || trimmed.starts_with("### 2.")
            {
                return Some(byte_offset);
            }
            if extract_file_header(trimmed).is_some() {
                return Some(byte_offset);
            }
        }

        byte_offset += line.len();
    }

    None
}

/// Extract the targeted code block from text matching the target file's extension.
pub fn extract_targeted_code_block<'a>(text: &'a str, target_filename: &str) -> Option<&'a str> {
    let ext = target_filename.split('.').last()?.to_ascii_lowercase();
    let lang_tags: &[&str] = match ext.as_str() {
        "toml" => &["toml"],
        "rs" => &["rust", "rs"],
        "py" => &["python", "py"],
        "json" => &["json"],
        "js" => &["javascript", "js"],
        "ts" => &["typescript", "ts"],
        "html" => &["html"],
        "css" => &["css"],
        "sh" | "bash" => &["bash", "sh"],
        _ => &[],
    };

    for tag in lang_tags {
        let pattern = format!("```{}", tag);
        if let Some(start) = text.find(&pattern) {
            let after = &text[start + pattern.len()..];
            if let Some(first_nl) = after.find('\n') {
                let body = &after[first_nl + 1..];
                if let Some(end) = body.find("```") {
                    let code = body[..end].trim();
                    if !code.is_empty() {
                        return Some(code);
                    }
                }
            }
        }

        let bare_pattern = format!("\n{}\n", tag);
        if let Some(start) = text.find(&bare_pattern) {
            let body = &text[start + bare_pattern.len()..];
            if let Some(delimiter_idx) = find_second_file_delimiter(body) {
                let code = body[..delimiter_idx].trim();
                if !code.is_empty() {
                    return Some(code);
                }
            } else {
                let code = body.trim();
                if !code.is_empty() {
                    return Some(code);
                }
            }
        }
    }
    None
}

/// Strip outer code fences from content body.
pub fn strip_outer_code_fences(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.starts_with("```") {
        if let Some(first_nl) = trimmed.find('\n') {
            let after_fence = &trimmed[first_nl + 1..];
            if let Some(last_fence) = after_fence.rfind("```") {
                return after_fence[..last_fence].trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Sanitize tool call arguments to enforce atomic, single-file discipline.
pub fn sanitize_tool_call_arguments(_tool_name: &str, raw_args: &str) -> String {
    let clean_raw = strip_markdown_fences(raw_args);
    let mut val: Value = match serde_json::from_str(clean_raw) {
        Ok(v) => v,
        Err(_) => return raw_args.to_string(),
    };

    if let Some(obj) = val.as_object_mut() {
        let path_key = if obj.contains_key("path") {
            Some("path")
        } else if obj.contains_key("file_path") {
            Some("file_path")
        } else if obj.contains_key("filepath") {
            Some("filepath")
        } else if obj.contains_key("filename") {
            Some("filename")
        } else {
            None
        };

        let content_key = if obj.contains_key("content") {
            Some("content")
        } else if obj.contains_key("contents") {
            Some("contents")
        } else if obj.contains_key("text") {
            Some("text")
        } else {
            None
        };

        if let Some(ck) = content_key {
            if let Some(content_str) = obj.get(ck).and_then(|c| c.as_str()) {
                let mut content = content_str.to_string();

                let first_line_header = content.lines().next().and_then(extract_file_header);
                if let Some(ref header_path) = first_line_header {
                    if let Some(pk) = path_key {
                        let current_path = obj.get(pk).and_then(|p| p.as_str()).unwrap_or("");
                        if current_path == "config.toml" || !current_path.ends_with(header_path.as_str()) {
                            obj.insert(pk.to_string(), Value::String(header_path.clone()));
                        }
                    }
                    if let Some(first_nl) = content.find('\n') {
                        content = content[first_nl + 1..].trim_start_matches('\n').to_string();
                    }
                }

                // If path is known, try extracting targeted code block matching the file extension
                if let Some(pk) = path_key {
                    if let Some(target_path) = obj.get(pk).and_then(|p| p.as_str()) {
                        if let Some(targeted) = extract_targeted_code_block(&content, target_path) {
                            content = targeted.to_string();
                        }
                    }
                }

                if let Some(delimiter_idx) = find_second_file_delimiter(&content) {
                    content.truncate(delimiter_idx);
                }

                let clean_content = strip_outer_code_fences(&content);
                obj.insert(ck.to_string(), Value::String(clean_content));
            }
        }
    }

    serde_json::to_string(&val).unwrap_or_else(|_| raw_args.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_header() {
        assert_eq!(extract_file_header("# core/Cargo.toml"), Some("core/Cargo.toml".into()));
        assert_eq!(extract_file_header("// core/src/keyring.rs"), Some("core/src/keyring.rs".into()));
        assert_eq!(extract_file_header("### File: app/src/main.rs"), Some("app/src/main.rs".into()));
        assert_eq!(extract_file_header("// Save the key"), None);
        assert_eq!(extract_file_header("// http://example.com"), None);
    }

    #[test]
    fn test_strip_markdown_fences() {
        let text = "```json\n{\"path\": \"test.rs\"}\n```";
        assert_eq!(strip_markdown_fences(text), "{\"path\": \"test.rs\"}");
    }

    #[test]
    fn test_sanitize_hermes_dump_payload() {
        let dumped_content = "# core/Cargo.toml\n\n[dependencies]\nkeyring = { version = \"2.0\", features = [\"secret-service\", \"kwallet\"]\n}\n\n\nrust\n// core/src/keyring.rs\n\nuse keyring::{create, get_password, set_password, delete_password};\n\nconst SERVICE_NAME: &str = \"swai-auditor\";\n";
        let raw_json = serde_json::json!({
            "content": dumped_content,
            "path": "config.toml"
        }).to_string();

        let sanitized = sanitize_tool_call_arguments("write_file", &raw_json);
        let parsed: Value = serde_json::from_str(&sanitized).unwrap();

        assert_eq!(parsed["path"], "core/Cargo.toml");
        let content = parsed["content"].as_str().unwrap();
        assert!(content.contains("[dependencies]"));
        assert!(content.contains("keyring = { version = \"2.0\""));
        assert!(!content.contains("core/src/keyring.rs"));
        assert!(!content.contains("SERVICE_NAME"));
    }

    #[test]
    fn test_preserve_normal_comments_in_code() {
        let code = "fn main() {\n    // Save the key into storage\n    let x = 1;\n}";
        let raw_json = serde_json::json!({
            "content": code,
            "path": "src/main.rs"
        }).to_string();

        let sanitized = sanitize_tool_call_arguments("write_file", &raw_json);
        let parsed: Value = serde_json::from_str(&sanitized).unwrap();
        assert_eq!(parsed["path"], "src/main.rs");
        assert_eq!(parsed["content"], code);
    }

    #[test]
    fn test_sanitize_tutorial_step_dump() {
        let text = "# Adding keyring crate dependency to core/Cargo.toml with Secret Service / KWallet Linux support\n\n## Step 1: Add keyring crate dependency to core/Cargo.toml\n\nFirst, we need to add the keyring crate to the core/Cargo.toml file with the appropriate features for Secret Service and KWallet support.\n\ntoml\n[dependencies]\nkeyring = { version = \"3.0\", features = [\"secret-service\", \"kwalletd\"] }\n\n\n## Step 2: Implement the swai_core::keyring module in core/src/keyring.rs\n\nrust\nuse keyring::Entry;\n";
        let raw_json = serde_json::json!({
            "content": text,
            "path": "core/Cargo.toml"
        }).to_string();

        let sanitized = sanitize_tool_call_arguments("write_file", &raw_json);
        let parsed: Value = serde_json::from_str(&sanitized).unwrap();

        assert_eq!(parsed["path"], "core/Cargo.toml");
        let content = parsed["content"].as_str().unwrap();
        assert!(content.contains("[dependencies]"));
        assert!(content.contains("keyring = { version = \"3.0\""));
        assert!(!content.contains("## Step 2"));
        assert!(!content.contains("use keyring::Entry"));
    }
}
