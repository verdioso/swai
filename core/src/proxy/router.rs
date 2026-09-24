use std::sync::{Arc, Mutex};
use tiny_http::{Header, Request, Response};
use tracing::{debug, error};

use super::anthropic::process_anthropic_payload;
pub use super::council::{
    extract_model_from_body, is_council_model,
    parse_pipeline_header, should_run_council_universal, ProxyExecutor,
};
pub use super::council_sse::{build_council_sse_events, escape_sse_text};
use super::ollama::{
    handle_ollama_chat, handle_ollama_generate, handle_ollama_tags, is_ollama_endpoint,
};
use super::openai::{handle_v1_models, normalize_codex_payload};
use super::state::ProxyState;
use super::streaming::{
    translate_openai_sse_to_responses, ResponsesSource, ResponsesStreamingBody,
};
use reqwest::blocking::Client;

/// Capacity of the broadcast channel used to fan live Council events out to
/// UI subscribers. Large enough that a slow reader never blocks the debate.
pub const BROADCAST_CAPACITY: usize = 256;

/// Handle an incoming proxy request by inspecting state and forwarding.
pub fn handle_proxy_request(mut req: Request, state: Arc<Mutex<ProxyState>>, client: Client) {
    let proxy_state = match state.lock() {
        Ok(s) => s,
        Err(e) => {
            error!("proxy state lock poisoned: {}", e);
            return;
        }
    };
    if proxy_state.primary_port.is_none() && proxy_state.active_models.is_empty() {
        drop(proxy_state);
        let _ = req.respond(error_response(503, "No active model server in SWAI"));
        return;
    }
    if proxy_state.is_loading {
        drop(proxy_state);
        let _ = req.respond(error_response(
            503,
            "Model server is currently starting/restarting",
        ));
        return;
    }

    // Read the request body to inspect for a `model` field.
    let mut request_body = Vec::new();
    let reader = req.as_reader();
    let _ = reader.read_to_end(&mut request_body);
    let request_body_len = request_body.len();

    let target_port = resolve_target_port(&proxy_state, &request_body);
    drop(proxy_state);

    // Council model interception: route to CouncilEngine locally.
    // Active when enable_council is true in proxy state, or when explicitly
    // requesting a council virtual model.
    if let Some(model_id) = extract_model_from_body(&request_body) {
        if is_council_model(&model_id) || should_run_council_universal(&req, &state, &request_body) {
            super::council_route::handle_council_request(
                req,
                &model_id,
                &request_body,
                state.clone(),
                client.clone(),
                target_port,
            );
            return;
        }
    }

    let target_port = match target_port {
        Some(port) => port,
        None => {
            let primary = match state.lock() {
                Ok(s) => s.primary_port,
                Err(_) => None,
            };
            match primary {
                Some(p) => p,
                None => {
                    let _ = req.respond(error_response(503, "No active model server in SWAI"));
                    return;
                }
            }
        }
    };

    let path_and_query = req.url().to_string();
    let path = path_and_query.split('?').next().unwrap_or(&path_and_query);
    if req.method().as_str() == "GET" && (path == "/v1/models" || path == "/models") {
        handle_v1_models(req, &state);
        return;
    }
    if is_ollama_endpoint(&path_and_query) {
        match path_and_query.as_str() {
            "/api/tags" => handle_ollama_tags(req, &state),
            "/api/generate" => handle_ollama_generate(req, state, client, target_port),
            "/api/chat" => handle_ollama_chat(req, state, client, target_port),
            _ => {}
        }
        return;
    }

    // Build headers for the forwarded request, stripping hop-by-hop headers.
    let mut forward_headers = Vec::new();
    for header in req.headers() {
        let field_name = header.field.as_str();
        if is_hop_by_hop_header(field_name.as_ref()) {
            continue;
        }
        let field_bytes = field_name.as_bytes();
        forward_headers.push(
            Header::from_bytes(field_bytes, header.value.as_bytes()).unwrap_or_else(|_| {
                Header::from_bytes(field_bytes, b"").expect("header construction should never fail")
            }),
        );
    }

    let target_url = format!("http://localhost:{}/{}", target_port, path_and_query.trim_start_matches('/'));
    let method_str = req.method().as_str();

    // Process anthropic payloads on the request body BEFORE forwarding
    if path_and_query.contains("/v1/messages") && method_str == "POST" {
        if let Ok(mut j) = serde_json::from_slice::<serde_json::Value>(&request_body) {
            process_anthropic_payload(&mut j, request_body_len, &state, target_port);
            if let Ok(s) = serde_json::to_vec(&j) { request_body = s; }
        }
    }

    let mut builder = client.request(
        method_str.parse().unwrap_or(reqwest::Method::GET),
        &target_url,
    );

    // Set headers on the forwarded request.
    for header in forward_headers.iter() {
        builder = builder.header(header.field.as_str().as_str(), header.value.as_str());
    }

    // Handle body based on HTTP method.
    let response = match method_str {
        "POST" | "PUT" => {
            if request_body.is_empty() {
                builder.send()
            } else {
                builder.body(request_body).send()
            }
        }
        _ => builder.send(),
    };

    // Handle the response from the target model server.
    let response = match response {
        Ok(resp) => resp,
        Err(e) => {
            debug!("failed to forward request to {}: {}", target_url, e);
            let _ = req.respond(error_response(502, "Failed to connect to model server"));
            return;
        }
    };

    let status = response.status().as_u16();
    let response_headers: Vec<Header> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            Header::from_bytes(name.as_str().as_bytes(), value.as_bytes()).unwrap_or_else(|_| {
                Header::from_bytes(name.as_str().as_bytes(), b"")
                    .expect("header construction should never fail")
            })
        })
        .collect();

    // Get the response body for streaming.
    let response_bytes = match response.bytes() {
        Ok(b) => b,
        Err(e) => {
            debug!("failed to read response body: {}", e);
            let _ = req.respond(error_response(502, "Failed to read model response"));
            return;
        }
    };

    let mut processed_bytes = response_bytes.to_vec();

    // Normalize codex payloads if needed.
    if path_and_query.contains("/v1/responses") && method_str == "POST" {
        if let Ok(mut json_val) = serde_json::from_slice::<serde_json::Value>(&processed_bytes) {
            normalize_codex_payload(&mut json_val);
            if let Ok(serialized) = serde_json::to_vec(&json_val) {
                processed_bytes = serialized;
            }
        }
    }

    // Translate OpenAI SSE responses to the Responses API format if needed.
    let is_responses_api = path_and_query.contains("/v1/responses");

    if is_responses_api {
        let body_str = String::from_utf8_lossy(&processed_bytes).to_string();
        let translated_events = translate_openai_sse_to_responses(&body_str, "swai-active-model");
        let streaming_body = ResponsesStreamingBody {
            source: ResponsesSource::Events {
                events: translated_events,
                pos: 0,
            },
            leftover: Vec::new(),
        };

        let tiny_response = Response::new(
            tiny_http::StatusCode(status),
            response_headers,
            Box::new(streaming_body),
            None,
            None,
        );

        if let Err(e) = req.respond(tiny_response) {
            debug!("failed to respond to responses API client: {}", e);
        }
    } else {
        let tiny_response = Response::new(
            tiny_http::StatusCode(status),
            response_headers,
            Box::new(std::io::Cursor::new(processed_bytes)),
            None,
            None,
        );

        if let Err(e) = req.respond(tiny_response) {
            debug!("failed to respond to proxy client: {}", e);
        }
    }
}

/// Resolve the target port for an incoming request by inspecting its JSON body.
pub fn resolve_target_port(state: &ProxyState, body: &[u8]) -> Option<u16> {
    if body.is_empty() {
        return None;
    }
    let has_model_key = body
        .windows(7)
        .any(|w| w.eq_ignore_ascii_case(b"\"model\"") || w.eq_ignore_ascii_case(b"'model'"));
    if !has_model_key {
        return None;
    }

    let model_id = crate::proxy::council::extract_model_from_body(body)?;

    for (id, &port) in &state.active_models {
        if id == &model_id {
            return Some(port);
        }
    }
    None
}

/// Check if a header name is a hop-by-hop header per RFC 7230 §6.1.
pub fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Build an error response with a JSON body.
pub fn error_response(status: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = format!("{{\"error\": \"{}\"}}", message);
    Response::from_data(body.into_bytes())
        .with_status_code(tiny_http::StatusCode(status))
        .with_header(
            Header::from_bytes("content-type", b"application/json").unwrap_or_else(|_| {
                Header::from_bytes("content-type", b"application/json").expect("should never fail")
            }),
        )
}

pub use super::prompt::extract_prompt_from_body;

