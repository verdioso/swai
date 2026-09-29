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
    CouncilPipelineConfig, CouncilRole, DebateOutcome, DebateTranscript,
    PipelineStage, TurnResult,
};
pub use crate::council::types::{CouncilError, DebateState};
use crate::council::human_feedback;
use crate::council::Executor;
use std::time::Instant;


/// The council debate engine.
///
/// When constructed with `with_events`, `StageStarted` / `TokenChunk` /
/// `StageCompleted` / `PipelineCompleted` events are broadcast as the debate
/// runs. `PipelineFailed` is emitted when a stage errors.
pub struct CouncilEngine<E: Executor> {
    pub config: CouncilPipelineConfig,
    pub executor: E,
    events: Option<tokio::sync::broadcast::Sender<CouncilEvent>>,
    pub workspace: Option<std::path::PathBuf>,
}

impl<E: Executor> CouncilEngine<E> {
    pub fn new(config: CouncilPipelineConfig, executor: E) -> Self {
        Self {
            config,
            executor,
            events: None,
            workspace: None,
        }
    }

    /// Construct an engine that broadcasts `CouncilEvent`s to subscribers.
    
    pub fn with_workspace(mut self, workspace: std::path::PathBuf) -> Self {
        self.workspace = Some(workspace);
        self
    }

