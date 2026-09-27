import re

with open('core/src/proxy/tool_calling.rs', 'r') as f:
    content = f.read()

# Replace remap_arguments_to_schema block
old_remap = """fn remap_arguments_to_schema(name: &str, args_json: String, tools: &[Value]) -> String {
    let mut args: serde_json::Map<String, Value> = match serde_json::from_str(&args_json) {
        Ok(Value::Object(m)) => m,
        _ => return args_json,
    };

    let tool = tools.iter().find(|t| {
        let n = t.get("name").or_else(|| t.get("function").and_then(|f| f.get("name"))).and_then(|n| n.as_str()).unwrap_or("");
        n.eq_ignore_ascii_case(name)
    });

    if let Some(t) = tool {
        if let Some(props) = t.get("input_schema")
            .or_else(|| t.get("parameters"))
            .or_else(|| t.get("function").and_then(|f| f.get("parameters")))
            .and_then(|p| p.get("properties"))
            .and_then(|p| p.as_object())
        {
            // Remap 'path' to 'file_path' if the tool expects 'file_path' but we have 'path'
            if props.contains_key("file_path") && !props.contains_key("path") {
                if let Some(val) = args.remove("path") {
                    args.insert("file_path".to_string(), val);
                }
            }
            // Remap 'file_path' to 'path' if the tool expects 'path' but we have 'file_path'
            if props.contains_key("path") && !props.contains_key("file_path") {
                if let Some(val) = args.remove("file_path") {
                    args.insert("path".to_string(), val);
                }
            }
            // Remap target paths for Aider 'filepath'
            if props.contains_key("filepath") && !props.contains_key("path") {
                if let Some(val) = args.remove("path").or_else(|| args.remove("file_path")) {
                    args.insert("filepath".to_string(), val);
                }
            }
        }
    }

    serde_json::to_string(&args).unwrap_or(args_json)
}"""

