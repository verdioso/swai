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

use crate::council::barrier::CouncilPauseController;
use crate::council::events::CouncilEvent;
use crate::council::types::{
    CouncilPipelineConfig, CouncilRole, DebateOutcome, DebateTranscript, FallbackAction,
    PipelineStage, TurnResult,
};
use crate::council::human_feedback;
use crate::council::Executor;
use std::time::Instant;

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
    /// Shared human-in-the-loop pause barrier. When `Some`, the pipeline
    /// pauses before executing the Synthesizer stage unless the human resumes.
    pause_controller: Option<CouncilPauseController>,
}

impl<E: Executor> CouncilEngine<E> {
    pub fn new(config: CouncilPipelineConfig, executor: E) -> Self {
        Self {
            config,
            executor,
            events: None,
            pause_controller: None,
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
            pause_controller: None,
        }
    }

    /// Enable human-in-the-loop interruption. When set, the pipeline pauses
    /// before the Synthesizer stage (if a pause is requested) and injects any
    /// human feedback into the synthesizer prompt.
    pub fn with_pause_controller(mut self, controller: CouncilPauseController) -> Self {
        self.pause_controller = Some(controller);
        self
    }

    /// The pause controller for this engine, if human-in-the-loop mode is on.
    pub fn pause_controller(&self) -> Option<&CouncilPauseController> {
        self.pause_controller.as_ref()
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

        let slug = crate::council::history::prompt_to_slug(input_prompt);
        let session_id = format!(
            "debate_{}_{}",
            chrono::Local::now().format("%Y%m%d_%H%M%S"),
            slug
        );
        let mut state = DebateState::new(
            session_id,
            input_prompt.to_string(),
            self.config.clone(),
        );
        crate::council::history::save_intermediate(&state.transcript);

        // Stage 1: Generator (first stage with role Generator or first stage)
        self.run_generator(&mut state);
        crate::council::history::save_intermediate(&state.transcript);
        if !state.aborted {
            // Stage 2: Auditor(s)
            self.run_auditors(&mut state);
            crate::council::history::save_intermediate(&state.transcript);
            // Stage 3: Synthesizer
            self.run_synthesizer(&mut state);
            crate::council::history::save_intermediate(&state.transcript);
        }

        let outcome = self.build_outcome(state);

        // Always emit the terminal event once the transcript is finalized.
        let transcript = match &outcome {
            DebateOutcome::Success { transcript, .. }
            | DebateOutcome::Partial { transcript, .. }
            | DebateOutcome::Aborted { transcript, .. } => Some(transcript.clone()),
        };
        if let Some(full_transcript) = transcript {
            crate::council::history::save_intermediate(&full_transcript);
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
            "Review and critique the following draft implementation for any bugs, missing requirements, or improvements. (Note: The Generator produces the implementation; file creation on disk is handled automatically by the system. Critique the technical implementation, correctness, and code completeness):\n\nOriginal prompt:\n{}\n\nDraft to audit:\n{draft}\n\nProvide constructive technical critiques and suggested improvements:",
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

                    human_feedback::handle_human_audit_feedback(
                        &self.executor,
                        stage,
                        *stage_index,
                        &draft,
                        state,
                        &|e| self.emit(e),
                    );
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

        // Human-in-the-loop gate: pause before the Synthesizer stage if the
        // user has "Chimed In". Block until they resume (optionally with
        // feedback). Any injected guidance becomes an authoritative turn in
        // the transcript and is prepended to the prompt.
        let human_feedback = self.await_human_gate().or_else(|| self.executor.take_human_feedback());
        if let Some(ref fb) = human_feedback {
            let trimmed = fb.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
            if matches!(trimmed.as_str(), "stop" | "cancel" | "abort" | "halt" | "exit" | "quit" | "please stop") {
                state.aborted = true;
                self.emit(CouncilEvent::HumanIntervention { stage_index, feedback: fb.clone() });
                state.transcript.append_turn(TurnResult {
                    turn_index: state.transcript.turns.len(),
                    role: CouncilRole::Custom("Human Intervention".into()),
                    model_id: "human".into(),
                    output: "⏹ Debate stopped by human operator.".into(),
                    duration: std::time::Duration::ZERO,
                    error: None,
                });
                return;
            }
            self.emit(CouncilEvent::HumanIntervention { stage_index, feedback: fb.clone() });
        }

        let prompt = human_feedback::build_synthesizer_prompt(
            &self.config,
            &state.transcript.input_prompt,
            draft,
            &critiques,
            human_feedback.as_deref(),
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
                // Record the human intervention as an authoritative turn so it
                // appears with a distinct badge in the transcript.
                if let Some(feedback) = &human_feedback {
                    state.transcript.append_turn(TurnResult {
                        turn_index: state.transcript.turns.len(),
                        role: CouncilRole::Custom("Human Intervention".into()),
                        model_id: "human".into(),
                        output: feedback.clone(),
                        duration: std::time::Duration::ZERO,
                        error: None,
                    });
                }
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

    /// Wait at the human-in-the-loop barrier before the Synthesizer runs.
    ///
    /// If a pause controller is present and a pause was requested, block until
    /// the human resumes (with or without feedback) or aborts. Returns the
    /// injected human feedback, if any. The gate consumes the decision and
    /// clears the pause flag atomically, so a later gate will not block again.
    fn await_human_gate(&self) -> Option<String> {
        let Some(controller) = &self.pause_controller else {
            return None;
        };
        human_feedback::await_human_gate(controller)
    }

    fn build_outcome(&self, state: DebateState) -> DebateOutcome {
        if state.aborted {
            return DebateOutcome::Aborted { reason: "pipeline aborted due to stage failure".into(), transcript: state.transcript };
        }
        match state.draft {
            Some(resp) if !state.warnings.is_empty() => DebateOutcome::Partial { fallback_response: resp, warnings: state.warnings, transcript: state.transcript },
            Some(final_response) => DebateOutcome::Success { final_response, transcript: state.transcript },
            None if !state.warnings.is_empty() => DebateOutcome::Partial { fallback_response: String::new(), warnings: state.warnings, transcript: state.transcript },
            _ => DebateOutcome::Aborted { reason: "no response produced".into(), transcript: state.transcript },
        }
    }
}
