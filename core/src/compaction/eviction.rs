use super::budget::ContextBudget;
use super::extractor::build_eviction_units;
use super::extractor::serialize_dropped_slice;
use super::types::CompactionConfig;
use serde_json::Value;

pub fn compact_messages_anthropic(
    messages: &[Value],
    config: &CompactionConfig,
) -> (Vec<String>, Vec<Value>) {
    compact_messages_with_budget(messages, config, None)
}

pub fn compact_messages_with_budget(
    messages: &[Value],
    config: &CompactionConfig,
    budget: Option<&ContextBudget>,
) -> (Vec<String>, Vec<Value>) {
    if !config.enabled || messages.is_empty() {
        return (Vec::new(), messages.to_vec());
    }

    // Build budget: prefer explicit budget, then derive from config.max_tokens, then use 64k default
    let config_budget;
    let budget = if let Some(b) = budget {
        b
    } else if config.max_tokens > 0 && config.max_tokens < 100_000 {
        // Legacy path: config.max_tokens was explicitly set (not the 100k default).
        // Derive a budget where max_history_chars matches the old behavior.
        config_budget = ContextBudget {
            ctx_tokens: config.max_tokens,
            max_history_chars: if config.max_tokens < 1000 {
                config.max_tokens
            } else {
                config.max_tokens.saturating_mul(4).min(75_000)
            },
            compaction_trigger_chars: config.max_tokens.saturating_mul(4),
            tool_result_max_chars: 8_000,
            recent_turns_keep: 3,
            file_reread_compress_threshold: 2,
        };
        &config_budget
    } else {
        config_budget = ContextBudget::default();
        &config_budget
    };

    // Use dynamic budget from the model's context window size
    let max_budget_chars = budget.max_history_chars;
    let tool_truncation_limit = budget.tool_result_max_chars;
    let recent_turns_protect = budget.recent_turns_keep;

    let msg_len = |m: &Value| -> usize { serde_json::to_string(m).map(|s| s.len()).unwrap_or(0) };

    let mut total_chars: usize = messages.iter().map(msg_len).sum();

    // Collect all files that were edited anywhere in the conversation
    let mut edited_files = std::collections::HashSet::new();
    for msg in messages {
        if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                        match name {
                            "Edit"
                            | "edit"
                            | "ReplaceFileContent"
                            | "replace_file_content"
                            | "multi_replace_file_content"
                            | "Write"
                            | "write"
                            | "write_to_file"
                            | "WriteToFile" => {
                                if let Some(input) = block.get("input") {
                                    if let Some(path) = input
                                        .get("file_path")
                                        .or_else(|| input.get("path"))
                                        .or_else(|| input.get("TargetFile"))
                                        .and_then(|p| p.as_str())
                                    {
                                        edited_files.insert(path.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Helper: checks if a message is associated with an edited file
    let is_edited_file_msg = |m: &Value| -> bool {
        if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    if let Some(input) = block.get("input") {
                        if let Some(path) = input
                            .get("file_path")
                            .or_else(|| input.get("path"))
                            .or_else(|| input.get("TargetFile"))
                            .and_then(|p| p.as_str())
                        {
                            if edited_files.contains(path) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    };

    // Helper: checks if a message read a plan/spec file that MUST be protected
    let is_plan_file_msg = |m: &Value| -> bool {
        if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                        if matches!(name, "Read" | "read" | "view_file" | "ViewFile") {
                            if let Some(input) = block.get("input") {
                                if let Some(path) = input
                                    .get("file_path")
                                    .or_else(|| input.get("path"))
                                    .or_else(|| input.get("TargetFile"))
                                    .and_then(|p| p.as_str())
                                {
                                    // Specifically protect plan and spec documents across all phases
                                    let is_phase_file = path
                                        .rsplit('/')
                                        .next()
                                        .map(|f| f.starts_with("PHASE") && f.ends_with(".md"))
                                        .unwrap_or(false);
                                    if path.contains("PLAN/")
                                        || path.contains("/spec")
                                        || is_phase_file
                                        || path.ends_with("MASTER_PLAN.md")
                                        || path.ends_with("HEADROOM.md")
                                    {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        false
    };

    // Helper: checks if a message contains substantive assistant discussion (>300 chars)
    let is_substantive_discussion_msg = |m: &Value| -> bool {
        if m.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            return false;
        }
        if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        if text.trim().len() > 300 {
                            return true;
                        }
                    }
                }
            }
        }
        false
    };

    // Helper: checks if a unit contains an edited file, critical plan file, or substantive discussion
    let is_critical_unit = |unit: &(usize, usize)| -> bool {
        for idx in unit.0..=unit.1 {
            if is_edited_file_msg(&messages[idx])
                || is_plan_file_msg(&messages[idx])
                || is_substantive_discussion_msg(&messages[idx])
            {
                return true;
            }
        }
        false
    };

    let units = build_eviction_units(messages);
    let mut dropped_indices = std::collections::HashSet::new();

    // Preserve unit 0 (the original user prompt/goal) if messages[0] is a user text message
    // and we have more than 2 units to evict from.
    let is_initial_user_prompt = messages
        .first()
        .map(|m| {
            m.get("role").and_then(|r| r.as_str()) == Some("user")
                && !m
                    .get("content")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        arr.iter()
                            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                    })
                    .unwrap_or(false)
        })
        .unwrap_or(false);

    let start_u_idx = if is_initial_user_prompt && units.len() > 2 {
        1
    } else {
        0
    };
    let max_droppable_units = if units.len() > recent_turns_protect {
        units.len() - recent_turns_protect
    } else {
        0
    };

    // Pass 1: Evict non-critical units (unedited files, exploratory commands, general turns)
    if total_chars > max_budget_chars {
        for u_idx in start_u_idx..max_droppable_units {
            if total_chars <= max_budget_chars {
                break;
            }
            let unit = &units[u_idx];
            if !is_critical_unit(unit) {
                for idx in unit.0..=unit.1 {
                    dropped_indices.insert(idx);
                    total_chars = total_chars.saturating_sub(msg_len(&messages[idx]));
                }
            }
        }
    }

    // Pass 2: If budget is STILL exceeded, drop oldest non-plan units (even if edited), but NEVER drop plan files
    if total_chars > max_budget_chars {
        for u_idx in start_u_idx..max_droppable_units {
            if total_chars <= max_budget_chars {
                break;
            }
            let unit = &units[u_idx];
            let contains_plan = (unit.0..=unit.1).any(|idx| is_plan_file_msg(&messages[idx]));
            if !dropped_indices.contains(&unit.0) && !contains_plan {
                for idx in unit.0..=unit.1 {
                    dropped_indices.insert(idx);
                    total_chars = total_chars.saturating_sub(msg_len(&messages[idx]));
                }
            }
        }
    }

    let mut dropped = Vec::new();
    let mut remaining = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        if dropped_indices.contains(&idx) {
            dropped.push(msg.clone());
        } else {
            remaining.push(msg.clone());
        }
    }

    let summary_lines = if !dropped.is_empty() {
        serialize_dropped_slice(&dropped)
    } else {
        Vec::new()
    };

    // Step 2: In remaining messages, truncate any large tool_result that exceeds the dynamic budget limit
    for msg in &mut remaining {
        if let Some(arr) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    if let Some(content_val) = block.get_mut("content") {
                        match content_val {
                            Value::String(s) => {
                                if s.len() > tool_truncation_limit {
                                    *s = truncate_head_tail(s, tool_truncation_limit);
                                }
                            }
                            Value::Array(blocks) => {
                                for inner in blocks.iter_mut() {
                                    if let Some(text) = inner.get("text").and_then(|t| t.as_str()) {
                                        if text.len() > tool_truncation_limit {
                                            let truncated =
                                                truncate_head_tail(text, tool_truncation_limit);
                                            if let Some(obj) = inner.as_object_mut() {
                                                obj.insert(
                                                    "text".to_string(),
                                                    Value::String(truncated),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    (summary_lines, remaining)
}

fn truncate_head_tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let keep_each = limit / 2;
    let head: String = text.chars().take(keep_each).collect();
    let tail_chars: Vec<char> = text.chars().rev().take(keep_each).collect();
    let tail: String = tail_chars.into_iter().rev().collect();
    format!("{}\n... [truncated] ...\n{}", head, tail)
}

/// Inject a checkpoint block into an Anthropic Messages API JSON payload.
///
/// Inserts a synthetic `user` message at index 0 (or immediately after the
/// first system message) containing the formatted checkpoint text. This gives
/// the model context about earlier work that was compacted out of the
/// conversation history.
///
/// Returns whether the checkpoint was successfully injected.
pub fn inject_checkpoint_into_payload(payload: &mut Value, checkpoint_text: &str) -> bool {
    // Find the "messages" array in the payload.
    let messages = match payload.get_mut("messages") {
        Some(Value::Array(messages)) => messages,
        _ => return false,
    };

    if messages.is_empty() {
        return false;
    }

    // Build the checkpoint message.
    let checkpoint_msg = serde_json::json!({
        "role": "user",
        "content": checkpoint_text,
    });

    // Find the last system message index to insert after.
    let insert_at = messages
        .iter()
        .rev()
        .position(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .map(|rev_pos| messages.len() - rev_pos)
        .unwrap_or(0);

    // Insert the checkpoint message at the appropriate position.
    messages.insert(insert_at, checkpoint_msg);

    true
}