new_remap = """pub fn find_tool_in_tools<'a>(name: &str, tools: &'a [Value]) -> Option<&'a Value> {
    if let Some(t) = tools.iter().find(|t| {
        let n = t.get("name").or_else(|| t.get("function").and_then(|f| f.get("name"))).and_then(|n| n.as_str()).unwrap_or("");
        n.eq_ignore_ascii_case(name)
    }) {
        return Some(t);
    }
    let lower = name.to_ascii_lowercase();
    let is_write = matches!(lower.as_str(), "write" | "write_to_file" | "write_file" | "create_file" | "save_file" | "replace" | "replace_file" | "replace_file_content" | "edit_file" | "edit");
    let is_read = matches!(lower.as_str(), "read" | "read_file" | "view_file" | "view");
    let is_cmd = matches!(lower.as_str(), "terminal" | "bash" | "execute_command" | "command" | "run_command" | "bash_command");
    let is_search = matches!(lower.as_str(), "search_files" | "search" | "grep" | "glob" | "find");

    tools.iter().find(|t| {
        let n = t.get("name").or_else(|| t.get("function").and_then(|f| f.get("name"))).and_then(|n| n.as_str()).unwrap_or("").to_ascii_lowercase();
        if is_write && matches!(n.as_str(), "write" | "write_to_file" | "write_file" | "create_file" | "save_file" | "replace" | "replace_file" | "replace_file_content" | "edit_file" | "edit") {
            let is_surgical = t.get("input_schema")
                .or_else(|| t.get("parameters"))
                .or_else(|| t.get("function").and_then(|f| f.get("parameters")))
                .and_then(|p| p.get("properties"))
                .map_or(false, |props| props.get("old_string").is_some() || props.get("TargetContent").is_some());
            !is_surgical
        } else if is_read && matches!(n.as_str(), "read" | "read_file" | "view_file" | "view") {
            true
        } else if is_cmd && matches!(n.as_str(), "terminal" | "bash" | "execute_command" | "command" | "run_command" | "bash_command") {
            true
        } else if is_search && matches!(n.as_str(), "search_files" | "search" | "grep" | "glob" | "find") {
            true
        } else {
            false
        }
    })
}

pub fn remap_arguments_to_schema(name: &str, args_json: String, tools: &[Value]) -> String {
    let mut args: serde_json::Map<String, Value> = match serde_json::from_str(&args_json) {
        Ok(Value::Object(m)) => m,
        _ => return args_json,
    };

    let tool = find_tool_in_tools(name, tools);

    if let Some(t) = tool {
        if let Some(props) = t.get("input_schema")
            .or_else(|| t.get("parameters"))
            .or_else(|| t.get("function").and_then(|f| f.get("parameters")))
            .and_then(|p| p.get("properties"))
            .and_then(|p| p.as_object())
        {
            let path_candidates = ["path", "file_path", "filepath", "filename", "TargetFile", "file"];
            let expected_path_key = path_candidates.iter().find(|&&k| props.contains_key(k)).copied();
            if let Some(exp_key) = expected_path_key {
                if !args.contains_key(exp_key) {
                    let existing_val = path_candidates.iter().find_map(|&k| args.remove(k));
                    if let Some(val) = existing_val {
                        args.insert(exp_key.to_string(), val);
                    }
                }
            }

            let content_candidates = ["content", "contents", "CodeContent", "ReplacementContent", "text", "body"];
            let expected_content_key = content_candidates.iter().find(|&&k| props.contains_key(k)).copied();
            if let Some(exp_key) = expected_content_key {
                if !args.contains_key(exp_key) {
                    let existing_val = content_candidates.iter().find_map(|&k| args.remove(k));
                    if let Some(val) = existing_val {
                        args.insert(exp_key.to_string(), val);
                    }
                }
            }

            if props.contains_key("Overwrite") && !args.contains_key("Overwrite") {
                args.insert("Overwrite".to_string(), Value::Bool(true));
            }
            if props.contains_key("Description") && !args.contains_key("Description") {
                args.insert("Description".to_string(), Value::String("Updated by SWAI Council".to_string()));
            }

            let cmd_candidates = ["command", "CommandLine", "cmd"];
            let expected_cmd_key = cmd_candidates.iter().find(|&&k| props.contains_key(k)).copied();
            if let Some(exp_key) = expected_cmd_key {
                if !args.contains_key(exp_key) {
                    let existing_val = cmd_candidates.iter().find_map(|&k| args.remove(k));
                    if let Some(val) = existing_val {
                        args.insert(exp_key.to_string(), val);
                    }
                }
            }
            if props.contains_key("Cwd") && !args.contains_key("Cwd") {
                args.insert("Cwd".to_string(), Value::String(".".to_string()));
            }
            if props.contains_key("WaitMsBeforeAsync") && !args.contains_key("WaitMsBeforeAsync") {
                args.insert("WaitMsBeforeAsync".to_string(), Value::Number(5000.into()));
            }

            let pattern_candidates = ["pattern", "query", "regex", "search_term"];
            let expected_pattern_key = pattern_candidates.iter().find(|&&k| props.contains_key(k)).copied();
            if let Some(exp_key) = expected_pattern_key {
                if !args.contains_key(exp_key) {
                    let existing_val = pattern_candidates.iter().find_map(|&k| args.remove(k));
                    if let Some(val) = existing_val {
                        args.insert(exp_key.to_string(), val);
                    }
                }
            }
        }
    }

    serde_json::to_string(&args).unwrap_or(args_json)
}

pub fn format_immediate_tool_call(
    dir: &crate::council::planner::PlannerDirective,
    available_tools: Option<&[Value]>,
) -> String {
    let mut tool_name = dir.tool.clone();
    let target = &dir.target;
    let a = dir.action.to_ascii_lowercase();

    if (tool_name.contains("terminal") || tool_name.is_empty())
        && (a.contains("read") || a.contains("inspect") || a.contains("examine"))
        && (target.contains('.') || target.contains('/'))
    {
        tool_name = "read_file".into();
    }

    let lower_tool = tool_name.to_ascii_lowercase();
    let mut args = serde_json::Map::new();
    if lower_tool.contains("read") {
        args.insert("path".to_string(), Value::String(target.to_string()));
    } else if lower_tool.contains("search") || lower_tool.contains("grep") || lower_tool.contains("find") {
        args.insert("pattern".to_string(), Value::String(target.to_string()));
    } else if lower_tool.contains("terminal") || lower_tool.contains("run_command") || lower_tool.contains("bash") || lower_tool.contains("execute") || lower_tool.contains("explore") {
        args.insert("command".to_string(), Value::String(target.to_string()));
    } else {
        args.insert("path".to_string(), Value::String(target.to_string()));
    }

    let mut args_str = serde_json::to_string(&args).unwrap_or_default();

    if let Some(tools) = available_tools {
        if let Some(target_tool) = find_tool_in_tools(&tool_name, tools) {
            let actual_name = target_tool
                .get("name")
                .or_else(|| target_tool.get("function").and_then(|f| f.get("name")))
                .and_then(|n| n.as_str())
                .unwrap_or(&tool_name);
            tool_name = actual_name.to_string();
        }
        args_str = remap_arguments_to_schema(&tool_name, args_str, tools);
    } else {
        if lower_tool.contains("terminal") || lower_tool.contains("run_command") {
            tool_name = "run_command".into();
            args.insert("CommandLine".to_string(), Value::String(target.to_string()));
            args.insert("Cwd".to_string(), Value::String(".".to_string()));
            args.insert("WaitMsBeforeAsync".to_string(), Value::Number(5000.into()));
            args.remove("command");
            args_str = serde_json::to_string(&args).unwrap_or_default();
        }
    }

    let parsed_args: Value = serde_json::from_str(&args_str).unwrap_or(Value::Object(args));
    serde_json::json!({
        "name": tool_name,
        "arguments": parsed_args
    }).to_string()
}"""

