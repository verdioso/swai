use super::session_tracker::SessionTracker;
use super::state::ProxyState;
use std::sync::{Arc, Mutex};

// Thread-local session tracker. Each proxy thread gets its own tracker instance
// to detect loops without cross-thread locking overhead.
thread_local! {
    static SESSION_TRACKER: std::cell::RefCell<SessionTracker> = std::cell::RefCell::new(SessionTracker::new());
}

/// Check if a text message is a synthetic client prompt (recap, reminder) that should be skipped.
pub fn is_synthetic_prompt(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("<system-reminder>")
        || t.starts_with("<system_reminder>")
        || t.starts_with("The user stepped away")
        || t.starts_with("As you answer the user")
        || t.is_empty()
}

/// Process Anthropic `/v1/messages` payloads:
/// - Remap model ID to claude-sonnet-4-5
/// - Run loop detection and inject anti-loop directives when needed
/// - Perform context compaction with model-adaptive budgets
/// - Persist session checkpoints with cumulative deduplication
pub fn process_anthropic_payload(
    json_val: &mut serde_json::Value,
    request_body_len: usize,
    state: &Arc<Mutex<ProxyState>>,
    target_port: u16,
) {
    // Sanitize non-first system messages to prevent llama-server Jinja 500 crashes
    sanitize_messages_for_jinja(json_val);

    let mut model_id = String::new();
    if let Some(obj) = json_val.as_object_mut() {
        if let Some(m) = obj.get("model").and_then(|v| v.as_str()) {
            model_id = m.to_string();
        }
        // Remap model field so llama-server doesn't reject custom model IDs
        obj.insert(
            "model".to_string(),
            serde_json::Value::String("claude-sonnet-4-5".to_string()),
        );
    }

    // Look up the active model's context window size for dynamic budget scaling
    let ctx_size = state
        .lock()
        .ok()
        .map(|s| s.ctx_size_for_port(target_port))
        .unwrap_or(65_536);

    // Read the user-configurable compaction threshold from proxy state
    let threshold_pct = state
        .lock()
        .ok()
        .map(|s| s.compaction_threshold_pct)
        .unwrap_or(crate::compaction::DEFAULT_THRESHOLD_PCT);

    let budget =
        crate::compaction::ContextBudget::from_ctx_size_and_threshold(ctx_size, threshold_pct);

    // Check if checkpointing is enabled. When disabled, bypass loop detection
    // and checkpoint persistence entirely so the proxy acts as a transparent
    // router with zero interception overhead.
    let checkpointing_enabled = state.lock().map(|s| s.enable_checkpointing).unwrap_or(true);

    let loop_directive: Option<String> = if checkpointing_enabled {
        // --- Loop Detection (Phase A) ---
        // Record the current turn and check if the model is stuck in a planning loop.
        if let Some(messages_arr) = json_val.get("messages").and_then(|m| m.as_array()) {
            let messages_clone = messages_arr.clone();
            SESSION_TRACKER.with(|tracker| {
                let mut tracker = tracker.borrow_mut();
                tracker.record_turn(&messages_clone);
                tracker.check_interventions()
            })
        } else {
            None
        }
    } else {
        None
    };
    if let Some(directive) = loop_directive {
        inject_loop_directive(json_val, &directive);
    }

    // If checkpointing is disabled, we skip compaction entirely to act as a transparent router.
    if !checkpointing_enabled {
        return;
    }

    // --- Compaction (Phase B+C) ---
    // Use dynamic trigger threshold from the model's context budget
    let trigger_threshold = budget.compaction_trigger_chars * 90 / 100;
    let trigger_msg_count = if ctx_size <= 65_536 { 12 } else { 20 };
    let trigger_msg_size = trigger_threshold * 50 / 100;

    if let Some(messages_arr) = json_val.get("messages").and_then(|m| m.as_array()).cloned() {
        if request_body_len > trigger_threshold
            || (messages_arr.len() > trigger_msg_count && request_body_len > trigger_msg_size)
        {
            // Extract initial user prompt objective (skipping any synthetic client blocks)
            let initial_objective: Option<String> = extract_initial_objective(&messages_arr);

            let compaction_cfg = crate::compaction::CompactionConfig::default();
            let (summary_lines, remaining) = crate::compaction::compact_messages_with_budget(
                &messages_arr,
                &compaction_cfg,
                Some(&budget),
            );

            // 1. Always update messages array with the compacted/truncated remaining messages
            if let Some(obj) = json_val.as_object_mut() {
                obj.insert("messages".to_string(), serde_json::Value::Array(remaining));
            }

            // 2. If messages were dropped and checkpointing is enabled, update
            //    session checkpoint with deduplication. When checkpointing is
            //    disabled, skip writing to disk entirely.
            if checkpointing_enabled && !summary_lines.is_empty() {
                persist_checkpoint(
                    json_val,
                    summary_lines,
                    initial_objective.as_deref(),
                    &model_id,
                    state,
                    target_port,
                );
            }
        }
    }
}

