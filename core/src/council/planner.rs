//! SWAI — Council Planner/Architect stage for atomic tool execution.

use serde::{Deserialize, Serialize};

/// Structured execution plan emitted by the Planner stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannerDirective {
    #[serde(default, alias = "name", alias = "function")]
    pub tool: String,
    #[serde(default, alias = "path", alias = "file", alias = "command")]
    pub target: String,
    #[serde(default, alias = "description", alias = "step")]
    pub action: String,
    #[serde(default, alias = "constraints")]
    pub rules: String,
}

impl Default for PlannerDirective {
    fn default() -> Self {
        Self {
            tool: "write_file".into(),
            target: "".into(),
            action: "".into(),
            rules: "Emit only one atomic operation for the target file.".into(),
        }
    }
}

/// Build the prompt for the Planner (Ornith 35B) to decompose a task.
pub fn build_planner_prompt(input_prompt: &str, tool_protocol: &str) -> String {
    let directive = if input_prompt.contains("Execution History & Tool Results:") {
        "1. If the prompt above contains an \"Execution History & Tool Results\" section, that\n\
         means earlier atomic steps have already been executed. Read that history carefully\n\
         and determine the NEXT unexecuted atomic step — do NOT repeat, re-plan, or re-target\n\
         a step that the history already shows as completed.\n\
         2. Do NOT write full code or solve all steps now — only the single next step.\n\
         3. Determine which single file or command must be executed for that step."
    } else {
        "1. Break this task down into the FIRST immediate atomic step.\n\
         2. Do NOT write full code or solve all steps now.\n\
         3. Determine which single file or command must be executed FIRST."
    };

    format!(
        "You are the Lead Software Architect and Planner in the Council.\n\
        The user has requested the following task:\n\
        ---\n\
        {}\n\
        ---\n\n\
        {}\n\n\
        YOUR ARCHITECTURAL DIRECTIVE:\n\
        {}\n\
        4. Output your plan strictly as a JSON object:\n\
        {{\n  \
          \"tool\": \"<target tool name>\",\n  \
          \"target\": \"<single target file or command>\",\n  \
          \"action\": \"<concise description of what to implement in this step>\",\n  \
          \"rules\": \"<specific constraints for this step>\"\n\
        }}\n\n\
        CRITICAL DIRECTIVE: You must output ONLY the JSON object and nothing else. Do not output any conversational text before or after the JSON. Stop generating immediately after the closing '}}'.",
        input_prompt, tool_protocol, directive
    )
}

/// Verdict emitted by the Auditor.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditVerdict {
    pub approved: bool,
    pub critique: String,
}

pub fn parse_audit_verdict(output: &str) -> Option<AuditVerdict> {
    for json_str in find_balanced_json_candidates(output) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(status) = v.get("status").and_then(|s| s.as_str()) {
                let approved = status.eq_ignore_ascii_case("approved");
                let critique = v.get("critique").and_then(|c| c.as_str()).unwrap_or("").to_string();
                return Some(AuditVerdict { approved, critique });
            }
        }
    }
    None
}

/// Find candidate JSON objects with balanced braces in text.
fn find_balanced_json_candidates(text: &str) -> Vec<&str> {
    let mut candidates = Vec::new();
    for (start, ch) in text.char_indices() {
        if ch == '{' {
            let slice = &text[start..];
            let (mut depth, mut in_str, mut escape) = (0, false, false);
            for (idx, c) in slice.char_indices() {
                if escape { escape = false; continue; }
                if c == '\\' { escape = true; continue; }
                if c == '"' { in_str = !in_str; continue; }
                if !in_str {
                    if c == '{' { depth += 1; }
                    else if c == '}' {
                        depth -= 1;
                        if depth == 0 {
                            candidates.push(&slice[..=idx]);
                            break;
                        }
                    }
                }
            }
        }
    }
    candidates
}