    pub fn with_events(
        config: CouncilPipelineConfig,
        executor: E,
        events: tokio::sync::broadcast::Sender<CouncilEvent>,
    ) -> Self {
        Self {
            config,
            executor,
            events: Some(events),
            workspace: None,
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

        // RC1: Planner inspection loop.
        // The Planner may need to read files before it can emit a write directive.
        // Loop: Planner → inspect tool → append result → re-invoke Planner.
        // Cap at MAX_INSPECTION_ITERS to prevent infinite read-loops.
        const MAX_INSPECTION_ITERS: usize = 8;
        let workspace = self.workspace.clone().unwrap_or_else(|| crate::council::tools::detect_workspace_root());
        let mut inspection_count = 0;

        if let Some(stage) = self.config.stages.iter().find(|s| s.role == CouncilRole::Planner) {
            self.emit(CouncilEvent::StageStarted {
                stage_index: 0,
                role: CouncilRole::Planner,
                model_id: stage.model_id.clone(),
                model_name: stage.model_id.clone(),
            });
            let welcome = "Welcome! Agents, stay steady. I'm taking a quick look at all available tools and skills in our workspace, then I'll orchestrate a clear, step-by-step plan for us.";
            self.emit(CouncilEvent::StageCompleted {
                stage_index: 0,
                full_text: welcome.to_string(),
                duration_sec: 0.0,
                tok_per_sec: 0.0,
            });
        }

        loop {
            crate::council::planner::run_planner_stage(
                &self.config.stages,
                &self.executor,
                &mut state,
                &|e| self.emit(e),
            );
            crate::council::history::save_intermediate(&state.transcript);

            if state.aborted {
                break;
            }

            // Check if the Planner emitted an inspection (read-only) directive.
            if let Some(ref dir) = state.planner_directive {
                if crate::council::planner::is_immediate_inspection_directive(dir) {
                    inspection_count += 1;

                    // Try to execute the tool locally first (RC1).
                    if let Some(result) = crate::council::tools::execute_local_tool(
                        &workspace,
                        &dir.tool,
                        &dir.target,
                    ) {
                        // Append the tool result to the input prompt so the
                        // Planner sees it on the next invocation.
                        let tool_desc = format!("🔍 Exploring: {} {}", dir.tool, dir.target);
                        self.emit(CouncilEvent::StageCompleted {
                            stage_index: 0,
                            full_text: format!("🔍 Exploring: {} {}", dir.tool, dir.target),
                            duration_sec: 0.0,
                            tok_per_sec: 0.0,
                        });
                        let header = if state.transcript.input_prompt.contains("Execution History & Tool Results:") {
                            String::new()
                        } else {
                            "\n\nExecution History & Tool Results:\n".to_string()
                        };
                        state.transcript.input_prompt.push_str(&format!(
                            "{}---\nTool: {} | Target: {}\nResult:\n{}\n---EXIT:{}---\n",
                            header,
                            dir.tool,
                            dir.target,
                            result.output,
                            if result.success { "0" } else { "1" },
                        ));

                        // Record the inspection as a turn in the transcript.
                        state.transcript.append_turn(crate::council::TurnResult {
                            turn_index: state.transcript.turns.len(),
                            role: crate::council::CouncilRole::Custom(format!("LocalTool:{}", tool_desc)),
                            model_id: "swai-local".into(),
                            output: result.output,
                            duration: std::time::Duration::ZERO,
                            error: if result.success { None } else { Some("tool error".into()) },
                        });

                        // Clear the directive so the Planner starts fresh.
                        state.planner_directive = None;

                        if inspection_count >= MAX_INSPECTION_ITERS {
                            state.warnings.push(format!(
                                "Planner exhausted {} inspection iterations without emitting a write directive.",
                                MAX_INSPECTION_ITERS
                            ));
                            state.aborted = true;
                            break;
                        }

                        // Re-invoke the Planner with the updated prompt.
                        continue;
                    } else {
                        // Tool not recognized locally — fall through to external
                        // CLI round-trip (the old behavior).
                        crate::council::planner::should_skip_generation_for_inspection(&mut state);
                        break;
                    }
                }
            }

            // Planner emitted a write directive (or no directive at all).
            // Exit the inspection loop and proceed to Generator/Auditor.
            break;
        }

        if !state.aborted && state.draft.is_none() {
            let max_iterations = 3;
            let mut current_iteration = 0;
            let mut last_approved = false;
            let mut auditor_ran = false;

            while current_iteration < max_iterations && !state.aborted {
                current_iteration += 1;

                // Stage 1: Generator
                self.run_generator(&mut state, current_iteration);
                crate::council::history::save_intermediate(&state.transcript);
                if state.aborted {
                    break;
                }

                // Stage 2: Auditor(s)
                let audit_count_before = state.audit_results.len();
                self.run_auditors(&mut state);
                crate::council::history::save_intermediate(&state.transcript);

                if state.audit_results.len() > audit_count_before {
                    // Auditor produced a verdict this iteration.
                    auditor_ran = true;
                    if let Some(last_critique) = state.audit_results.last() {
                        let verdict = crate::council::planner::parse_audit_verdict(last_critique);
                        last_approved = verdict
                            .map(|v| v.approved)
                            .unwrap_or_else(|| {
                                last_critique.contains("STATUS: APPROVED")
                                    || last_critique.contains("STATUS:APPROVED")
                            });
                        if last_approved {
                            break;
                        }
                    }
                } else {
                    // Auditor stage errored/skipped — treat draft as best-effort and stop looping.
                    break;
                }
            }

            // If an Auditor actually ran and we exhausted all iterations without approval,
            // abort so the rejected draft is never silently passed back as a "success".
            if auditor_ran && !last_approved && !state.aborted {
                let critique_summary = state.audit_results.last()
                    .and_then(|c| crate::council::planner::parse_audit_verdict(c))
                    .map(|v| v.critique)
                    .unwrap_or_else(|| "Auditor did not approve the implementation.".into());
                state.warnings.push(format!(
                    "Council pipeline exhausted {} iterations without Auditor approval. Last critique: {}",
                    max_iterations, critique_summary
                ));
                state.aborted = true;
                // Clear draft so build_outcome returns Aborted, not a rejected draft.
                state.draft = None;
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
                "{}\n\nAuditor Feedback / Required Fixes from previous iteration:\n{}\n\nPlease update the implementation to address the above feedback. Output exactly ONE structured JSON tool call. Do not use markdown fences.",
                scoped_input,
                last_critique
            );
        } else {
            scoped_input = format!(
                "{}\n\nOutput exactly ONE structured JSON tool call. Do not use markdown fences.",
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
        let directive_str = state.planner_directive.as_ref().map(|d| format!("Architect's Atomic Step:\n- Tool: {}\n- Target: {}\n- Action: {}\n- Constraints: {}", d.tool, d.target, d.action, d.rules)).unwrap_or_else(|| "No specific plan provided.".into());

        let prompt = format!(
            "Review and critique the following draft implementation for any bugs, missing requirements, or improvements. (Note: The Generator produces the implementation; file creation on disk is handled automatically by the system. Critique the technical implementation, correctness, and code completeness):\n\nOriginal prompt:\n{}\n\n{}\n\nDraft to audit:\n{draft}\n\nCRITICAL DIRECTIVE: You have NO tools available. Do NOT attempt to emit tool calls or format your output as a tool call.\nProvide constructive technical critiques and suggested improvements. Then output a JSON object on its own line, after your critique, in exactly this shape:\n{{\"status\": \"approved\", \"critique\": \"<one-line summary>\"}}\nor\n{{\"status\": \"changes_needed\", \"critique\": \"<what must change>\"}}",
            state.transcript.input_prompt, directive_str
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
            let reason = if !state.warnings.is_empty() {
                state.warnings.join("; ")
            } else {
                "pipeline aborted due to stage failure".into()
            };
            return DebateOutcome::Aborted { reason, transcript: state.transcript };
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
