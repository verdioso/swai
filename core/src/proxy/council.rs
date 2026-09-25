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

/// Maximum token generation ceiling per Council stage (3072 tokens).
const COUNCIL_STAGE_MAX_TOKENS: u32 = 3072;

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
            "max_tokens": COUNCIL_STAGE_MAX_TOKENS,
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
                        .and_then(|choice| {
                            choice["message"]["content"]
                                .as_str()
                                .or_else(|| choice["message"]["reasoning_content"].as_str())
                        })
                        .map(String::from)
                        .ok_or_else(|| "No response content in choices".into())
                } else {
                    Err(format!("Backend returned status {}", resp.status()))
                }
            }
            Err(e) => Err(format!("Request failed: {}", e)),
        }
    }

    fn execute_stream(
        &self,
        stage: &PipelineStage,
        input: &str,
        stage_index: usize,
        emit: &dyn Fn(&CouncilEvent),
    ) -> Result<String, String> {
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
            "max_tokens": COUNCIL_STAGE_MAX_TOKENS,
            "stream": true,
        });
        let body_str = serde_json::to_string(&body).unwrap_or_default();
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .map_err(|e| format!("Request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Backend returned status {}", resp.status()));
        }

        if let Ok(state) = self.state.lock() {
            state.reset_abort();
        }

        let mut accumulated = String::new();
        let reader = std::io::BufReader::new(resp);
        use std::io::BufRead;
        for line in reader.lines() {
            if let Ok(state) = self.state.lock() {
                if state.is_abort_requested() {
                    tracing::info!("Inference stream interrupted immediately by human chime-in!");
                    break;
                }
            }
            let line = line.map_err(|e| format!("Stream read error: {}", e))?;
            let trimmed = line.trim();
            if trimmed.starts_with("data: ") {
                let data = &trimmed[6..];
                if data == "[DONE]" {
                    break;
                }
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(delta) = json["choices"]
                        .as_array()
                        .and_then(|c| c.first())
                        .and_then(|c| c.get("delta"))
                        .and_then(|d| {
                            d.get("content")
                                .or_else(|| d.get("reasoning_content"))
                                .or_else(|| d.get("reasoning"))
                        })
                        .and_then(|c| c.as_str())
                    {
                        if !delta.is_empty() {
                            accumulated.push_str(delta);
                            emit(&CouncilEvent::TokenChunk {
                                stage_index,
                                text: delta.to_string(),
                            });
                        }
                    }
                }
            }
        }

        let was_aborted = self.state.lock().map(|s| s.is_abort_requested()).unwrap_or(false);
        if accumulated.is_empty() {
            if was_aborted {
                Ok("[Generation interrupted by human guidance]".to_string())
            } else {
                self.execute(stage, input)
            }
        } else {
            Ok(accumulated)
        }
    }

    fn take_human_feedback(&self) -> Option<String> {
        self.state.lock().ok().and_then(|s| s.take_human_guidance())
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

    fn take_human_feedback(&self) -> Option<String> {
        self.inner.take_human_feedback()
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
        // Automatically persist the full debate transcript to disk (skip transient queries)
        if !crate::council::history::is_transient(transcript) {
            if let Err(e) = crate::council::save_transcript(transcript) {
                tracing::warn!("Failed to save debate transcript: {}", e);
            }
        }

        for (i, turn) in transcript.turns.iter().enumerate() {
            let role_name = match turn.role {
                crate::council::CouncilRole::Planner => "1. Planner",
                crate::council::CouncilRole::Generator => "2. Generator",
                crate::council::CouncilRole::Auditor => "3. Auditor",
                crate::council::CouncilRole::Synthesizer => "4. Synthesizer",
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

/// Check if a request is auxiliary (e.g. title generation, summarization, tool result follow-up).
pub fn is_auxiliary_request(body: &[u8]) -> bool {
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(max_tokens) = json.get("max_tokens").and_then(|m| m.as_u64()) {
            if max_tokens <= 128 {
                return true;
            }
        }
        if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {

            for msg in messages {
                if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                    let lower = content.to_lowercase();
                    if lower.contains("title") && msg.get("role").and_then(|r| r.as_str()) == Some("system") {
                        return true;
                    }
                    if (lower.contains("write the title") || lower.contains("<session>")) && msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Determine whether council debate should run universally across all models.
pub fn should_run_council_universal(
    req: &tiny_http::Request,
    state: &Arc<Mutex<ProxyState>>,
    body: &[u8],
) -> bool {
    if is_auxiliary_request(body) {
        return false;
    }
    let enable_council = state.lock().map(|s| s.enable_council).unwrap_or(false);
    if enable_council {
        return true;
    }
    for header in req.headers() {
        if header.field.as_str().as_str().eq_ignore_ascii_case("x-swai-pipeline") {
            return true;
        }
    }
    false
}
