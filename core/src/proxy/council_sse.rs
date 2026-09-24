//! SWAI — Council SSE formatting and streaming events for Anthropic and OpenAI protocols.

use crate::council::DebateOutcome;

/// Build SSE events for streaming a council debate outcome.
pub fn build_council_sse_events(
    outcome: &DebateOutcome,
    model_id: &str,
    prompt: &str,
    available_tools: Option<&[serde_json::Value]>,
    is_openai: bool,
) -> Vec<Vec<u8>> {
    let mut events = Vec::new();

    let transcript = match outcome {
        DebateOutcome::Success { transcript, .. } => transcript,
        DebateOutcome::Partial { transcript, .. } => transcript,
        DebateOutcome::Aborted { transcript, .. } => transcript,
    };

    let mut log_dir = dirs::data_local_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    log_dir.push("swai");
    log_dir.push("logs");
    log_dir.push("council_transcripts");
    let _ = std::fs::create_dir_all(&log_dir);

    let log_path = log_dir.join(format!("{}.md", transcript.session_id));
    let mut md = String::new();
    md.push_str(&format!(
        "# Council Debate Transcript: {}\n\n",
        transcript.session_id
    ));
    md.push_str(&format!(
        "## Original Prompt\n```\n{}\n```\n\n",
        transcript.input_prompt
    ));
    for turn in &transcript.turns {
        md.push_str(&format!(
            "## Turn {} - {:?} ({})\n",
            turn.turn_index, turn.role, turn.model_id
        ));
        md.push_str(&format!("Duration: {:.2?}\n", turn.duration));
        if let Some(err) = &turn.error {
            md.push_str(&format!("**Error:** {}\n", err));
        } else {
            md.push_str(&format!("**Output:**\n\n```\n{}\n```\n", turn.output));
        }
        md.push_str("\n---\n\n");
    }
    let _ = std::fs::write(&log_path, md);
    tracing::info!("Council transcript saved to {}", log_path.display());

    // Stream the final response as text deltas (chunked for SSE).
    let final_text = match outcome {
        DebateOutcome::Success { final_response, .. } => final_response.clone(),
        DebateOutcome::Partial {
            fallback_response, ..
        } => {
            format!("Debate partial: {}", fallback_response)
        }
        DebateOutcome::Aborted { reason, .. } => format!("Debate aborted: {}", reason),
    };

    if is_openai {
        if let Some(tool_call) = super::tool_calling::extract_tool_call(&final_text, prompt, available_tools, outcome.target()) {
            let initial_chunk = serde_json::json!({
                "id": "chatcmpl_council",
                "object": "chat.completion.chunk",
                "created": 1725381000,
                "model": model_id,
                "choices": [{
                    "index": 0,
                    "delta": {
                        "role": "assistant",
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_council_01",
                            "type": "function",
                            "function": {
                                "name": tool_call.name,
                                "arguments": ""
                            }
                        }]
                    },
                    "finish_reason": null
                }]
            });
            events.push(format!("data: {}\n\n", initial_chunk).into_bytes());

            let chunk_size = 50;
            let chars: Vec<char> = tool_call.arguments.chars().collect();
            let mut accumulated = String::new();
            let mut prev_len = 0;

            for (i, ch) in chars.iter().enumerate() {
                accumulated.push(*ch);
                if (i + 1) % chunk_size == 0 || i == chars.len() - 1 {
                    let delta = &accumulated[prev_len..];
                    let payload = serde_json::json!({
                        "id": "chatcmpl_council",
                        "object": "chat.completion.chunk",
                        "created": 1725381000,
                        "model": model_id,
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": 0,
                                    "function": {
                                        "arguments": delta
                                    }
                                }]
                            },
                            "finish_reason": null
                        }]
                    });
                    events.push(format!("data: {}\n\n", payload).into_bytes());
                    prev_len = i + 1;
                }
            }

            let final_chunk = serde_json::json!({
                "id": "chatcmpl_council",
                "object": "chat.completion.chunk",
                "created": 1725381000,
                "model": model_id,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "tool_calls"
                }]
            });
            events.push(format!("data: {}\n\n", final_chunk).into_bytes());
            events.push(b"data: [DONE]\n\n".to_vec());
            return events;
        }

        let chunk_size = 50;
        let chars: Vec<char> = final_text.chars().collect();
        let mut accumulated = String::new();
        let mut prev_len = 0;

        for (i, ch) in chars.iter().enumerate() {
            accumulated.push(*ch);
            if (i + 1) % chunk_size == 0 || i == chars.len() - 1 {
                let delta = &accumulated[prev_len..];
                let payload = serde_json::json!({
                    "id": "chatcmpl_council",
                    "object": "chat.completion.chunk",
                    "created": 1725381000,
                    "model": model_id,
                    "choices": [{
                        "index": 0,
                        "delta": { "content": delta },
                        "finish_reason": null
                    }]
                });
                events.push(format!("data: {}\n\n", payload).into_bytes());
                prev_len = i + 1;
            }
        }

        let final_chunk = serde_json::json!({
            "id": "chatcmpl_council",
            "object": "chat.completion.chunk",
            "created": 1725381000,
            "model": model_id,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        events.push(format!("data: {}\n\n", final_chunk).into_bytes());
        events.push(b"data: [DONE]\n\n".to_vec());
        return events;
    }

    if let Some(tool_call) = super::tool_calling::extract_tool_call(&final_text, prompt, available_tools, outcome.target()) {
        events.push(
            format!(
                "event: content_block_start\ndata: {{\"type\": \"content_block_start\", \"index\": 0, \"content_block\": {{\"type\": \"tool_use\", \"id\": \"toolu_council_01\", \"name\": \"{}\", \"input\": {{}}}}}}\n\n",
                tool_call.name
            ).into_bytes()
        );
        events.push(
            format!(
                "event: content_block_delta\ndata: {{\"type\": \"content_block_delta\", \"index\": 0, \"delta\": {{\"type\": \"input_json_delta\", \"partial_json\": \"{}\"}}}}\n\n",
                escape_sse_text(&tool_call.arguments)
            ).into_bytes()
        );
        events.push(
            "event: content_block_stop\ndata: {\"type\": \"content_block_stop\", \"index\": 0}\n\nevent: message_delta\ndata: {\"type\": \"message_delta\", \"delta\": {\"stop_reason\": \"tool_use\", \"stop_sequence\": null}, \"usage\": {\"output_tokens\": 10}}\n\nevent: message_stop\ndata: {\"type\": \"message_stop\"}\n\n".to_string().into_bytes()
        );
        return events;
    }

    events.push(
        "event: content_block_start\ndata: {\"type\": \"content_block_start\", \"index\": 0, \"content_block\": {\"type\": \"text\", \"text\": \"\"}}\n\n".to_string().into_bytes()
    );

    let chunk_size = 50;
    let chars: Vec<char> = final_text.chars().collect();
    let mut accumulated = String::new();
    let mut prev_len = 0;

    for (i, ch) in chars.iter().enumerate() {
        accumulated.push(*ch);
        if (i + 1) % chunk_size == 0 || i == chars.len() - 1 {
            let delta = &accumulated[prev_len..];
            let delta_payload = serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": delta
                }
            });
            events.push(
                format!("event: content_block_delta\ndata: {}\n\n", delta_payload).into_bytes(),
            );
            prev_len = i + 1;
        }
    }

    // Anthropic content_block_stop & message_delta & message_stop
    events.push(
        "event: content_block_stop\ndata: {\"type\": \"content_block_stop\", \"index\": 0}\n\nevent: message_delta\ndata: {\"type\": \"message_delta\", \"delta\": {\"stop_reason\": \"end_turn\", \"stop_sequence\": null}, \"usage\": {\"output_tokens\": 10}}\n\nevent: message_stop\ndata: {\"type\": \"message_stop\"}\n\n".to_string().into_bytes()
    );
    events
}

/// Escape special characters in SSE text data.
pub fn escape_sse_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