/// Parse the JSON or XML directive emitted by the Planner.
pub fn parse_planner_directive(output: &str) -> Option<PlannerDirective> {
    let trimmed = output.trim();
    if let Ok(dir) = serde_json::from_str::<PlannerDirective>(trimmed) {
        if !dir.target.is_empty() {
            return Some(dir);
        }
    }
    for json_str in find_balanced_json_candidates(output) {
        if let Ok(dir) = serde_json::from_str::<PlannerDirective>(json_str) {
            if !dir.target.is_empty() {
                return Some(dir);
            }
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
            let tool = v.get("tool")
                .or_else(|| v.get("name"))
                .or_else(|| v.get("function"))
                .and_then(|val| val.as_str());
            let args = v.get("arguments").and_then(|a| a.as_object());

            let target = v.get("glob")
                .or_else(|| v.get("pattern"))
                .or_else(|| v.get("target"))
                .or_else(|| v.get("path"))
                .or_else(|| v.get("file"))
                .or_else(|| v.get("command"))
                .or_else(|| args.and_then(|a| a.get("glob").or_else(|| a.get("pattern")).or_else(|| a.get("target")).or_else(|| a.get("path")).or_else(|| a.get("file")).or_else(|| a.get("command"))))
                .and_then(|val| val.as_str());

            if let Some(t) = tool {
                if let Some(tgt) = target {
                    let action = v.get("action")
                        .or_else(|| v.get("step"))
                        .or_else(|| v.get("description"))
                        .or_else(|| args.and_then(|a| a.get("action").or_else(|| a.get("step")).or_else(|| a.get("description"))))
                        .and_then(|val| val.as_str())
                        .unwrap_or("Execute tool step");
                    let rules = v.get("rules")
                        .or_else(|| v.get("constraints"))
                        .or_else(|| args.and_then(|a| a.get("rules").or_else(|| a.get("constraints"))))
                        .and_then(|val| val.as_str())
                        .unwrap_or("");
                    return Some(PlannerDirective {
                        tool: t.to_string(),
                        target: tgt.to_string(),
                        action: action.to_string(),
                        rules: rules.to_string(),
                    });
                }
            }
        }
    }
    // XML tool_call fallback (Hermes / Nous agentic dialect)
    if let Some(start) = output.find("<tool_call>") {
        if let Some(end) = output[start..].find("</tool_call>") {
            let block = &output[start..start + end + 12];
            let tool_name = if let Some(fn_start) = block.find("<function=") {
                let rest = &block[fn_start + 10..];
                rest.split('>').next().unwrap_or("").trim().to_string()
            } else {
                String::new()
            };
            let target = if let Some(p_start) = block.find("<parameter=") {
                let rest = &block[p_start..];
                if let Some(val_start) = rest.find('>') {
                    let val_body = &rest[val_start + 1..];
                    if let Some(val_end) = val_body.find("</parameter>") {
                        val_body[..val_end].trim().to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            if !tool_name.is_empty() {
                return Some(PlannerDirective {
                    tool: tool_name,
                    target,
                    action: "Execute direct tool call from Architect".into(),
                    rules: "Atomic execution".into(),
                });
            }
        }
    }
    None
}

/// Construct a scoped prompt for the Generator based on the Planner's directive.
pub fn build_scoped_generator_prompt(
    directive: &PlannerDirective,
    original_prompt: &str,
) -> String {
    let tool_name = if directive.tool.is_empty() { "write_file" } else { &directive.tool };
    let target_name = if directive.target.is_empty() { "target file" } else { &directive.target };
    format!(
        "You are the Generator. You must implement ONLY the following atomic step directed by the Architect:\n\
        - Target: {}\n\
        - Tool: {}\n\
        - Action: {}\n\
        - Constraints: {}\n\n\
        Original User Task:\n\
        {}\n\n\
        CRITICAL DIRECTIVE: Emit ONLY the implementation for target '{}'.\n\
        Emit either the raw code or the structured JSON tool call matching the tool schema:\n\
        {{\"name\": \"{}\", \"arguments\": {{...}}}}\n\
        Do NOT hallucinate execution history or fake tool results. Do NOT touch any other files.",
        target_name, tool_name, directive.action, directive.rules, original_prompt, target_name, tool_name
    )
}

/// Check if a Planner directive is an immediate inspection tool that skips code generation.
pub fn is_immediate_inspection_directive(dir: &PlannerDirective) -> bool {
    let t = dir.tool.to_ascii_lowercase();
    if t == "read_file"
        || t == "read"
        || t == "search_files"
        || t == "search"
        || t == "session_search"
        || t == "clarify"
        || t == "skill_view"
        || t == "skills_list"
        || t == "run_command"
        || t == "terminal"
    {
        return true;
    }
    let a = dir.action.to_ascii_lowercase();
    if (a.contains("read") || a.contains("inspect") || a.contains("examine"))
        && (dir.target.contains('.') || dir.target.contains('/'))
        && !dir.target.contains(' ')
    {
        return true;
    }
    false
}

/// Format an immediate tool call from a Planner directive.
pub fn format_immediate_tool_call(dir: &PlannerDirective) -> String {
    let mut tool_name = dir.tool.clone();
    let target = &dir.target;
    let a = dir.action.to_ascii_lowercase();

    if (tool_name.contains("terminal") || tool_name.is_empty())
        && (a.contains("read") || a.contains("inspect") || a.contains("examine"))
        && (target.contains('.') || target.contains('/'))
    {
        tool_name = "read_file".into();
    }

    let mut args = serde_json::Map::new();
    if tool_name.contains("read") {
        args.insert("path".to_string(), serde_json::Value::String(target.to_string()));
    } else if tool_name.contains("search") {
        args.insert("pattern".to_string(), serde_json::Value::String(target.to_string()));
    } else if tool_name.contains("terminal") || tool_name.contains("run_command") {
        tool_name = "run_command".into();
        args.insert("CommandLine".to_string(), serde_json::Value::String(target.to_string()));
        args.insert("Cwd".to_string(), serde_json::Value::String(".".to_string()));
        args.insert("WaitMsBeforeAsync".to_string(), serde_json::Value::Number(5000.into()));
    } else {
        args.insert("path".to_string(), serde_json::Value::String(target.to_string()));
    }
    serde_json::json!({
        "name": tool_name,
        "arguments": args
    }).to_string()
}

/// Check if generation should be skipped because the Planner emitted an immediate tool call.
pub fn should_skip_generation_for_inspection(state: &mut DebateState) -> bool {
    if let Some(ref dir) = state.planner_directive {
        if is_immediate_inspection_directive(dir) {
            let tool_json = format_immediate_tool_call(dir);
            state.draft = Some(tool_json);
            return true;
        }
    }
    false
}

use crate::council::{CouncilEvent, CouncilRole, PipelineStage, TurnResult};
use crate::council::executor::Executor;
use crate::council::pipeline::DebateState;
use std::time::Instant;

/// Execute the Planner stage if one is configured in the pipeline.
pub fn run_planner_stage<E: Executor>(
    stages: &[PipelineStage],
    executor: &E,
    state: &mut DebateState,
    emit: &dyn Fn(CouncilEvent),
) {
    let (stage, stage_index) = match stages.iter().position(|s| s.role == CouncilRole::Planner) {
        Some(idx) => (&stages[idx], idx),
        None => return,
    };

    let start = Instant::now();
    emit(CouncilEvent::StageStarted {
        stage_index,
        role: stage.role.clone(),
        model_id: stage.model_id.clone(),
        model_name: stage.model_id.clone(),
    });

    let tool_protocol = stage.system_prompt.as_deref().unwrap_or("");
    let prompt = build_planner_prompt(&state.transcript.input_prompt, tool_protocol);

    match executor.execute_stream(stage, &prompt, stage_index, &|event| emit(event.clone())) {
        Ok(output) => {
            let dur = start.elapsed();
            emit(CouncilEvent::StageCompleted {
                stage_index,
                full_text: output.clone(),
                duration_sec: dur.as_secs_f64(),
                tok_per_sec: 0.0,
            });
            if let Some(directive) = parse_planner_directive(&output) {
                state.planner_directive = Some(directive);
            }
            state.transcript.append_turn(TurnResult {
                turn_index: state.transcript.turns.len(),
                role: CouncilRole::Planner,
                model_id: stage.model_id.clone(),
                output,
                duration: dur,
                error: None,
            });
        }
        Err(err) => {
            emit(CouncilEvent::PipelineFailed {
                stage_index,
                error: err.clone(),
            });
            let fallback = state.transcript.config.fallback.clone();
            state.handle_failure(&fallback, stage_index, &err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_planner_prompt_construction() {
        let p = build_planner_prompt("Add auth", "- write_file: write a file");
        assert!(p.contains("Lead Software Architect"));
        assert!(p.contains("Add auth"));
        assert!(p.contains("write_file"));
    }

    #[test]
    fn test_parse_planner_directive_clean_json() {
        let json = r#"{
            "tool": "write_file",
            "target": "core/Cargo.toml",
            "action": "Add keyring dependency",
            "rules": "Atomic edit"
        }"#;
        let parsed = parse_planner_directive(json).expect("should parse");
        assert_eq!(parsed.tool, "write_file");
        assert_eq!(parsed.target, "core/Cargo.toml");
        assert_eq!(parsed.action, "Add keyring dependency");
    }

    #[test]
    fn test_parse_planner_directive_without_rules() {
        let json = r#"{
            "tool": "read_file",
            "target": "PLAN/PHASES/phase35.md",
            "action": "Read the Phase 35 specification"
        }"#;
        let parsed = parse_planner_directive(json).expect("should parse without rules");
        assert_eq!(parsed.tool, "read_file");
        assert_eq!(parsed.target, "PLAN/PHASES/phase35.md");
    }

    #[test]
    fn test_parse_planner_directive_with_surrounding_text() {
        let text = "Here is my architecture plan:\n```json\n{\"tool\": \"write_file\", \"target\": \"src/main.rs\", \"action\": \"Update main\", \"rules\": \"None\"}\n```\nProceed with care.";
        let parsed = parse_planner_directive(text).expect("should parse embedded JSON");
        assert_eq!(parsed.tool, "write_file");
        assert_eq!(parsed.target, "src/main.rs");
    }

    #[test]
    fn test_build_scoped_generator_prompt() {
        let dir = PlannerDirective {
            tool: "write_file".into(),
            target: "core/Cargo.toml".into(),
            action: "Add keyring dependency".into(),
            rules: "Only Cargo.toml".into(),
        };
        let prompt = build_scoped_generator_prompt(&dir, "Add keyring and implement secure store");
        assert!(prompt.contains("core/Cargo.toml"));
        assert!(prompt.contains("Do NOT touch any other files"));
    }

    #[test]
    fn test_immediate_inspection_directive() {
        let dir = PlannerDirective {
            tool: "read_file".into(),
            target: "PLAN/PHASES/phase35.md".into(),
            action: "Read spec".into(),
            rules: "None".into(),
        };
        assert!(is_immediate_inspection_directive(&dir));
        let tool_call = format_immediate_tool_call(&dir);
        assert!(tool_call.contains("read_file"));
        assert!(tool_call.contains("PLAN/PHASES/phase35.md"));
    }

    #[test]
    fn test_parse_planner_directive_nested_args_with_trailing_brace() {
        let text = "Let me start with council mod.rs\n```json\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"core/src/council/mod.rs\"}}}\n```";
        let parsed = parse_planner_directive(text).expect("should parse nested tool call with trailing brace");
        assert_eq!(parsed.tool, "read_file");
        assert_eq!(parsed.target, "core/src/council/mod.rs");
        assert!(is_immediate_inspection_directive(&parsed));
    }

    #[test]
    fn test_planner_prompt_with_execution_history() {
        let p = build_planner_prompt("Execution History & Tool Results:\ncore/Cargo.toml edited", "");
        assert!(p.contains("determine the NEXT unexecuted atomic step"));
        assert!(!p.contains("determine the FIRST immediate atomic step"));
    }

    #[test]
    fn test_parse_audit_verdict_approved() {
        let v = parse_audit_verdict("Critique here\n{\"status\": \"approved\", \"critique\": \"LGTM\"}").unwrap();
        assert!(v.approved);
        assert_eq!(v.critique, "LGTM");
    }

    #[test]
    fn test_parse_audit_verdict_changes_needed() {
        let v = parse_audit_verdict("{\"status\": \"changes_needed\", \"critique\": \"bug\"}").unwrap();
        assert!(!v.approved);
    }
}
