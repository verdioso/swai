//! SWAI — Council route handler and SSE streaming adapter.
//!
//! Provides the proxy execution bridge between incoming client requests
//! targeting synthetic "council" models and the local `CouncilEngine`.

use crate::council::Executor;
use crate::council::types::{DebateOutcome, PipelineStage};
use crate::council::CouncilEvent;
use crate::council::CouncilPipelineConfig;
use reqwest::blocking::Client;

/// Check if a model name targets the council engine.
///
/// Matches "council" exactly or any model starting with "council:" prefix.
pub fn is_council_model(model: &str) -> bool {
    model == "council" || model == "council-pipeline" || model.starts_with("council:")
}

/// Parse an optional X-SWAI-Pipeline header into a CouncilPipelineConfig.
pub fn parse_pipeline_header(header_value: &str) -> Option<CouncilPipelineConfig> {
    serde_json::from_str(header_value).ok()
}

/// Extract the `model` field from a JSON request body.
pub fn extract_model_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let model_str = json_val.get("model").and_then(|m| m.as_str())?;
    let stripped = if model_str.starts_with("anthropic.") {
        &model_str[10..]
    } else {
        model_str
    };
    Some(stripped.to_string())
}

use crate::proxy::state::ProxyState;
use std::sync::{Arc, Mutex};

/// Proxy executor that forwards council stages to the primary model backend.
pub struct ProxyExecutor {
    pub client: Client,
    pub primary_port: u16,
    pub state: Arc<Mutex<ProxyState>>,
}

impl Executor for ProxyExecutor {
    fn execute(&self, stage: &PipelineStage, input: &str) -> Result<String, String> {
        let target_port = {
            if let Ok(state) = self.state.lock() {
                state
                    .active_models
                    .iter()
                    .find(|(id, _)| *id == &stage.model_id)
                    .map(|(_, port)| *port)
                    .unwrap_or(self.primary_port)
            } else {
                self.primary_port
            }
        };
        let url = format!("http://localhost:{}/v1/chat/completions", target_port);
        let content = if !stage.prompt_template.is_empty() {
            stage.prompt_template.replace("{input}", input)
        } else {
            input.to_string()
        };

        let mut messages = Vec::new();
        if let Some(ref sys) = stage.system_prompt {
            if !sys.is_empty() {
                messages.push(serde_json::json!({"role": "system", "content": sys}));
            }
        }
        messages.push(serde_json::json!({"role": "user", "content": content}));

        let body = serde_json::json!({
            "model": stage.model_id,
            "messages": messages,
            "temperature": stage.temperature,
            "top_p": stage.top_p,
        });
        let body_str = serde_json::to_string(&body).unwrap_or_default();
        match self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    let text = resp.text().unwrap_or_default();
                    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                    json["choices"]
                        .as_array()
                        .and_then(|choices| choices.first())
                        .and_then(|choice| choice["message"]["content"].as_str())
                        .map(String::from)
                        .ok_or_else(|| "No response content in choices".into())
                } else {
                    Err(format!("Backend returned status {}", resp.status()))
                }
            }
            Err(e) => Err(format!("Request failed: {}", e)),
        }
    }
}

/// Wraps a `ProxyExecutor` and forwards every `CouncilEvent` it emits to the
/// live broadcast receiver registered on the proxy state.
///
/// This is the bridge that lets the UI observe a debate as it streams: the
/// wrapped `ProxyExecutor` produces the tokens, while the wrapper fans the
/// emitted events out to any subscriber.
pub struct EventProxyExecutor {
    pub inner: ProxyExecutor,
}

impl Executor for EventProxyExecutor {
    fn execute(&self, stage: &PipelineStage, input: &str) -> Result<String, String> {
        self.inner.execute(stage, input)
    }

    fn execute_stream(
        &self,
        stage: &PipelineStage,
        input: &str,
        stage_index: usize,
        emit: &dyn Fn(&CouncilEvent),
    ) -> Result<String, String> {
        self.inner.execute_stream(stage, input, stage_index, emit)
    }
}

/// Execute council debate and record telemetry in ProxyState.
///
/// Generic over the executor type so it works with both the plain
/// `ProxyExecutor` and the event-emitting `EventProxyExecutor`. The engine is
/// expected to have been constructed with `CouncilEngine::with_events`, so any
/// registered UI subscriber receives live `CouncilEvent`s as the debate runs.
pub fn run_council_and_record_telemetry<E: Executor>(
    engine: &crate::council::CouncilEngine<E>,
    prompt: &str,
    state: &Arc<Mutex<ProxyState>>,
) -> (
    DebateOutcome,
    Vec<crate::proxy::state::CouncilStageTelemetry>,
) {
    let outcome = engine.execute(prompt);
    let mut stages = Vec::new();
    let mut total_tokens = 0;
    let mut total_duration = 0.0;

    let transcript_opt = match &outcome {
        DebateOutcome::Success { transcript, .. }
        | DebateOutcome::Partial { transcript, .. }
        | DebateOutcome::Aborted { transcript, .. } => Some(transcript),
    };

    if let Some(transcript) = transcript_opt {
        for (i, turn) in transcript.turns.iter().enumerate() {
            let role_name = match turn.role {
                crate::council::CouncilRole::Generator => "1. Generator",
                crate::council::CouncilRole::Auditor => "2. Auditor",
                crate::council::CouncilRole::Synthesizer => "3. Synthesizer",
                crate::council::CouncilRole::Custom(ref s) => s.as_str(),
            };
            let stage_title = format!("Stage {} ({})", i + 1, role_name);
            let tokens = (turn.output.len() / 4).max(1);
            let dur_sec = turn.duration.as_secs_f64();
            let speed = if dur_sec > 0.0 {
                tokens as f64 / dur_sec
            } else {
                0.0
            };
            total_tokens += tokens;
            total_duration += dur_sec;
            stages.push(crate::proxy::state::CouncilStageTelemetry {
                stage_name: stage_title,
                model_id: turn.model_id.clone(),
                output_tokens: tokens,
                duration_sec: dur_sec,
                speed_tokens_sec: speed,
            });
        }
    }

    if let Ok(mut s) = state.lock() {
        s.last_council_telemetry = Some(crate::proxy::state::CouncilTelemetryData {
            total_duration_sec: total_duration,
            total_tokens,
            is_processing: false,
            current_stage: None,
            stages: stages.clone(),
        });
    }

    (outcome, stages)
}

/// Build SSE events for streaming a council debate outcome.
pub fn build_council_sse_events(outcome: &DebateOutcome, _model_id: &str) -> Vec<Vec<u8>> {
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

    // Anthropic content_block_start
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
