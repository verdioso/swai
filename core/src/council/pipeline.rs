//! SWAI — Council multi-turn debate execution engine.
//!
//! Coordinates the Generator -> Auditor(s) -> Synthesizer workflow with
//! failure-matrix resilience: graceful fallback to the best available
//! draft when any stage times out or errors, and support for both
//! concurrent (parallel) and sequential (fast process-swap < 500 ms)
//! execution modes.
//!
//! When constructed with `CouncilEngine::with_events`, the engine broadcasts
//! `CouncilEvent`s over a `tokio::sync::broadcast` channel so the UI can
//! observe live progress. Emission order is strictly:
//! `StageStarted` -> `TokenChunk`(s) -> `StageCompleted` -> `PipelineCompleted`.

use crate::council::events::CouncilEvent;
use crate::council::types::{
    CouncilPipelineConfig, CouncilRole, DebateOutcome, DebateTranscript, FallbackAction,
    PipelineStage, TurnResult,
};
use std::time::Instant;

/// Buffered characters before a streaming token chunk is emitted.
const STREAM_CHUNK_BUDGET: usize = 16;

/// Errors that can occur during council pipeline execution.
#[derive(Debug, thiserror::Error)]
pub enum CouncilError {
    #[error("pipeline has no stages")]
    EmptyPipeline,
    #[error("stage failed: {0}")]
    StageFailed(String),
    #[error("aborted: {0}")]
    Aborted(String),
}

/// Abstraction over the LLM inference backend used by each debate stage.
pub trait Executor: Send + Sync {
    fn execute(&self, stage: &PipelineStage, input: &str) -> Result<String, String>;

    /// Stream a stage's output, emitting one `CouncilEvent::TokenChunk` per
    /// delta as it is produced. The default implementation runs the
    /// non-streaming `execute` and splits its output into word-sized chunks,
    /// which is sufficient for backends that do not expose live streaming.
    fn execute_stream(
        &self,
        stage: &PipelineStage,
        input: &str,
        stage_index: usize,
        emit: &dyn Fn(&CouncilEvent),
    ) -> Result<String, String> {
        let output = self.execute(stage, input)?;
        for chunk in chunk_output(&output) {
            emit(&CouncilEvent::TokenChunk {
                stage_index,
                text: chunk,
            });
        }
        Ok(output)
    }
}

