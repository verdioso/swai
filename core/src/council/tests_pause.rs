// ---------------------------------------------------------------------------
// Phase 33.3 — Human-in-the-loop ("Chime In") pause / feedback / resume tests
// ---------------------------------------------------------------------------

use crate::council::barrier::{CouncilPauseController, PauseDecision};
use crate::council::pipeline::CouncilEngine;
use crate::council::types::{CouncilPipelineConfig, CouncilRole, DebateOutcome, PipelineStage};
use crate::council::Executor;
use std::sync::{Arc, Mutex};

/// A mock executor that records every prompt it receives so tests can assert
/// that human feedback is actually injected into the Synthesizer prompt.
struct RecordingExecutor {
    responses: Vec<String>,
    synth_prompts: Arc<Mutex<Vec<String>>>,
}

impl RecordingExecutor {
    fn new(responses: Vec<String>, synth_prompts: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            responses,
            synth_prompts,
        }
    }
}

impl Executor for RecordingExecutor {
    fn execute(&self, stage: &PipelineStage, input: &str) -> Result<String, String> {
        if stage.role == CouncilRole::Synthesizer {
            self.synth_prompts.lock().unwrap().push(input.to_string());
        }
        let idx = if stage.role == CouncilRole::Generator {
            0
        } else if stage.role == CouncilRole::Auditor {
            1
        } else {
            2
        };
        if idx < self.responses.len() {
            Ok(self.responses[idx].clone())
        } else {
            Ok("mock response".to_string())
        }
    }
}

/// Build a three-stage (generator, auditor, synthesizer) pipeline config.
fn three_stage_config() -> CouncilPipelineConfig {
    CouncilPipelineConfig {
        stages: vec![
            PipelineStage {
                model_id: "gen".into(),
                role: CouncilRole::Generator,
                prompt_template: "{input}".into(),
                temperature: 0.7,
                top_p: 0.9,
                system_prompt: None,
            },
            PipelineStage {
                model_id: "audit".into(),
                role: CouncilRole::Auditor,
                prompt_template: "{input}".into(),
                temperature: 0.7,
                top_p: 0.9,
                system_prompt: None,
            },
            PipelineStage {
                model_id: "synth".into(),
                role: CouncilRole::Synthesizer,
                prompt_template: "{input}".into(),
                temperature: 0.7,
                top_p: 0.9,
                system_prompt: None,
            },
        ],
        ..Default::default()
    }
}

/// Run a pipeline with a pause controller and a background responder that
/// blocks the gate until `respond` fires. Returns the captured synthesizer
/// prompts and the debate outcome.
///
/// The pause is requested *before* the pipeline starts so the (instant mock
/// executor) pipeline is guaranteed to block at the gate. The main thread then
/// waits for the pipeline to reach the gate before calling `respond()`, which
/// mirrors the real UI: the user clicks "Chime In", the pipeline blocks at the
/// next gate, and the user resumes with feedback.
fn run_with_pause(
    controller: CouncilPauseController,
    respond: impl FnOnce() + Send + 'static,
) -> (Vec<String>, DebateOutcome) {
    let synth_prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // Request the pause BEFORE the pipeline starts so the pipeline blocks at
    // the gate.
    controller.request_pause();

    let engine = CouncilEngine::new(
        three_stage_config(),
        RecordingExecutor::new(
            vec![
                "Generated draft".into(),
                "Audit passed".into(),
                "Final synthesized consensus".into(),
            ],
            synth_prompts.clone(),
        ),
    )
    .with_pause_controller(controller.clone());

    // Run the pipeline in a background thread so we can wait for the gate.
    let prompts_clone = synth_prompts.clone();
    let pipeline_thread = std::thread::spawn(move || {
        let outcome = engine.execute("test prompt");
        let prompts = prompts_clone.lock().unwrap().clone();
        (prompts, outcome)
    });

    // Wait for the pipeline to reach the gate (it will block there because
    // `pause_requested` is already true).
    controller.wait_until_paused();

    // Now resume the pipeline with feedback (or without).
    respond();

    // Wait for the pipeline to finish.
    let (prompts, outcome) = pipeline_thread.join().unwrap();
    (prompts, outcome)
}

#[test]
fn test_no_pause_runs_synthesizer_without_feedback() {
    // No pause controller attached: the pipeline runs to completion
    // synchronously and never blocks. The synthesizer prompt must not
    // contain any human guidance.
    let synth_prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let engine = CouncilEngine::new(
        three_stage_config(),
        RecordingExecutor::new(
            vec![
                "Generated draft".into(),
                "Audit passed".into(),
                "Final synthesized consensus".into(),
            ],
            synth_prompts.clone(),
        ),
    );
    let outcome = engine.execute("test prompt");
    let prompts = synth_prompts.lock().unwrap().clone();

    assert_eq!(prompts.len(), 1, "synthesizer should run once");
    assert!(!prompts[0].contains("Human guidance"));
    matches!(outcome, DebateOutcome::Success { .. });
}

#[test]
fn test_pause_then_resume_with_feedback_injects_guidance() {
    let controller = CouncilPauseController::new();
    let (prompts, outcome) = run_with_pause(controller.clone(), move || {
        controller.request_pause();
        controller.resume_with_feedback(Some("Please handle edge cases".into()));
    });

    assert_eq!(prompts.len(), 1, "synthesizer should run once after resume");
    assert!(
        prompts[0].contains("Please handle edge cases"),
        "feedback should be injected into synthesizer prompt, got: {}",
        prompts[0]
    );
    assert!(
        prompts[0].contains("Human guidance (authoritative)"),
        "feedback should carry the authoritative header"
    );
    matches!(outcome, DebateOutcome::Success { .. });
}

#[test]
fn test_pause_then_resume_without_changes_runs_normally() {
    let controller = CouncilPauseController::new();
    let (prompts, outcome) = run_with_pause(controller.clone(), move || {
        controller.request_pause();
        controller.resume_without_changes();
    });

    assert_eq!(prompts.len(), 1);
    assert!(!prompts[0].contains("Human guidance"));
    matches!(outcome, DebateOutcome::Success { .. });
}

#[test]
fn test_pause_decision_consumed_once() {
    let controller = CouncilPauseController::new();
    controller.request_pause();
    controller.resume_with_feedback(Some("once".into()));

    // First gate consumes the decision.
    match controller.await_stage_gate() {
        PauseDecision::ResumeWithFeedback { feedback } => {
            assert_eq!(feedback.as_deref(), Some("once"));
        }
        other => panic!("expected feedback resume, got {other:?}"),
    }
    // Second gate sees no pending decision and resumes as a no-op.
    assert_eq!(
        controller.await_stage_gate(),
        PauseDecision::ResumeWithoutChanges
    );
}
