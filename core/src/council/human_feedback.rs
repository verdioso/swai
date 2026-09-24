//! SWAI — Human-in-the-loop feedback injection for the Synthesizer stage.
//!
//! This module holds the pure, GTK-free logic for applying a human
//! ("Chime In") interruption to a debate right before the Synthesizer runs:
//!
//! * [`await_human_gate`] blocks the pipeline thread on the shared
//!   [`CouncilPauseController`] until the human resumes (with or without
//!   feedback) or aborts.
//! * [`build_synthesizer_prompt`] assembles the prompt sent to the Synthesizer,
//!   prepending any injected human guidance when present.
//!
//! Keeping this logic in its own file keeps `pipeline.rs` small and focused
//! while isolating the human-in-the-loop behavior in one testable place.

use crate::council::barrier::{CouncilPauseController, PauseDecision};
use crate::council::events::CouncilEvent;
use crate::council::executor::Executor;
use crate::council::types::{CouncilPipelineConfig, CouncilRole, PipelineStage, TurnResult};

/// Wait at the human-in-the-loop barrier before a stage executes.
///
/// If a pause controller is present and a pause was requested, block until the
/// human resumes (with or without feedback) or aborts. Returns the injected
/// human feedback, if any.
///
/// The gate consumes the decision and clears the pause flag atomically, so a
/// later gate will not block again. When no pause controller is configured, or
/// no pause was requested, this returns immediately with `None` so the common
/// (non-interrupted) path never blocks.
pub fn await_human_gate(controller: &CouncilPauseController) -> Option<String> {
    // Non-blocking fast path: skip the gate entirely when no pause was
    // requested and no decision is pending.
    if !controller.is_paused_requested() {
        return None;
    }

    match controller.await_stage_gate() {
        PauseDecision::ResumeWithFeedback { feedback } => feedback,
        PauseDecision::ResumeWithoutChanges => None,
        PauseDecision::Aborted => None,
    }
}

/// Build the prompt for the Auditor when human guidance is injected during an audit.
pub fn build_auditor_guidance_prompt(
    input_prompt: &str,
    draft: &str,
    human_guidance: &str,
) -> String {
    format!(
        "The human operator has chimed in with specific authoritative guidance regarding this task.\n\nOriginal prompt:\n{input_prompt}\n\nDraft to audit:\n{draft}\n\nAuthoritative Human Guidance:\n{human_guidance}\n\nRe-audit the draft specifically addressing and incorporating the user's guidance. Provide constructive technical critiques and recommendations for the Synthesizer:"
    )
}

/// Run follow-up audit after human intervention during an audit stage.
pub fn run_guided_audit<E: Executor>(
    executor: &E,
    stage: &PipelineStage,
    stage_index: usize,
    input_prompt: &str,
    draft: &str,
    feedback: &str,
    emit: &dyn Fn(CouncilEvent),
) -> Result<TurnResult, String> {
    let prompt = build_auditor_guidance_prompt(input_prompt, draft, feedback);
    let start = std::time::Instant::now();
    emit(CouncilEvent::StageStarted {
        stage_index,
        role: stage.role.clone(),
        model_id: stage.model_id.clone(),
        model_name: stage.model_id.clone(),
    });
    let output = executor.execute_stream(stage, &prompt, stage_index, &|ev| emit(ev.clone()))?;
    let duration = start.elapsed();
    emit(CouncilEvent::StageCompleted {
        stage_index,
        full_text: output.clone(),
        duration_sec: duration.as_secs_f64(),
        tok_per_sec: 0.0,
    });
    Ok(TurnResult {
        turn_index: 0,
        role: CouncilRole::Auditor,
        model_id: stage.model_id.clone(),
        output,
        duration,
        error: None,
    })
}