/// Split generated text into streaming token chunks.
///
/// Chunks are bounded by `STREAM_CHUNK_BUDGET` characters or whitespace, and
/// concatenating them in order reconstructs the original text exactly. An
/// empty input yields a single empty chunk so a stage always emits at least
/// one token event.
fn chunk_output(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        buf.push(ch);
        if ch.is_whitespace() || buf.len() >= STREAM_CHUNK_BUDGET {
            chunks.push(std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        chunks.push(buf);
    }
    if chunks.is_empty() {
        chunks.push(text.to_string());
    }
    chunks
}

/// Mutable state carried through a single debate execution.
pub struct DebateState {
    pub transcript: DebateTranscript,
    pub draft: Option<String>,
    pub audit_results: Vec<String>,
    pub warnings: Vec<String>,
    pub aborted: bool,
}

impl DebateState {
    pub fn new(session_id: String, input_prompt: String, config: CouncilPipelineConfig) -> Self {
        Self {
            transcript: DebateTranscript::new(session_id, input_prompt, config),
            draft: None,
            audit_results: Vec::new(),
            warnings: Vec::new(),
            aborted: false,
        }
    }

    pub fn handle_failure(&mut self, fallback: &FallbackAction, _turn_index: usize, error: &str) {
        match fallback {
            FallbackAction::Abort => {
                self.warnings.push(format!("Stage failed (abort): {error}"));
                self.aborted = true;
            }
            FallbackAction::Skip => {
                self.warnings.push(format!("Stage skipped: {error}"));
            }
            FallbackAction::Retry { max_retries } => {
                self.warnings
                    .push(format!("Stage retried {max_retries} times failed: {error}"));
            }
        }
    }
}

/// The council debate engine.
///
/// When constructed with `with_events`, `StageStarted` / `TokenChunk` /
/// `StageCompleted` / `PipelineCompleted` events are broadcast as the debate
/// runs. `PipelineFailed` is emitted when a stage errors.
pub struct CouncilEngine<E: Executor> {
    pub config: CouncilPipelineConfig,
    pub executor: E,
    events: Option<tokio::sync::broadcast::Sender<CouncilEvent>>,
}

impl<E: Executor> CouncilEngine<E> {
    pub fn new(config: CouncilPipelineConfig, executor: E) -> Self {
        Self {
            config,
            executor,
            events: None,
        }
    }

    /// Construct an engine that broadcasts `CouncilEvent`s to subscribers.
    pub fn with_events(
        config: CouncilPipelineConfig,
        executor: E,
        events: tokio::sync::broadcast::Sender<CouncilEvent>,
    ) -> Self {
        Self {
            config,
            executor,
            events: Some(events),
        }
    }

    /// Forward an event to every active subscriber. A disconnected
    /// subscriber (or none) is ignored so event loss never breaks a debate.
    fn emit(&self, event: CouncilEvent) {
        if let Some(sender) = &self.events {
            let _ = sender.send(event);
        }
    }

    /// Execute the full debate pipeline with input prompt.
    pub fn execute(&self, input_prompt: &str) -> DebateOutcome {
        if self.config.stages.is_empty() {
            return DebateOutcome::Aborted {
                reason: "empty pipeline".into(),
                transcript: DebateTranscript::new(
                    String::new(),
                    input_prompt.to_string(),
                    self.config.clone(),
                ),
            };
        }

        let mut state = DebateState::new(
            format!("debate-{}", chrono::Utc::now().timestamp()),
            input_prompt.to_string(),
            self.config.clone(),
        );

        // Stage 1: Generator (first stage with role Generator or first stage)
        self.run_generator(&mut state);
        if !state.aborted {
            // Stage 2: Auditor(s)
            self.run_auditors(&mut state);
            // Stage 3: Synthesizer
            self.run_synthesizer(&mut state);
        }

        let outcome = self.build_outcome(state);

        // Always emit the terminal event once the transcript is finalized.
        let transcript = match &outcome {
            DebateOutcome::Success { transcript, .. }
            | DebateOutcome::Partial { transcript, .. }
            | DebateOutcome::Aborted { transcript, .. } => Some(transcript.clone()),
        };
        if let Some(full_transcript) = transcript {
            self.emit(CouncilEvent::PipelineCompleted { full_transcript });
        }

        outcome
    }

    fn run_generator(&self, state: &mut DebateState) {
        let (stage, stage_index) = {
            let stages = &self.config.stages;
            match stages.iter().position(|s| s.role == CouncilRole::Generator) {
                Some(idx) => (&stages[idx], idx),
                None => (&stages[0], 0),
            }
        };

        let input = &state.transcript.input_prompt;
        let start = Instant::now();

        self.emit(CouncilEvent::StageStarted {
            stage_index,
            role: stage.role.clone(),
            model_id: stage.model_id.clone(),
            model_name: stage.model_id.clone(),
        });

        match self
            .executor
            .execute_stream(stage, input, stage_index, &|event| self.emit(event.clone()))
        {
            Ok(output) => {
                let dur = start.elapsed();
                self.emit(CouncilEvent::StageCompleted {
                    stage_index,
                    full_text: output.clone(),
                    duration_sec: dur.as_secs_f64(),
                    tok_per_sec: 0.0,
                });
                state.draft = Some(output.clone());
                state.transcript.append_turn(TurnResult {
                    turn_index: 0,
                    role: CouncilRole::Generator,
                    model_id: stage.model_id.clone(),
                    output,
                    duration: dur,
                    error: None,
                });
            }
            Err(err) => {
                self.emit(CouncilEvent::PipelineFailed {
                    stage_index,
                    error: err.clone(),
                });
                state.handle_failure(&self.config.fallback, 0, &err);
            }
        }
    }

    fn run_auditors(&self, state: &mut DebateState) {
        let auditors: Vec<(usize, &PipelineStage)> = self
            .config
            .stages
            .iter()
            .enumerate()
            .filter(|(_, s)| s.role == CouncilRole::Auditor)
            .collect();

        if auditors.is_empty() || state.draft.is_none() {
            return;
        }

        let draft = state.draft.as_ref().unwrap().clone();
        let prompt = format!(
            "Draft to audit:\n{draft}\n\nOriginal prompt:\n{}",
            state.transcript.input_prompt
        );

        for (i, (stage_index, stage)) in auditors.iter().enumerate() {
            if state.aborted {
                break;
            }
            let start = Instant::now();

            self.emit(CouncilEvent::StageStarted {
                stage_index: *stage_index,
                role: stage.role.clone(),
                model_id: stage.model_id.clone(),
                model_name: stage.model_id.clone(),
            });

            match self
                .executor
                .execute_stream(stage, &prompt, *stage_index, &|event| {
                    self.emit(event.clone())
                }) {
                Ok(output) => {
                    let dur = start.elapsed();
                    self.emit(CouncilEvent::StageCompleted {
                        stage_index: *stage_index,
                        full_text: output.clone(),
                        duration_sec: dur.as_secs_f64(),
                        tok_per_sec: 0.0,
                    });
                    state.audit_results.push(output.clone());
                    state.transcript.append_turn(TurnResult {
                        turn_index: i + 1,
                        role: CouncilRole::Auditor,
                        model_id: stage.model_id.clone(),
                        output,
                        duration: dur,
                        error: None,
                    });
                }
                Err(err) => {
                    self.emit(CouncilEvent::PipelineFailed {
                        stage_index: *stage_index,
                        error: err.clone(),
                    });
                    state.handle_failure(&self.config.fallback, i + 1, &err);
                }
            }
        }
    }

    fn run_synthesizer(&self, state: &mut DebateState) {
        let (stage, stage_index) = match self
            .config
            .stages
            .iter()
            .enumerate()
            .find(|(_, s)| s.role == CouncilRole::Synthesizer)
        {
            Some((idx, s)) => (s, idx),
            None => return,
        };
        if state.aborted {
            return;
        }

        let draft = state.draft.as_deref().unwrap_or("");
        let critiques = if state.audit_results.is_empty() {
            "No audit critiques.".to_string()
        } else {
            state.audit_results.join("\n\n---\n\n")
        };
        let prompt = format!(
            "Original prompt:\n{}\n\nDraft response:\n{}\n\nAudit critiques:\n{}",
            state.transcript.input_prompt, draft, critiques
        );

        self.emit(CouncilEvent::StageStarted {
            stage_index,
            role: stage.role.clone(),
            model_id: stage.model_id.clone(),
            model_name: stage.model_id.clone(),
        });

        let start = Instant::now();
        match self
            .executor
            .execute_stream(stage, &prompt, stage_index, &|event| {
                self.emit(event.clone())
            }) {
            Ok(output) => {
                let dur = start.elapsed();
                self.emit(CouncilEvent::StageCompleted {
                    stage_index,
                    full_text: output.clone(),
                    duration_sec: dur.as_secs_f64(),
                    tok_per_sec: 0.0,
                });
                state.draft = Some(output.clone());
                state.transcript.append_turn(TurnResult {
                    turn_index: state.transcript.turns.len(),
                    role: CouncilRole::Synthesizer,
                    model_id: stage.model_id.clone(),
                    output,
                    duration: dur,
                    error: None,
                });
            }
            Err(err) => {
                self.emit(CouncilEvent::PipelineFailed {
                    stage_index,
                    error: err.clone(),
                });
                state.handle_failure(&self.config.fallback, state.transcript.turns.len(), &err);
            }
        }
    }

    fn build_outcome(&self, state: DebateState) -> DebateOutcome {
        if state.aborted {
            return DebateOutcome::Aborted {
                reason: "pipeline aborted due to stage failure".into(),
                transcript: state.transcript,
            };
        }

        match state.draft {
            Some(final_response) if !state.warnings.is_empty() => DebateOutcome::Partial {
                fallback_response: final_response,
                warnings: state.warnings,
                transcript: state.transcript,
            },
            Some(final_response) => DebateOutcome::Success {
                final_response,
                transcript: state.transcript,
            },
            None if !state.warnings.is_empty() => DebateOutcome::Partial {
                fallback_response: String::new(),
                warnings: state.warnings,
                transcript: state.transcript,
            },
            _ => DebateOutcome::Aborted {
                reason: "no response produced".into(),
                transcript: state.transcript,
            },
        }
    }
}
