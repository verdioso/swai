//! SWAI — Tool calling conversion and extraction for Council debates.

use serde_json::Value;
pub use crate::proxy::tool_sanitizer::sanitize_tool_call_arguments;

/// Extracted tool call from model output.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallExtraction {
    pub name: String,
    pub arguments: String,
}

/// Filter client tools so non-coding tools (e.g. text_to_speech) do not pollute the Council prompt.
fn is_allowed_coding_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "read_file" | "read" | "write_file" | "write" | "write_to_file" | "create_file"
            | "edit" | "multiedit" | "replace" | "patch" | "todowrite" | "task"
            | "terminal" | "bash" | "execute_command" | "command"
            | "search_files" | "search" | "grep" | "glob" | "view_file" | "view"
    )
}

/// Convert client tool definitions (Anthropic or OpenAI) into OpenAI tool schemas.
pub fn extract_openai_tools(request_body: &[u8]) -> Option<Vec<Value>> {
    let json: Value = serde_json::from_slice(request_body).ok()?;
    let tools_arr = json.get("tools").and_then(|t| t.as_array())?;
    let mut out = Vec::new();

    for t in tools_arr {
        let name = t.get("name")
            .or_else(|| t.get("function").and_then(|f| f.get("name")))
            .and_then(|n| n.as_str())
            .unwrap_or("");
        if !is_allowed_coding_tool(name) {
            continue;
        }
        if t.get("type").and_then(|s| s.as_str()) == Some("function") {
            out.push(t.clone());
        } else if !name.is_empty() {
            let desc = t.get("description").and_then(|d| d.as_str()).unwrap_or("");
            let params = t.get("input_schema").cloned().unwrap_or(serde_json::json!({
                "type": "object", "properties": {}
            }));
            out.push(serde_json::json!({
                "type": "function",
                "function": { "name": name, "description": desc, "parameters": params }
            }));
        }
    }

    if !out.is_empty() { Some(out) } else { None }
}

/// Generate a strict tool-calling protocol prompt based on available client tools.
pub fn build_tool_protocol_instructions(tools: &[Value]) -> String {
    let mut s = String::from("You have access to the following tools:\n");
    for t in tools {
        let name = t.get("name").or_else(|| t.get("function").and_then(|f| f.get("name"))).and_then(|n| n.as_str()).unwrap_or("unknown");
        let desc = t.get("description").or_else(|| t.get("function").and_then(|f| f.get("description"))).and_then(|d| d.as_str()).unwrap_or("");
        s.push_str(&format!("- {}: {}\n", name, desc));
    }
    s.push_str("\nCRITICAL TOOL-CALLING DIRECTIVES:\n1. ATOMIC: Execute ONLY ONE tool call per turn.\n2. SINGLE-FILE: Target EXACTLY ONE file. Never bundle multiple files.\n3. RAW PAYLOAD: Do not wrap file contents in markdown code fences inside arguments.\n4. NO CHAT: Emit only the structured tool call or code without conversational commentary.\n5. FORMAT: If calling a tool, emit JSON using the tool's exact schema parameter names: {\"name\": \"TOOL_NAME\", \"arguments\": { ... }}\n");
    s
}

/// Inject tool-calling discipline rules and prepend Planner stage into council pipeline.
pub fn inject_tool_discipline_into_config(
    config: &mut crate::council::CouncilPipelineConfig,
    tools: &[Value],
) {
    let protocol = build_tool_protocol_instructions(tools);

    // If no Planner stage is present, insert one at index 0 using Auditor's model
    if !config.stages.iter().any(|s| s.role == crate::council::CouncilRole::Planner) {
        let planner_model = config.stages.iter()
            .find(|s| s.role == crate::council::CouncilRole::Auditor)
            .map(|s| s.model_id.clone())
            .unwrap_or_else(|| config.stages.first().map(|s| s.model_id.clone()).unwrap_or_default());

        if !planner_model.is_empty() {
            config.stages.insert(0, crate::council::PipelineStage {
                model_id: planner_model,
                role: crate::council::CouncilRole::Planner,
                prompt_template: "".into(),
                temperature: 0.2,
                top_p: 0.9,
                system_prompt: Some(protocol.clone()),
            });
        }
    }

    for stage in &mut config.stages {
        if let Some(ref mut existing) = stage.system_prompt {
            if !existing.contains("CRITICAL TOOL-CALLING DIRECTIVES") {
                existing.push_str("\n\n");
                existing.push_str(&protocol);
            }
        } else {
            stage.system_prompt = Some(protocol.clone());
        }
    }
}

