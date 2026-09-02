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
use crate::council::types::CouncilPipelineConfig;

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
) -> String {
    let _ = config; // Reserved for future per-role template overrides.
    match human_feedback {
        Some(feedback) => format!(
            "Human guidance (authoritative):\n{feedback}\n\nOriginal prompt:\n{}\n\nDraft response:\n{}\n\nAudit critiques:\n{}",
            input_prompt, draft, critiques
        ),
        None => format!(
            "Original prompt:\n{}\n\nDraft response:\n{}\n\nAudit critiques:\n{}",
            input_prompt, draft, critiques
        ),
    }
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
        let prompt = build_synthesizer_prompt(&config, "orig", "draft", "critiques", None);
        assert!(prompt.starts_with("Original prompt:\norig"));
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
