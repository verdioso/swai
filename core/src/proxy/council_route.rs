//! SWAI — Council proxy route handler.

use std::sync::{Arc, Mutex};
use tiny_http::{Header, Request, Response};
use reqwest::blocking::Client;
use crate::council::{CouncilEngine, DebateOutcome, CouncilPipelineConfig};
use super::council::{
    parse_pipeline_header, run_council_and_record_telemetry,
    EventProxyExecutor, ProxyExecutor,
};
use super::council_sse::build_council_sse_events;
use super::router::{error_response, extract_prompt_from_body};
use super::state::ProxyState;
use super::streaming::{ResponsesSource, ResponsesStreamingBody};

/// Broadcast capacity for council streaming.
pub const BROADCAST_CAPACITY: usize = 256;

fn resolve_pipeline_config(req: &Request, state: &Arc<Mutex<ProxyState>>) -> CouncilPipelineConfig {
    // 1. Check HTTP header
    for header in req.headers() {
        if header.field.as_str().as_str().eq_ignore_ascii_case("x-swai-pipeline") {
            if let Some(config) = parse_pipeline_header(header.value.as_str()) {
                if !config.stages.is_empty() {
                    return config;
                }
            }
        }
    }

    // 2. Check ~/.config/swai/config.toml
    if let Ok(home) = std::env::var("HOME") {
        let mut path = std::path::PathBuf::from(home);
        path.push(".config");
        path.push("swai");
        path.push("config.toml");
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(parsed) = toml::from_str::<toml::Value>(&content) {
                if let Some(council_val) = parsed.get("council") {
                    if let Ok(config) = council_val.clone().try_into::<CouncilPipelineConfig>() {
                        if !config.stages.is_empty() {
                            return config;
                        }
                    }
                }
            }
        }
    }

    // 3. Fallback: synthesize default pipeline from active running models
    if let Ok(s) = state.lock() {
        if !s.active_models.is_empty() {
            let active_keys: Vec<String> = s.active_models.keys().cloned().collect();
            let gen = active_keys.first().cloned().unwrap_or_else(|| "default".into());
            let audit = active_keys.get(1).cloned().unwrap_or_else(|| gen.clone());
            return CouncilPipelineConfig {
                mode: crate::council::CouncilMode::Sequential,
                fallback: crate::council::types::FallbackAction::Skip,
                stages: vec![
                    crate::council::PipelineStage {
                        model_id: gen.clone(),
                        role: crate::council::CouncilRole::Generator,
                        prompt_template: String::new(),
                        temperature: 0.7,
                        top_p: 0.9,
                        system_prompt: None,
                    },
                    crate::council::PipelineStage {
                        model_id: audit,
                        role: crate::council::CouncilRole::Auditor,
                        prompt_template: String::new(),
                        temperature: 0.7,
                        top_p: 0.9,
                        system_prompt: None,
                    },
                    crate::council::PipelineStage {
                        model_id: gen,
                        role: crate::council::CouncilRole::Synthesizer,
                        prompt_template: String::new(),
                        temperature: 0.7,
                        top_p: 0.9,
                        system_prompt: None,
                    },
                ],
                role_overrides: std::collections::HashMap::new(),
            };
        }
    }

    CouncilPipelineConfig::default()
}