content = content.replace(old_remap, new_remap)

# Also fix the fallback 3 manual guessing to use remap_arguments_to_schema
old_fallback_3 = """            let (path_key, content_key) = if let Some(props) = tool_entry
                .get("input_schema")
                .or_else(|| tool_entry.get("parameters"))
                .or_else(|| tool_entry.get("function").and_then(|f| f.get("parameters")))
                .and_then(|p| p.get("properties"))
            {
                let p = if props.get("path").is_some() { "path" } else if props.get("TargetFile").is_some() { "TargetFile" } else if props.get("filepath").is_some() { "filepath" } else if props.get("filename").is_some() { "filename" } else { "file_path" };
                let c = if props.get("contents").is_some() { "contents" } else if props.get("CodeContent").is_some() { "CodeContent" } else if props.get("ReplacementContent").is_some() { "ReplacementContent" } else if props.get("text").is_some() { "text" } else { "content" };
                (p, c)
            } else {
                ("file_path", "content")
            };

            let filename_opt = target_override
                .map(|s| s.to_string())
                .or_else(|| extract_target_from_prompt(prompt))
                .or_else(|| crate::proxy::tool_sanitizer::extract_filename_from_code_block(text));

            if let Some(filename) = filename_opt {
                if !filename.is_empty() {
                    let actual_code = if let Some(stripped) = text.strip_prefix(&format!("```{}", filename)) {
                        stripped
                    } else {
                        text
                    };
                    
                    let blocks = crate::proxy::tool_sanitizer::extract_markdown_blocks(actual_code);
                    if !blocks.is_empty() {
                        let content_to_write = crate::proxy::tool_sanitizer::extract_targeted_code_block(text, &filename)
                            .unwrap_or(actual_code);
                        let mut args_map = serde_json::Map::new();
                        args_map.insert(path_key.into(), Value::String(filename));
                        args_map.insert(content_key.into(), Value::String(content_to_write.to_string()));

                        if let Some(props) = tool_entry
                            .get("input_schema")
                            .or_else(|| tool_entry.get("parameters"))
                            .or_else(|| tool_entry.get("function").and_then(|f| f.get("parameters")))
                            .and_then(|p| p.get("properties"))
                        {
                            if props.get("Overwrite").is_some() {
                                args_map.insert("Overwrite".to_string(), Value::Bool(true));
                            }
                            if props.get("Description").is_some() {
                                args_map.insert("Description".to_string(), Value::String("Updated by SWAI Council".to_string()));
                            }
                        }

                        let raw_args = serde_json::to_string(&args_map).unwrap_or_default();
                        let sanitized_args = sanitize_tool_call_arguments(tool_name, &raw_args);
                        return Some(ToolCallExtraction {
                            name: tool_name.to_string(),
                            arguments: sanitized_args,
                        });
                    }
                }
            }"""