/// Find a JSON tool call object candidate with balanced braces starting with a tool call key.
pub fn find_json_tool_call_candidate(text: &str) -> Option<&str> {
    let patterns = ["{\"name\"", "{\n  \"name\"", "{\"tool\"", "{\n  \"tool\"", "{\"function\"", "{ \"name\""];
    for pat in patterns {
        if let Some(start) = text.find(pat) {
            let slice = &text[start..];
            let (mut depth, mut end_idx, mut in_str, mut escape) = (0, None, false, false);
            for (idx, ch) in slice.char_indices() {
                if escape { escape = false; continue; }
                if ch == '\\' { escape = true; continue; }
                if ch == '"' { in_str = !in_str; continue; }
                if !in_str {
                    if ch == '{' { depth += 1; }
                    else if ch == '}' { depth -= 1; if depth == 0 { end_idx = Some(idx); break; } }
                }
            }
            if let Some(end) = end_idx { return Some(&slice[..=end]); }
        }
    }
    None
}

fn is_tool_result_envelope(s: &str) -> bool {
    let t = s.trim();
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(t) {
        if map.contains_key("name") || map.contains_key("tool") {
            return false;
        }
        return map.contains_key("success")
            || map.contains_key("error")
            || map.contains_key("output")
            || map.contains_key("stdout")
            || map.contains_key("stderr")
            || map.contains_key("total_count")
            || map.contains_key("bytes_written");
    }
    t.starts_with("{\"success\":")
        || t.starts_with("{\n  \"success\":")
        || t.starts_with("{\"error\":")
        || t.starts_with("{\n  \"error\":")
        || t.starts_with("{\"total_count\":")
}