/// Handle incoming request targeting a council model.
pub fn handle_council_request(
    req: Request,
    model_id: &str,
    request_body: &[u8],
    state: Arc<Mutex<ProxyState>>,
    client: Client,
    target_port: Option<u16>,
) {
    let mut pipeline_config = resolve_pipeline_config(&req, &state);
    
    // Inject CLI system instructions (tools, OS rules, skills, etc.)
    if let Some(cli_system) = super::prompt::extract_system_prompt_from_body(request_body) {
        if let Some(planner) = pipeline_config.stages.iter_mut().find(|s| s.role == crate::council::CouncilRole::Planner) {
            let existing = planner.system_prompt.clone().unwrap_or_default();
            planner.system_prompt = Some(format!("{}\n\n=== CLI ENVIRONMENT / CAPABILITIES ===\n{}", existing, cli_system));
        }
    }

    let available_tools = super::tool_calling::extract_openai_tools(request_body);
    if let Some(ref tools) = available_tools {
        super::tool_calling::inject_tool_discipline_into_config(&mut pipeline_config, tools);
    }

    let primary_port = match state.lock() {
        Ok(s) => s.primary_port.or(target_port),
        Err(_) => target_port,
    };
    let primary_port = match primary_port {
        Some(p) => p,
        None => {
            let _ = req.respond(error_response(503, "No active model server for council execution"));
            return;
        }
    };

    let prompt = extract_prompt_from_body(request_body).unwrap_or_else(|| "No prompt".into());
    let inner = ProxyExecutor {
        client: client.clone(),
        primary_port,
        state: state.clone(),
    };
    let (tx, rx) = tokio::sync::broadcast::channel(BROADCAST_CAPACITY);
    let executor = EventProxyExecutor { inner };
    let engine = CouncilEngine::with_events(pipeline_config, executor, tx);

    let is_stream = serde_json::from_slice::<serde_json::Value>(request_body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(true);

    let state_for_council = state.clone();
    if let Ok(mut s) = state_for_council.lock() {
        s.last_council_telemetry = Some(crate::proxy::state::CouncilTelemetryData {
            is_processing: true,
            current_stage: Some("1. Generating".to_string()),
            ..Default::default()
        });
        s.set_events_receiver(Some(Arc::new(Mutex::new(rx))));
    }

    let is_openai = req.url().contains("/chat/completions") || req.url().contains("/completions");

    if !is_stream {
        let (outcome, _) = run_council_and_record_telemetry(&engine, &prompt, &state_for_council);
        let final_text = match &outcome {
            DebateOutcome::Success { final_response, .. } => final_response.clone(),
            DebateOutcome::Partial { fallback_response, .. } => fallback_response.clone(),
            DebateOutcome::Aborted { reason, .. } => reason.clone(),
        };

        let json_resp = if is_openai {
            if let Some(tool_call) = super::tool_calling::extract_tool_call(&final_text, &prompt, available_tools.as_deref(), outcome.target()) {
                serde_json::json!({
                    "id": "chatcmpl_council",
                    "object": "chat.completion",
                    "created": 1725381000,
                    "model": model_id,
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_council_01",
                                "type": "function",
                                "function": {
                                    "name": tool_call.name,
                                    "arguments": tool_call.arguments
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": { "prompt_tokens": 50, "completion_tokens": (final_text.len() / 4).max(1), "total_tokens": 50 + (final_text.len() / 4).max(1) }
                })
            } else {
                serde_json::json!({
                    "id": "chatcmpl_council",
                    "object": "chat.completion",
                    "created": 1725381000,
                    "model": model_id,
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": final_text
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 50, "completion_tokens": (final_text.len() / 4).max(1), "total_tokens": 50 + (final_text.len() / 4).max(1) }
                })
            }
        } else if let Some(tool_call) = super::tool_calling::extract_tool_call(&final_text, &prompt, available_tools.as_deref(), outcome.target()) {
            let input_val: serde_json::Value = serde_json::from_str(&tool_call.arguments).unwrap_or(serde_json::json!({}));
            serde_json::json!({
                "id": "msg_council",
                "type": "message",
                "role": "assistant",
                "model": model_id,
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_council_01",
                    "name": tool_call.name,
                    "input": input_val
                }],
                "stop_reason": "tool_use",
                "stop_sequence": null,
                "usage": { "input_tokens": 50, "output_tokens": (final_text.len() / 4).max(1) }
            })
        } else {
            serde_json::json!({
                "id": "msg_council",
                "type": "message",
                "role": "assistant",
                "model": model_id,
                "content": [{ "type": "text", "text": final_text }],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": { "input_tokens": 50, "output_tokens": (final_text.len() / 4).max(1) }
            })
        };

        let body = serde_json::to_vec(&json_resp).unwrap_or_default();
        let mut resp = Response::from_data(body);
        if let Ok(h) = Header::from_bytes(b"content-type", b"application/json") {
            resp.add_header(h);
        }
        let _ = req.respond(resp);
        return;
    }

    let (sse_tx, sse_rx) = std::sync::mpsc::channel();
    if !is_openai {
        let _ = sse_tx.send(format!(
            "event: message_start\ndata: {{\"type\": \"message_start\", \"message\": {{\"id\": \"msg_council\", \"type\": \"message\", \"role\": \"assistant\", \"content\": [], \"model\": \"{}\", \"stop_reason\": null, \"stop_sequence\": null, \"usage\": {{\"input_tokens\": 0, \"output_tokens\": 0}}}}}}\n\n",
            model_id
        ).into_bytes());
    }

    let sse_tx_for_heartbeat = sse_tx.clone();
    let heartbeat_active = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let heartbeat_flag = heartbeat_active.clone();
    std::thread::spawn(move || {
        let ping_payload = if is_openai {
            b": ping\n\n".to_vec()
        } else {
            (": keepalive ".to_string() + &".".repeat(4096) + "\nevent: ping\ndata: {\"type\": \"ping\"}\n\n").into_bytes()
        };
        while heartbeat_flag.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(3));
            if !heartbeat_flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if sse_tx_for_heartbeat.send(ping_payload.clone()).is_err() {
                break;
            }
        }
    });

    let model_str = model_id.to_string();
    let prompt_clone = prompt.clone();
    let tools_clone = available_tools.clone();
    std::thread::spawn(move || {
        let (outcome, _) = run_council_and_record_telemetry(&engine, &prompt_clone, &state_for_council);
        heartbeat_active.store(false, std::sync::atomic::Ordering::Relaxed);
        let sse_events = build_council_sse_events(&outcome, &model_str, &prompt_clone, tools_clone.as_deref(), is_openai);
        for event in sse_events {
            if sse_tx.send(event).is_err() {
                break;
            }
        }
    });

    let streaming_body = ResponsesStreamingBody {
        source: ResponsesSource::Receiver { receiver: sse_rx },
        leftover: Vec::new(),
    };
    let response_headers = vec![
        Header::from_bytes(b"content-type", b"text/event-stream").unwrap_or_else(|_| {
            Header::from_bytes("content-type", b"text/event-stream").expect("valid header")
        }),
        Header::from_bytes(b"cache-control", b"no-cache").unwrap_or_else(|_| {
            Header::from_bytes("cache-control", b"no-cache").expect("valid header")
        }),
    ];
    let response = Response::new(
        tiny_http::StatusCode(200),
        response_headers,
        streaming_body,
        None,
        None,
    );
    let _ = req.respond(response);
}