/// Handle human guidance during an audit stage by recording the intervention
/// and triggering an updated critique from the auditor.
pub fn handle_human_audit_feedback<E: Executor>(
    executor: &E,
    stage: &PipelineStage,
    stage_index: usize,
    draft: &str,
    state: &mut crate::council::pipeline::DebateState,
    emit: &dyn Fn(CouncilEvent),
) {
    if let Some(feedback) = executor.take_human_feedback() {
        let trimmed = feedback.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
        if matches!(trimmed.as_str(), "stop" | "cancel" | "abort" | "halt" | "exit" | "quit" | "please stop") {
            state.aborted = true;
            emit(CouncilEvent::HumanIntervention {
                stage_index,
                feedback: feedback.clone(),
            });
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

        emit(CouncilEvent::HumanIntervention {
            stage_index,
            feedback: feedback.clone(),
        });
        state.transcript.append_turn(TurnResult {
            turn_index: state.transcript.turns.len(),
            role: CouncilRole::Custom("Human Intervention".into()),
            model_id: "human".into(),
            output: feedback.clone(),
            duration: std::time::Duration::ZERO,
            error: None,
        });
        if let Ok(mut turn) = run_guided_audit(
            executor,
            stage,
            stage_index,
            &state.transcript.input_prompt,
            draft,
            &feedback,
            emit,
        ) {
            turn.turn_index = state.transcript.turns.len();
            state.audit_results.push(turn.output.clone());
            state.transcript.append_turn(turn);
        }
    }
}

/// Build the prompt sent to the Synthesizer stage.
///
/// When `human_feedback` is `Some`, the guidance is prepended as an
/// authoritative block so the Synthesizer must account for it. Otherwise the
/// prompt follows the standard Generator -> Auditor -> Synthesizer layout.
pub fn build_synthesizer_prompt(
    config: &CouncilPipelineConfig,
    input_prompt: &str,
    draft: &str,
    critiques: &str,
    human_feedback: Option<&str>,
    directive: Option<&crate::council::planner::PlannerDirective>,
) -> String {
    let _ = config; // Reserved for future per-role template overrides.
    let guidance = match human_feedback {
        Some(feedback) => format!("Human guidance (authoritative):\n{feedback}\n\n"),
        None => String::new(),
    };
    let target_hint = match directive {
        Some(d) if !d.target.is_empty() || !d.tool.is_empty() => {
            let tool = if d.tool.is_empty() { "write_file" } else { &d.tool };
            format!(
                "Architect Directive:\n- Target: {}\n- Tool: {}\n- Template: {{\"name\": \"{}\", \"arguments\": {{\"path\": \"{}\", \"content\": \"...\"}}}}\n\n",
                d.target, tool, tool, d.target
            )
        }
        _ => String::new(),
    };
    format!(
        "{guidance}{target_hint}You are the Synthesizer. Review the Draft and Audit Critiques against the Original Prompt. Output ONLY the complete, full working final response and code directly. Do NOT repeat or quote the audit critiques or draft text:\n\nOriginal prompt:\n{}\n\nDraft response:\n{}\n\nAudit critiques:\n{}\n\nFinal Output:\n",
        input_prompt, draft, critiques
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::council::barrier::CouncilPauseController;

    #[test]
    fn test_no_controller_returns_none() {
        // When no pause was requested, the gate never blocks and returns None.
        let controller = CouncilPauseController::new();
        assert_eq!(await_human_gate(&controller), None);
    }

    #[test]
    fn test_build_prompt_with_feedback_prepends_guidance() {
        let config = CouncilPipelineConfig::default();
        let prompt = build_synthesizer_prompt(
            &config,
            "orig",
            "draft text",
            "critique one",
            Some("Please handle edge cases"),
            None,
        );
        assert!(prompt.starts_with("Human guidance (authoritative):"));
        assert!(prompt.contains("Please handle edge cases"));
        assert!(prompt.contains("Original prompt:\norig"));
        assert!(prompt.contains("Draft response:\ndraft text"));
        assert!(prompt.contains("Audit critiques:\ncritique one"));
    }

    #[test]
    fn test_build_prompt_without_feedback_is_standard() {
        let config = CouncilPipelineConfig::default();
        let prompt = build_synthesizer_prompt(&config, "orig", "draft", "critiques", None, None);
        assert!(prompt.contains("Original prompt:\norig"));
        assert!(!prompt.contains("Human guidance"));
    }

    #[test]
    fn test_pause_then_resume_injects_feedback() {
        let controller = CouncilPauseController::new();
        // Mirror the UI: request the pause *before* the pipeline reaches the
        // gate, then resume with feedback once the gate is reached.
        controller.request_pause();
        let shared = controller.clone();
        std::thread::spawn(move || {
            shared.wait_until_paused();
            shared.resume_with_feedback(Some("Steer this way".into()));
        });
        let feedback = await_human_gate(&controller).expect("expected feedback");
        assert_eq!(feedback, "Steer this way");
    }
}
