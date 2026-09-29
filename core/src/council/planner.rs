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
    let mut cleaned = output.to_string();
    if let Some(start) = cleaned.find("<think>") {
        if let Some(end) = cleaned.find("</think>") {
            cleaned = format!("{}{}", &cleaned[..start], &cleaned[end + 8..]);
        }
    }
    if let Some(start) = cleaned.find("<thinking>") {
        if let Some(end) = cleaned.find("</thinking>") {
            cleaned = format!("{}{}", &cleaned[..start], &cleaned[end + 11..]);
        }
    }

    for json_str in find_balanced_json_candidates(&cleaned) {
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
        Emit EXACTLY ONE structured JSON tool call matching the tool schema:\n\
        {{\"name\": \"{}\", \"arguments\": {{...}}}}\n\
        Do NOT wrap the JSON in markdown fences. Do NOT emit any conversational text or raw code outside the JSON object.\n\
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
        || t == "list_dir"
        || t == "ls"
        || t == "explore"
    {
        return true;
    }
    if t == "bash"
        || t == "bash_command"
        || t == "run_command"
        || t == "terminal"
        || t == "execute_command"
    {
        let cmd = dir.target.to_ascii_lowercase();
        // Mutating commands are write directives, not inspections
        if cmd.starts_with("mkdir")
            || cmd.contains(" mkdir ")
            || cmd.starts_with("rm")
            || cmd.contains(" rm ")
            || cmd.starts_with("mv")
            || cmd.contains(" mv ")
            || cmd.starts_with("cp")
            || cmd.contains(" cp ")
            || cmd.starts_with("touch")
            || cmd.contains(" touch ")
            || cmd.starts_with("cargo")
            || cmd.contains(" cargo ")
            || cmd.starts_with("git commit")
            || cmd.starts_with("git push")
            || cmd.starts_with("git add")
        {
            return false;
        }
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


pub fn format_immediate_tool_call(dir: &PlannerDirective) -> String {
    crate::proxy::tool_calling::format_immediate_tool_call(dir, None)
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
    let catalog = crate::council::tools::local_tool_catalog();
    let combined = format!("{}\n\n{}", tool_protocol, catalog);
    let prompt = build_planner_prompt(&state.transcript.input_prompt, &combined);

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

