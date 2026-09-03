//! SWAI — Tool calling conversion and extraction for Council debates.

use serde_json::Value;

/// Extracted tool call from model output.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallExtraction {
    pub name: String,
    pub arguments: String,
}

/// Convert client tool definitions (Anthropic or OpenAI) into OpenAI tool schemas.
pub fn extract_openai_tools(request_body: &[u8]) -> Option<Vec<Value>> {
    let json: Value = serde_json::from_slice(request_body).ok()?;
    let tools_arr = json.get("tools").and_then(|t| t.as_array())?;
    let mut out = Vec::new();

    for t in tools_arr {
        if t.get("type").and_then(|s| s.as_str()) == Some("function") {
            out.push(t.clone());
        } else if let Some(name) = t.get("name").and_then(|n| n.as_str()) {
            let desc = t.get("description").and_then(|d| d.as_str()).unwrap_or("");
            let params = t.get("input_schema").cloned().unwrap_or(serde_json::json!({
                "type": "object",
                "properties": {}
            }));
            out.push(serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": desc,
                    "parameters": params
                }
            }));
        }
    }

    if !out.is_empty() {
        Some(out)
    } else {
        None
    }
}

/// Extract tool call from model output text.
pub fn extract_tool_call(text: &str, prompt: &str, available_tools: Option<&[Value]>) -> Option<ToolCallExtraction> {
    let trimmed = text.trim();

    // 1. Direct JSON tool call: {"name": "...", "arguments": ...}
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        if let Some(name) = val.get("name").and_then(|n| n.as_str()) {
            if let Some(args) = val.get("arguments") {
                let args_str = if args.is_string() {
                    args.as_str().unwrap().to_string()
                } else {
                    args.to_string()
                };
                return Some(ToolCallExtraction {
                    name: name.to_string(),
                    arguments: args_str,
                });
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
            return Some(ToolCallExtraction {
                name: func_name.trim().to_string(),
                arguments: serde_json::to_string(&args_map).unwrap_or_default(),
            });
        }
    }

    // 3. Fallback: If client provides "Write" / "write_to_file" tool and text contains markdown code
    if let Some(tools) = available_tools {
        let write_tool = tools.iter().find(|t| {
            let n = t.get("name")
                .or_else(|| t.get("function").and_then(|f| f.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            n.eq_ignore_ascii_case("write")
                || n.eq_ignore_ascii_case("write_to_file")
                || n.eq_ignore_ascii_case("write_file")
                || n.eq_ignore_ascii_case("create_file")
                || n.eq_ignore_ascii_case("save_file")
        });

        if let Some(tool_entry) = write_tool {
            let tool_name = tool_entry.get("name")
                .or_else(|| tool_entry.get("function").and_then(|f| f.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or("Write");

            let (path_key, content_key) = if let Some(props) = tool_entry
                .get("input_schema")
                .or_else(|| tool_entry.get("parameters"))
                .or_else(|| tool_entry.get("function").and_then(|f| f.get("parameters")))
                .and_then(|p| p.get("properties"))
            {
                let p = if props.get("path").is_some() {
                    "path"
                } else if props.get("filepath").is_some() {
                    "filepath"
                } else if props.get("filename").is_some() {
                    "filename"
                } else {
                    "file_path"
                };
                let c = if props.get("contents").is_some() {
                    "contents"
                } else if props.get("text").is_some() {
                    "text"
                } else {
                    "content"
                };
                (p, c)
            } else {
                ("file_path", "content")
            };

            if let Some(code_start) = text.find("```") {
                let after_ticks = &text[code_start + 3..];
                if let Some(first_line_end) = after_ticks.find('\n') {
                    let code_body = &after_ticks[first_line_end + 1..];
                    if let Some(code_end) = code_body.rfind("```") {
                        let actual_code = code_body[..code_end].trim();
                        if let Some(filename) = extract_filename_hint(prompt).or_else(|| extract_filename_hint(text)) {
                            let mut args_map = serde_json::Map::new();
                            args_map.insert(path_key.into(), Value::String(filename));
                            args_map.insert(content_key.into(), Value::String(actual_code.to_string()));
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

    None
}

/// Extract a filename hint like `form.html` or `scratch/lru_cache.rs` from text.
pub fn extract_filename_hint(text: &str) -> Option<String> {
    for word in text.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '_' && c != '/' && c != '-');
        if (clean.ends_with(".html") || clean.ends_with(".rs") || clean.ends_with(".js") || clean.ends_with(".ts") || clean.ends_with(".py") || clean.ends_with(".toml") || clean.ends_with(".json") || clean.ends_with(".css"))
            && !clean.starts_with("http")
            && clean.len() > 3
        {
            return Some(clean.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_filename_hint() {
        assert_eq!(extract_filename_hint("Create a new file form.html with"), Some("form.html".into()));
        assert_eq!(extract_filename_hint("Write scratch/lru_cache.rs code"), Some("scratch/lru_cache.rs".into()));
    }

    #[test]
    fn test_extract_xml_tool_call() {
        let text = "<tool_call>\n<function=Write>\n<parameter=file_path>\nform.html\n</parameter>\n<parameter=content>\n<h1>Test</h1>\n</parameter>\n</tool_call>";
        let extracted = extract_tool_call(text, "", None).unwrap();
        assert_eq!(extracted.name, "Write");
        assert!(extracted.arguments.contains("form.html"));
        assert!(extracted.arguments.contains("<h1>Test</h1>"));
    }
}