new_fallback_3 = """            let filename_opt = target_override
                .map(|s| s.to_string())
                .or_else(|| extract_target_from_prompt(prompt))
                .or_else(|| crate::proxy::tool_sanitizer::extract_filename_from_code_block(text));

            if let Some(filename) = filename_opt {
                if !filename.is_empty() {
                    let actual_code = if let Some(stripped) = text.strip_prefix(&format!("```{}", filename)) {
                        stripped
                    } else {
                        text
                    };
                    
                    let blocks = crate::proxy::tool_sanitizer::extract_markdown_blocks(actual_code);
                    if !blocks.is_empty() {
                        let content_to_write = crate::proxy::tool_sanitizer::extract_targeted_code_block(text, &filename)
                            .unwrap_or(actual_code);
                        let mut args_map = serde_json::Map::new();
                        args_map.insert("path".to_string(), Value::String(filename));
                        args_map.insert("content".to_string(), Value::String(content_to_write.to_string()));

                        let raw_args = serde_json::to_string(&args_map).unwrap_or_default();
                        let sanitized_args = sanitize_tool_call_arguments(tool_name, &raw_args);
                        let remapped_args = remap_arguments_to_schema(tool_name, sanitized_args, tools);
                        return Some(ToolCallExtraction {
                            name: tool_name.to_string(),
                            arguments: remapped_args,
                        });
                    }
                }
            }"""

content = content.replace(old_fallback_3, new_fallback_3)


old_extract_1 = """                    if let Some(tools) = available_tools {
                        sanitized_args = remap_arguments_to_schema(name, sanitized_args, tools);
                    }
                    return Some(ToolCallExtraction {
                        name: name.to_string(),
                        arguments: sanitized_args,
                    });"""

new_extract_1 = """                    if let Some(tools) = available_tools {
                        sanitized_args = remap_arguments_to_schema(name, sanitized_args, tools);
                    }
                    let actual_name = if let Some(tools) = available_tools {
                        find_tool_in_tools(name, tools)
                            .and_then(|t| t.get("name").or_else(|| t.get("function").and_then(|f| f.get("name"))).and_then(|n| n.as_str()))
                            .unwrap_or(name)
                    } else {
                        name
                    };
                    return Some(ToolCallExtraction {
                        name: actual_name.to_string(),
                        arguments: sanitized_args,
                    });"""

content = content.replace(old_extract_1, new_extract_1)

old_extract_2 = """            if let Some(tools) = available_tools {
                sanitized_args = remap_arguments_to_schema(func_name.trim(), sanitized_args, tools);
            }
            return Some(ToolCallExtraction {
                name: func_name.trim().to_string(),
                arguments: sanitized_args,
            });"""

new_extract_2 = """            if let Some(tools) = available_tools {
                sanitized_args = remap_arguments_to_schema(func_name.trim(), sanitized_args, tools);
            }
            let actual_name = if let Some(tools) = available_tools {
                find_tool_in_tools(func_name.trim(), tools)
                    .and_then(|t| t.get("name").or_else(|| t.get("function").and_then(|f| f.get("name"))).and_then(|n| n.as_str()))
                    .unwrap_or(func_name.trim())
            } else {
                func_name.trim()
            };
            return Some(ToolCallExtraction {
                name: actual_name.to_string(),
                arguments: sanitized_args,
            });"""

content = content.replace(old_extract_2, new_extract_2)

old_extract_4 = """                                let mut args_map = serde_json::Map::new();
                                args_map.insert("command".to_string(), Value::String(cmd.to_string()));
                                return Some(ToolCallExtraction {
                                    name: tool_name.to_string(),
                                    arguments: serde_json::to_string(&args_map).unwrap_or_default(),
                                });"""

new_extract_4 = """                                let mut args_map = serde_json::Map::new();
                                args_map.insert("command".to_string(), Value::String(cmd.to_string()));
                                let raw_args = serde_json::to_string(&args_map).unwrap_or_default();
                                let remapped_args = remap_arguments_to_schema(tool_name, raw_args, tools);
                                return Some(ToolCallExtraction {
                                    name: tool_name.to_string(),
                                    arguments: remapped_args,
                                });"""

content = content.replace(old_extract_4, new_extract_4)

with open('core/src/proxy/tool_calling.rs', 'w') as f:
    f.write(content)