/// Extract tool call from model output text.
pub fn extract_tool_call(
    text: &str,
    prompt: &str,
    available_tools: Option<&[Value]>,
    target_override: Option<&str>,
) -> Option<ToolCallExtraction> {
    let trimmed = text.trim();

    // 1. Direct or embedded JSON tool call: {"name": "...", "arguments": ...}
    if let Some(json_candidate) = find_json_tool_call_candidate(trimmed).or_else(|| {
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            Some(trimmed)
        } else {
            None
        }
    }) {
        if let Ok(val) = serde_json::from_str::<Value>(json_candidate) {
            if let Some(name) = val.get("name").or_else(|| val.get("tool")).and_then(|n| n.as_str()) {
                if let Some(args) = val.get("arguments").or_else(|| val.get("parameters")) {
                    let content_val = args.get("content")
                        .or_else(|| args.get("contents"))
                        .or_else(|| args.get("text"))
                        .and_then(|c| c.as_str());
                    if let Some(c) = content_val {
                        if is_tool_result_envelope(c) {
                            return None;
                        }
                    }
                    let args_str = if args.is_string() {
                        args.as_str().unwrap().to_string()
                    } else {
                        args.to_string()
                    };
                    let sanitized_args = sanitize_tool_call_arguments(name, &args_str);
                    return Some(ToolCallExtraction {
                        name: name.to_string(),
                        arguments: sanitized_args,
                    });
                }
            }
        }
    }

    // 2. XML-style tool call: <tool_call><function=Write>...
    if text.contains("<tool_call>") && text.contains("<function=") {
        if let Some(func_name) = text.split("<function=").nth(1).and_then(|s| s.split('>').next()) {
            let mut args_map = serde_json::Map::new();
            for chunk in text.split("<parameter=").skip(1) {
                if let Some((k, rest)) = chunk.split_once('>') {
                    if let Some((v, _)) = rest.split_once("</parameter>") {
                        args_map.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
                    }
                }
            }
            let raw_args = serde_json::to_string(&args_map).unwrap_or_default();
            return Some(ToolCallExtraction {
                name: func_name.trim().to_string(),
                arguments: sanitize_tool_call_arguments(func_name.trim(), &raw_args),
            });
        }
    }

    // 3. Fallback: If client provides "Write" / "write_to_file" tool and text contains code
    if let Some(tools) = available_tools {
        let write_tool = tools.iter().find(|t| {
            let n = t.get("name").or_else(|| t.get("function").and_then(|f| f.get("name"))).and_then(|n| n.as_str()).unwrap_or("");
            matches!(n.to_ascii_lowercase().as_str(), "write" | "write_to_file" | "write_file" | "create_file" | "save_file")
        });

        if let Some(tool_entry) = write_tool {
            let tool_name = tool_entry.get("name").or_else(|| tool_entry.get("function").and_then(|f| f.get("name"))).and_then(|n| n.as_str()).unwrap_or("Write");

            let (path_key, content_key) = if let Some(props) = tool_entry
                .get("input_schema")
                .or_else(|| tool_entry.get("parameters"))
                .or_else(|| tool_entry.get("function").and_then(|f| f.get("parameters")))
                .and_then(|p| p.get("properties"))
            {
                let p = if props.get("path").is_some() { "path" } else if props.get("filepath").is_some() { "filepath" } else if props.get("filename").is_some() { "filename" } else { "file_path" };
                let c = if props.get("contents").is_some() { "contents" } else if props.get("text").is_some() { "text" } else { "content" };
                (p, c)
            } else {
                ("file_path", "content")
            };

            let filename_opt = target_override
                .map(|s| s.to_string())
                .or_else(|| extract_target_from_prompt(prompt))
                .or_else(|| extract_target_from_prompt(text))
                .or_else(|| extract_filename_hint(text))
                .or_else(|| extract_filename_hint(prompt));

            if let Some(filename) = filename_opt {
                let code_opt = if let Some(code_start) = text.find("```") {
                    let after_ticks = &text[code_start + 3..];
                    after_ticks.find('\n').and_then(|first_line_end| {
                        let code_body = &after_ticks[first_line_end + 1..];
                        code_body.rfind("```").map(|code_end| code_body[..code_end].trim())
                    })
                } else if !is_audit_review_text(trimmed) && !trimmed.is_empty() {
                    Some(trimmed)
                } else {
                    None
                };

                if let Some(actual_code) = code_opt {
                    if !is_audit_review_text(actual_code) && !is_tool_result_envelope(actual_code) {
                        let content_to_write = crate::proxy::tool_sanitizer::extract_targeted_code_block(text, &filename)
                            .unwrap_or(actual_code);
                        let mut args_map = serde_json::Map::new();
                        args_map.insert(path_key.into(), Value::String(filename));
                        args_map.insert(content_key.into(), Value::String(content_to_write.to_string()));
                        let raw_args = serde_json::to_string(&args_map).unwrap_or_default();
                        let sanitized_args = sanitize_tool_call_arguments(tool_name, &raw_args);
                        return Some(ToolCallExtraction {
                            name: tool_name.to_string(),
                            arguments: sanitized_args,
                        });
                    }
                }
            }
        }


        // 4. Fallback: If client provides "terminal" or "bash" tool and text contains shell command
        let term_tool = tools.iter().find(|t| {
            let n = t.get("name")
                .or_else(|| t.get("function").and_then(|f| f.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            n.eq_ignore_ascii_case("terminal")
                || n.eq_ignore_ascii_case("bash")
                || n.eq_ignore_ascii_case("execute_command")
                || n.eq_ignore_ascii_case("command")
        });

        if let Some(tool_entry) = term_tool {
            let tool_name = tool_entry.get("name")
                .or_else(|| tool_entry.get("function").and_then(|f| f.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or("terminal");

            for tag in &["```sh", "```bash", "```shell"] {
                if let Some(start) = text.find(tag) {
                    let after = &text[start + tag.len()..];
                    if let Some(first_nl) = after.find('\n') {
                        let body = &after[first_nl + 1..];
                        if let Some(end) = body.find("```") {
                            let cmd = body[..end].trim();
                            if !cmd.is_empty() {
                                let mut args_map = serde_json::Map::new();
                                args_map.insert("command".to_string(), Value::String(cmd.to_string()));
                                return Some(ToolCallExtraction {
                                    name: tool_name.to_string(),
                                    arguments: serde_json::to_string(&args_map).unwrap_or_default(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

fn is_audit_review_text(s: &str) -> bool {
    let l = s.to_lowercase();
    l.contains("critical issues:") || l.contains("audit critiques:") || l.contains("## critical issues") || l.contains("review and critique")
}

fn extract_target_from_prompt(prompt: &str) -> Option<String> {
    prompt.lines().find_map(|line| {
        let s = line.trim().strip_prefix("- Target:")?.trim();
        if !s.is_empty() && (s.contains('.') || s.contains('/')) && !s.contains(' ') && !s.to_lowercase().starts_with("plan/") {
            Some(s.to_string())
        } else {
            None
        }
    })
}

/// Extract a filename hint like `core/Cargo.toml` or `src/main.rs` from text.
pub fn extract_filename_hint(text: &str) -> Option<String> {
    let mut specific_path = None;
    let mut general_file = None;
    let exts = [".html", ".rs", ".js", ".ts", ".py", ".toml", ".json", ".css"];

    for word in text.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '_' && c != '/' && c != '-');
        if clean.to_lowercase().starts_with("plan/") || clean.starts_with("http") || clean.len() <= 3 {
            continue;
        }
        if exts.iter().any(|e| clean.ends_with(e)) {
            if clean.contains('/') || clean.contains('\\') {
                if specific_path.is_none() { specific_path = Some(clean.to_string()); }
            } else if general_file.is_none() {
                general_file = Some(clean.to_string());
            }
        }
    }
    specific_path.or(general_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_filename_hint() {
        assert_eq!(extract_filename_hint("Create a new file form.html with"), Some("form.html".into()));
        assert_eq!(extract_filename_hint("Write scratch/lru_cache.rs code"), Some("scratch/lru_cache.rs".into()));
        assert_eq!(extract_filename_hint("Never store in config.toml, edit core/Cargo.toml"), Some("core/Cargo.toml".into()));
    }

    #[test]
    fn test_extract_xml_tool_call() {
        let text = "<tool_call>\n<function=Write>\n<parameter=file_path>\nform.html\n</parameter>\n<parameter=content>\n<h1>Test</h1>\n</parameter>\n</tool_call>";
        let extracted = extract_tool_call(text, "", None, None).unwrap();
        assert_eq!(extracted.name, "Write");
        assert!(extracted.arguments.contains("form.html"));
        assert!(extracted.arguments.contains("<h1>Test</h1>"));
    }

    #[test]
    fn test_extract_json_tool_call_amidst_rust_and_bash_braces() {
        let text = "echo 'keyring = { version = \"0.8\" }' >> Cargo.toml\nmod tests {\n fn test() {}\n}\n{\"name\": \"write_file\", \"arguments\": {\"content\": \"pub fn save() {}\", \"path\": \"core/src/keyring.rs\"}}\n";
        let extracted = extract_tool_call(text, "", None, None).expect("should extract embedded tool call");
        assert_eq!(extracted.name, "write_file");
        assert!(extracted.arguments.contains("core/src/keyring.rs"));
    }

    #[test]
    fn test_extract_tool_call_with_target_override_unfenced() {
        let tools = vec![serde_json::json!({"name": "write_file", "parameters": {"properties": {"path": {}, "content": {}}}})];
        let code = "pub fn add(a: i32, b: i32) -> i32 { a + b }";
        let extracted = extract_tool_call(code, "", Some(&tools), Some("src/math.rs")).unwrap();
        assert_eq!(extracted.name, "write_file");
        assert!(extracted.arguments.contains("src/math.rs"));
        assert!(extracted.arguments.contains("pub fn add"));
    }

    #[test]
    fn test_build_tool_protocol_instructions() {
        let tools = vec![serde_json::json!({"name": "write_file", "description": "Writes file"})];
        let protocol = build_tool_protocol_instructions(&tools);
        assert!(protocol.contains("write_file") && protocol.contains("ATOMIC") && protocol.contains("SINGLE-FILE"));
    }

    #[test]
    fn test_inject_tool_discipline_into_config() {
        let tools = vec![serde_json::json!({"name": "write_file", "description": "Writes file"})];
        let mut config = crate::council::CouncilPipelineConfig {
            stages: vec![crate::council::PipelineStage {
                model_id: "test".into(), role: crate::council::CouncilRole::Generator,
                prompt_template: "".into(), temperature: 0.7, top_p: 0.9,
                system_prompt: Some("Base system prompt".into()),
            }],
            ..Default::default()
        };
        inject_tool_discipline_into_config(&mut config, &tools);
        assert_eq!(config.stages.len(), 2);
        assert_eq!(config.stages[0].role, crate::council::CouncilRole::Planner);
        let sys = config.stages[1].system_prompt.as_ref().unwrap();
        assert!(sys.contains("Base system prompt") && sys.contains("write_file"));
    }

    #[test]
    fn test_target_extraction_and_plan_skip() {
        assert_eq!(extract_filename_hint("Follow instructions in PLAN/PHASES/phase35.md"), None);
        assert_eq!(extract_target_from_prompt("Architect:\n- Target: core/Cargo.toml\n"), Some("core/Cargo.toml".into()));
    }

    #[test]
    fn test_reject_fenced_tool_result_envelope() {
        let tools = vec![serde_json::json!({"name": "write_file", "parameters": {"properties": {"path": {}, "content": {}}}})];
        let fenced = "```json\n{\n  \"success\": false,\n  \"error\": \"Refusing to write\"\n}\n```";
        assert_eq!(extract_tool_call(fenced, "", Some(&tools), Some("core/Cargo.toml")), None);
    }

    #[test]
    fn test_allowed_coding_tool_filter() {
        let body = serde_json::json!({"tools": [
            {"type": "function", "function": {"name": "text_to_speech"}},
            {"type": "function", "function": {"name": "edit"}},
            {"type": "function", "function": {"name": "write_file"}}
        ]});
        let extracted = extract_openai_tools(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(extracted.len(), 2);
        assert!(extracted.iter().any(|t| t["function"]["name"] == "edit"));
        assert!(extracted.iter().any(|t| t["function"]["name"] == "write_file"));
        assert!(!extracted.iter().any(|t| t["function"]["name"] == "text_to_speech"));
    }
}
