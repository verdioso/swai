//! SWAI — Council multi-turn debate execution engine.
//!
//! Coordinates the Generator -> Auditor(s) workflow with
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
    pub planner_directive: Option<crate::council::planner::PlannerDirective>,
    pub audit_results: Vec<String>,
    pub warnings: Vec<String>,
    pub aborted: bool,
}

impl DebateState {
    pub fn new(session_id: String, input_prompt: String, config: CouncilPipelineConfig) -> Self {
        Self {
            transcript: DebateTranscript::new(session_id, input_prompt, config),
            draft: None,
            planner_directive: None,
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
            FallbackAction::Skip => self.warnings.push(format!("Stage skipped: {error}")),
            FallbackAction::Retry { max_retries } => self.warnings.push(format!("Stage retried {max_retries} times failed: {error}")),
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

        // Stage 0: Planner (if configured)
        crate::council::planner::run_planner_stage(&self.config.stages, &self.executor, &mut state, &|e| self.emit(e));
        crate::council::history::save_intermediate(&state.transcript);

        if !state.aborted && !crate::council::planner::should_skip_generation_for_inspection(&mut state) {
            let max_iterations = 3;
            let mut current_iteration = 0;

            while current_iteration < max_iterations && !state.aborted {
                current_iteration += 1;

                // Stage 1: Generator
                self.run_generator(&mut state, current_iteration);
                crate::council::history::save_intermediate(&state.transcript);
                if state.aborted {
                    break;
                }

                // Stage 2: Auditor(s)
                self.run_auditors(&mut state);
                crate::council::history::save_intermediate(&state.transcript);

                if let Some(last_critique) = state.audit_results.last() {
                    let approved = crate::council::planner::parse_audit_verdict(last_critique)
                        .map(|v| v.approved)
                        .unwrap_or_else(|| {
                            last_critique.contains("STATUS: APPROVED")
                                || last_critique.contains("STATUS:APPROVED")
                        });
                    if approved {
                        break;
                    }
                }
            }
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

    fn run_generator(&self, state: &mut DebateState, _iteration: usize) {
        let (stage, stage_index) = {
            let stages = &self.config.stages;
            match stages.iter().position(|s| s.role == CouncilRole::Generator) {
                Some(idx) => (&stages[idx], idx),
                None => (&stages[0], 0),
            }
        };

        let mut scoped_input = if let Some(ref dir) = state.planner_directive {
            crate::council::planner::build_scoped_generator_prompt(dir, &state.transcript.input_prompt)
        } else {
            state.transcript.input_prompt.clone()
        };

        if let Some(last_critique) = state.audit_results.last() {
            scoped_input = format!(
                "{}\n\nAuditor Feedback / Required Fixes from previous iteration:\n{}\n\nPlease update the implementation to address the above feedback. ONLY output pure code inside markdown blocks (e.g., ```rust\n...\n```). Do not output JSON.",
                scoped_input,
                last_critique
            );
        } else {
            scoped_input = format!(
                "{}\n\nPlease output the pure code implementation inside markdown blocks (e.g., ```rust\n...\n```). Do not wrap your response in JSON.",
                scoped_input
            );
        }
        let start = Instant::now();

        self.emit(CouncilEvent::StageStarted {
            stage_index,
            role: stage.role.clone(),
            model_id: stage.model_id.clone(),
            model_name: stage.model_id.clone(),
        });

        match self.executor.execute_stream(stage, &scoped_input, stage_index, &|event| self.emit(event.clone())) {
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
            "Review and critique the following draft implementation for any bugs, missing requirements, or improvements. (Note: The Generator produces the implementation; file creation on disk is handled automatically by the system. Critique the technical implementation, correctness, and code completeness):\n\nOriginal prompt:\n{}\n\nDraft to audit:\n{draft}\n\nProvide constructive technical critiques and suggested improvements. Then output a JSON object on its own line, after your critique, in exactly this shape:\n{{\"status\": \"approved\", \"critique\": \"<one-line summary>\"}}\nor\n{{\"status\": \"changes_needed\", \"critique\": \"<what must change>\"}}",
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

    fn build_outcome(&self, state: DebateState) -> DebateOutcome {
        if state.aborted {
            return DebateOutcome::Aborted { reason: "pipeline aborted due to stage failure".into(), transcript: state.transcript };
        }
        let target = state.planner_directive.as_ref().map(|d| d.target.clone()).filter(|s| !s.is_empty());
        let tool = state.planner_directive.as_ref().map(|d| d.tool.clone()).filter(|s| !s.is_empty());
        match state.draft {
            Some(resp) if !state.warnings.is_empty() => DebateOutcome::Partial { fallback_response: resp, warnings: state.warnings, transcript: state.transcript, target, tool },
            Some(final_response) => DebateOutcome::Success { final_response, transcript: state.transcript, target, tool },
            None if !state.warnings.is_empty() => DebateOutcome::Partial { fallback_response: String::new(), warnings: state.warnings, transcript: state.transcript, target, tool },
            _ => DebateOutcome::Aborted { reason: "no response produced".into(), transcript: state.transcript },
        }
    }
}
