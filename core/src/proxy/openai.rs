use std::sync::{Arc, Mutex};
use tiny_http::{Header, Request, Response};

use super::state::ProxyState;

/// Translate an incoming `POST /v1/responses` request body into a standard
/// OpenAI `chat/completions` payload for the active model server.
///
/// - Converts the Responses API `input` field (string or item array) into
///   standard OpenAI `messages`, preserving supported roles and content.
/// - Remaps incoming model identifiers to SWAI's currently active model.
/// - Strips Responses API–specific fields (`stream_options`, etc.).
pub fn responses_adapter(
    body_bytes: &[u8],
    active_model_id: &str,
) -> Result<serde_json::Value, String> {
    let mut req_obj = serde_json::from_slice::<serde_json::Value>(body_bytes)
        .map_err(|e| format!("invalid JSON in /v1/responses request: {}", e))?;

    // Extract the original model ID
    let _original_model = req_obj
        .get("model")
        .and_then(|m| m.as_str())
        .map(String::from)
        .unwrap_or_else(|| active_model_id.to_string());

    // Remap model ID to SWAI's active model.
    req_obj["model"] = serde_json::Value::String(active_model_id.to_string());

    // Convert Responses API `input` (string or items array) → OpenAI `messages`.
    if let Some(obj) = req_obj.as_object_mut() {
        if let Some(input) = obj.get("input").cloned() {
            let messages = convert_responses_input_to_messages(&input);
            if !messages.is_empty() {
                obj.insert("messages".to_string(), serde_json::Value::Array(messages));
            }
            // Remove the `input` field — backend expects `messages`.
            obj.remove("input");
        }

        // Clean any nested array/object content inside messages so llama-server receives plain string content.
        if let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for msg in messages {
                if let Some(content) = msg.get_mut("content") {
                    if content.is_array() || content.is_object() {
                        let text = extract_text_from_item(content);
                        *content = serde_json::Value::String(text);
                    }
                }
            }
        }

        // Remove Responses API–only fields that llama-server doesn't understand.
        obj.remove("stream_options");
    }

    Ok(req_obj)
}

/// Convert a Responses API `input` value into OpenAI `messages`.
pub fn convert_responses_input_to_messages(input: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();

    match input {
        serde_json::Value::String(s) => {
            if !s.is_empty() {
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": s,
                }));
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(item_obj) = item.as_object() {
                    let item_type = item_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

                    if item_type == "item_reference" {
                        continue;
                    }

                    let role = item_obj
                        .get("role")
                        .and_then(|r| r.as_str())
                        .unwrap_or_else(|| {
                            if item_type == "function_call" || item_type == "tool_call" {
                                "assistant"
                            } else if item_type == "function_call_output"
                                || item_type == "tool_response"
                            {
                                "tool"
                            } else {
                                "user"
                            }
                        });

                    let content_text = if let Some(content) = item_obj.get("content") {
                        extract_text_from_item(content)
                    } else if let Some(text) = item_obj.get("text").and_then(|t| t.as_str()) {
                        text.to_string()
                    } else if let Some(output) = item_obj.get("output").and_then(|o| o.as_str()) {
                        output.to_string()
                    } else {
                        String::new()
                    };

                    messages.push(serde_json::json!({
                        "role": role,
                        "content": content_text,
                    }));
                }
            }
        }
        _ => {}
    }

    messages
}

/// Recursively extract plain text from an item content field.
pub fn extract_text_from_item(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            let mut parts = Vec::new();
            for elem in arr {
                if let Some(s) = elem.as_str() {
                    parts.push(s.to_string());
                } else if let Some(obj) = elem.as_object() {
                    if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                        parts.push(text.to_string());
                    } else if let Some(t) = obj.get("type").and_then(|t| t.as_str()) {
                        if t == "input_text" || t == "text" {
                            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                                parts.push(text.to_string());
                            }
                        }
                    }
                }
            }
            parts.join("\n")
        }
        serde_json::Value::Object(obj) => {
            if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                text.to_string()
            } else {
                serde_json::to_string(content).unwrap_or_default()
            }
        }
        _ => String::new(),
    }
}

/// Normalize OpenAI chat completion payloads from Codex / IDE clients.
pub fn normalize_codex_payload(json_val: &mut serde_json::Value) {
    if let Some(obj) = json_val.as_object_mut() {
        if let Some(input) = obj.get("input").cloned() {
            let converted = convert_responses_input_to_messages(&input);
            if !converted.is_empty() {
                if let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
                    messages.extend(converted);
                } else {
                    obj.insert("messages".to_string(), serde_json::Value::Array(converted));
                }
            }
            obj.remove("input");
        }

        if let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for msg in messages {
                if let Some(content) = msg.get_mut("content") {
                    if content.is_array() || content.is_object() {
                        let text = extract_text_from_item(content);
                        *content = serde_json::Value::String(text);
                    }
                }
            }
        }

        obj.remove("stream_options");
    }
}

/// Handle OpenAI `/v1/models` and `/models` — list all currently active models.
pub fn handle_v1_models(req: Request, state: &Arc<Mutex<ProxyState>>) {
    let proxy_state = match state.lock() {
        Ok(s) => s,
        Err(_) => {
            let _ = req.respond(Response::from_string("Internal error").with_status_code(500));
            return;
        }
    };

    let mut model_entries = Vec::new();
    let now_iso = "2026-08-18T00:00:00Z";

    for model_id in proxy_state.active_models.keys() {
        // Standard OpenAI model ID (for llama.cpp Web UI and standard OpenAI tooling)
        model_entries.push(serde_json::json!({
            "id": model_id,
            "object": "model",
            "type": "model",
            "display_name": model_id,
            "created": 1700000000,
            "owned_by": "swai",
            "created_at": now_iso
        }));
        // Anthropic prefixed model ID (for Claude Code / Hermes Anthropic client)
        model_entries.push(serde_json::json!({
            "id": format!("anthropic.{}", model_id),
            "object": "model",
            "type": "model",
            "display_name": model_id,
            "created": 1700000000,
            "owned_by": "swai",
            "created_at": now_iso
        }));
    }

    if proxy_state.primary_port.is_some() {
        model_entries.push(serde_json::json!({
            "id": "council-pipeline",
            "object": "model",
            "type": "model",
            "display_name": "Council Pipeline",
            "created": 1700000000,
            "owned_by": "swai",
            "created_at": now_iso
        }));
        model_entries.push(serde_json::json!({
            "id": "anthropic.council-pipeline",
            "object": "model",
            "type": "model",
            "display_name": "Council Pipeline",
            "created": 1700000000,
            "owned_by": "swai",
            "created_at": now_iso
        }));
    }

    let first_id = model_entries
        .first()
        .and_then(|e| e.get("id"))
        .cloned()
        .unwrap_or(serde_json::json!(""));
    let last_id = model_entries
        .last()
        .and_then(|e| e.get("id"))
        .cloned()
        .unwrap_or(serde_json::json!(""));

    let response = serde_json::json!({
        "object": "list",
        "data": model_entries,
        "has_more": false,
        "first_id": first_id,
        "last_id": last_id
    });

    let body = serde_json::to_vec(&response).unwrap_or_default();
    let headers = vec![
        Header::from_bytes("content-type", b"application/json").unwrap(),
        Header::from_bytes("content-length", body.len().to_string().as_bytes()).unwrap(),
        Header::from_bytes("access-control-allow-origin", b"*").unwrap(),
    ];

    let response = Response::new(
        tiny_http::StatusCode(200),
        headers,
        std::io::Cursor::new(body),
        None,
        None,
    );

    let _ = req.respond(response);
}