/// Inject a loop-breaking directive as a user message at the end of the messages array.
fn inject_loop_directive(payload: &mut serde_json::Value, directive: &str) {
    let messages = match payload.get_mut("messages") {
        Some(serde_json::Value::Array(msgs)) => msgs,
        _ => return,
    };

    // Only inject if the last message is from the assistant (to maintain valid turn alternation)
    let last_is_assistant = messages
        .last()
        .and_then(|m| m.get("role").and_then(|r| r.as_str()))
        .map(|r| r == "assistant")
        .unwrap_or(false);

    if last_is_assistant {
        messages.push(serde_json::json!({
            "role": "user",
            "content": directive,
        }));
    }
}

/// Extract the user's initial objective from the first non-synthetic user message.
fn extract_initial_objective(messages: &[serde_json::Value]) -> Option<String> {
    messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .find_map(|m| {
            if let Some(arr) = m.get("content").and_then(|c| c.as_array()) {
                for b in arr {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(t) = b.get("text").and_then(|s| s.as_str()) {
                            let trimmed = t.trim();
                            if !is_synthetic_prompt(trimmed) {
                                return Some(trimmed.chars().take(300).collect());
                            }
                        }
                    }
                }
                None
            } else if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
                let trimmed = s.trim();
                if let Some(end_tag) = trimmed.find("</system-reminder>") {
                    let remainder = trimmed[end_tag + "</system-reminder>".len()..].trim();
                    if !is_synthetic_prompt(remainder) {
                        return Some(remainder.chars().take(300).collect());
                    }
                }
                if !is_synthetic_prompt(trimmed) {
                    Some(trimmed.chars().take(300).collect())
                } else {
                    None
                }
            } else {
                None
            }
        })
}

/// Persist compaction summary to disk checkpoint file with per-line deduplication.
fn persist_checkpoint(
    json_val: &mut serde_json::Value,
    summary_lines: Vec<String>,
    initial_objective: Option<&str>,
    model_id: &str,
    state: &Arc<Mutex<ProxyState>>,
    target_port: u16,
) {
    let session_name = state
        .lock()
        .ok()
        .and_then(|s| {
            s.active_models
                .iter()
                .find(|(_, &p)| p == target_port)
                .map(|(id, _)| id.clone())
        })
        .unwrap_or_else(|| {
            if !model_id.is_empty() {
                model_id.to_string()
            } else {
                "swai-session".to_string()
            }
        });

    let writer = crate::checkpoint::CheckpointWriter::new(&session_name).ok();

    // Load existing session checkpoint to check for duplicates and accumulate entries
    let mut session = writer
        .as_ref()
        .and_then(|w| w.load_session())
        .unwrap_or_else(|| crate::checkpoint::SessionCheckpoint::new(session_name.clone()));

    if let Some(obj) = initial_objective {
        if session.initial_objective.is_none() {
            session.set_initial_objective(obj.to_string());
        }
    }

    // Per-line deduplication across all existing checkpoint entries
    let mut existing_lines = std::collections::HashSet::new();
    for entry in &session.entries {
        for line in &entry.summary_lines {
            existing_lines.insert(line.clone());
        }
    }

    // Filter out any lines that have already been recorded in previous checkpoints
    let new_unique_lines: Vec<String> = summary_lines
        .into_iter()
        .filter(|l| !existing_lines.contains(l))
        .collect();

    if !new_unique_lines.is_empty() {
        let next_idx = session.entries.len() + 1;
        let entry = crate::checkpoint::CheckpointEntry {
            index: next_idx,
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary_lines: new_unique_lines,
        };
        if let Some(ref w) = writer {
            let _ = w.write_entry_with_objective(&entry, initial_objective);
        }
        session.entries.push(entry);
    }

    if let Some(checkpoint_text) = session.format_for_injection() {
        crate::compaction::inject_checkpoint_into_payload(json_val, &checkpoint_text);
    }
}

/// Sanitize messages array to prevent Jinja template crashes like
/// `System message must be at the beginning`.
///
/// If any `role: "system"` message is present after index 0 in `messages`,
/// it converts its role to `"user"` so `llama-server`'s strict Jinja template
/// doesn't reject the payload with an HTTP 500 error.
pub fn sanitize_messages_for_jinja(json_val: &mut serde_json::Value) {
    if let Some(messages) = json_val.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for (i, msg) in messages.iter_mut().enumerate() {
            if i > 0 {
                if let Some(role) = msg.get_mut("role") {
                    if role == "system" {
                        *role = serde_json::Value::String("user".to_string());
                    }
                }
            }
        }
    }
}
