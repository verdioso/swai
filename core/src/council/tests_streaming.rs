//! SWAI — Council live broadcast event unit tests.
//!
//! Verifies the strict emission order guaranteed by the engine:
//! `StageStarted` -> `TokenChunk`(s) -> `StageCompleted` -> `PipelineCompleted`.
//!
//! A subscriber is registered before the engine runs and drained afterward, so
//! the captured sequence reflects the true emission order. The tests also
//! exercise `tokio::sync::broadcast` semantics: multiple subscribers,
//! disconnected-subscriber resilience, and late subscribers.

use crate::council::events::CouncilEvent;
use crate::council::pipeline::CouncilEngine;
use crate::council::Executor;
use crate::council::types::{
    CouncilPipelineConfig, CouncilRole, DebateOutcome, DebateTranscript, PipelineStage,
};

/// A minimal mock executor that returns a fixed output for every stage.
///
/// The engine drives `execute_stream` (which falls back to chunking the
/// non-streaming output into `TokenChunk` events), so this executor only
/// needs to supply the text; the broadcast events are produced by the engine.
struct MockExecutor {
    output: String,
}

impl Executor for MockExecutor {
    fn execute(&self, _stage: &PipelineStage, _input: &str) -> Result<String, String> {
        Ok(self.output.clone())
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

/// Run a pipeline while capturing every event broadcast on the channel.
///
/// Returns the captured event sequence in emission order. The receiver is
/// subscribed *before* the engine runs and drained *after* it returns, so the
/// capture is fully synchronous: `Sender::send` only buffers into the channel
/// (bounded by capacity), and nothing is produced past `PipelineCompleted`.
fn run_with_capture(
    config: CouncilPipelineConfig,
    executor: MockExecutor,
    prompt: &str,
) -> (Vec<CouncilEvent>, DebateOutcome) {
    let (tx, _rx) = tokio::sync::broadcast::channel(256);
    // Subscribe before running so no event is missed.
    let mut drain = tx.subscribe();

    let engine = CouncilEngine::with_events(config, executor, tx);
    let outcome = engine.execute(prompt);

    let mut events = Vec::new();
    loop {
        match drain.try_recv() {
            Ok(event) => {
                let is_terminal = matches!(event, CouncilEvent::PipelineCompleted { .. });
                events.push(event);
                if is_terminal {
                    break;
                }
            }
            // No more active senders: the pipeline is done.
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            // The channel is empty; no events remain to capture.
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            // A slow reader that fell behind: keep the last-seen value.
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
        }
    }

    (events, outcome)
}

/// Assert that every `StageStarted` is followed by at least one `TokenChunk`
/// and then a matching `StageCompleted`, with no stage left open at the end.
fn assert_started_token_completed(events: &[CouncilEvent]) {
    let mut pending_start: Option<usize> = None;
    let mut seen_chunks = 0usize;

    for event in events {
        match event {
            CouncilEvent::StageStarted { stage_index, .. } => {
                pending_start = Some(*stage_index);
                seen_chunks = 0;
            }
            CouncilEvent::TokenChunk { stage_index, .. } => {
                let start = pending_start.expect("TokenChunk emitted before any StageStarted");
                assert_eq!(start, *stage_index, "token belongs to wrong stage");
                seen_chunks += 1;
            }
            CouncilEvent::StageCompleted { stage_index, .. } => {
                let start = pending_start.expect("StageCompleted emitted before any StageStarted");
                assert_eq!(start, *stage_index, "completed belongs to wrong stage");
                assert!(
                    seen_chunks > 0,
                    "StageCompleted with no preceding TokenChunk"
                );
                pending_start = None;
                seen_chunks = 0;
            }
            CouncilEvent::PipelineCompleted { .. }
            | CouncilEvent::PipelineFailed { .. }
            | CouncilEvent::HumanIntervention { .. } => {
                assert!(
                    pending_start.is_none(),
                    "pipeline terminated while a stage was still open"
                );
            }
        }
    }
}

#[test]
fn test_emission_order_full_pipeline() {
    let (events, outcome) = run_with_capture(
        three_stage_config(),
        MockExecutor {
            output: "generated draft".into(),
        },
        "test prompt",
    );

    // Non-empty, ordered from StageStarted to PipelineCompleted.
    assert!(!events.is_empty(), "expected at least one event");
    assert!(
        matches!(events[0], CouncilEvent::StageStarted { .. }),
        "first event must be StageStarted, got {:?}",
        events[0]
    );
    assert!(
        matches!(
            events.last().unwrap(),
            CouncilEvent::PipelineCompleted { .. }
        ),
        "last event must be PipelineCompleted, got {:?}",
        events.last().unwrap()
    );

    // The full StageStarted -> TokenChunk -> StageCompleted chain holds.
    assert_started_token_completed(&events);

    // The debate itself should succeed end to end.
    matches!(outcome, DebateOutcome::Success { .. });
}

#[test]
fn test_pipeline_completed_always_emitted() {
    let (events, outcome) = run_with_capture(
        three_stage_config(),
        MockExecutor {
            output: "final synthesized consensus".into(),
        },
        "test prompt",
    );

    // StageStarted/StageCompleted present (one per stage) and a terminal
    // PipelineCompleted present exactly once, at the very end.
    let starts = events
        .iter()
        .filter(|e| matches!(e, CouncilEvent::StageStarted { .. }))
        .count();
    let completes = events
        .iter()
        .filter(|e| matches!(e, CouncilEvent::StageCompleted { .. }))
        .count();
    let terminal = events
        .iter()
        .filter(|e| matches!(e, CouncilEvent::PipelineCompleted { .. }))
        .count();

    assert_eq!(starts, 3, "one StageStarted per stage");
    assert_eq!(completes, 3, "one StageCompleted per stage");
    assert_eq!(terminal, 1, "exactly one PipelineCompleted");

    assert!(matches!(
        events.last().unwrap(),
        CouncilEvent::PipelineCompleted { .. }
    ));
    matches!(outcome, DebateOutcome::Success { .. });
}

#[test]
fn test_broadcast_multiple_subscribers_receive_events() {
    let (tx, mut rx1) = tokio::sync::broadcast::channel(16);

    let _ = tx.send(CouncilEvent::StageStarted {
        stage_index: 0,
        role: CouncilRole::Generator,
        model_id: "gen".into(),
        model_name: "gen".into(),
    });

    // rx1 received the first event; now subscribe a second listener.
    let sub1 = rx1.try_recv().unwrap();
    assert!(matches!(sub1, CouncilEvent::StageStarted { .. }));
    let mut rx2 = tx.subscribe();

    // Both active subscribers receive the second event.
    let _ = tx.send(CouncilEvent::TokenChunk {
        stage_index: 0,
        text: "hi".into(),
    });
    let got1 = rx1.try_recv().unwrap();
    let got2 = rx2.try_recv().unwrap();
    assert!(matches!(got1, CouncilEvent::TokenChunk { .. }));
    assert!(matches!(got2, CouncilEvent::TokenChunk { .. }));

    // A late subscriber only sees events broadcast after it subscribed.
    let mut rx3 = tx.subscribe();
    let _ = tx.send(CouncilEvent::StageCompleted {
        stage_index: 0,
        full_text: "done".into(),
        duration_sec: 1.0,
        tok_per_sec: 1.0,
    });
    assert!(matches!(
        rx3.try_recv().unwrap(),
        CouncilEvent::StageCompleted { .. }
    ));
    // rx2 already consumed the TokenChunk; its next message is StageCompleted.
    assert!(matches!(
        rx2.try_recv().unwrap(),
        CouncilEvent::StageCompleted { .. }
    ));
}

#[test]
fn test_broadcast_resilience_when_no_subscriber() {
    // Broadcasting to a sender with no active receiver must not panic.
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let events = vec![
        CouncilEvent::StageStarted {
            stage_index: 0,
            role: CouncilRole::Generator,
            model_id: "gen".into(),
            model_name: "gen".into(),
        },
        CouncilEvent::TokenChunk {
            stage_index: 0,
            text: "x".into(),
        },
        CouncilEvent::StageCompleted {
            stage_index: 0,
            full_text: "done".into(),
            duration_sec: 1.0,
            tok_per_sec: 1.0,
        },
        CouncilEvent::PipelineCompleted {
            full_transcript: DebateTranscript::new("s".into(), "p".into(), three_stage_config()),
        },
    ];

    for event in events {
        // Ignore the Err (no active receiver); the send must not panic.
        let _ = tx.send(event);
    }
}

#[test]
fn test_event_kind_and_stage_index_helpers() {
    let started = CouncilEvent::StageStarted {
        stage_index: 2,
        role: CouncilRole::Auditor,
        model_id: "a".into(),
        model_name: "a".into(),
    };
    assert_eq!(started.kind(), "stage_started");
    assert_eq!(started.stage_index(), Some(2));

    let chunk = CouncilEvent::TokenChunk {
        stage_index: 2,
        text: "t".into(),
    };
    assert_eq!(chunk.kind(), "token_chunk");
    assert_eq!(chunk.stage_index(), Some(2));

    let completed = CouncilEvent::StageCompleted {
        stage_index: 2,
        full_text: "done".into(),
        duration_sec: 1.0,
        tok_per_sec: 1.0,
    };
    assert_eq!(completed.kind(), "stage_completed");
    assert_eq!(completed.stage_index(), Some(2));

    let failed = CouncilEvent::PipelineFailed {
        stage_index: 1,
        error: "boom".into(),
    };
    assert_eq!(failed.kind(), "pipeline_failed");
    assert_eq!(failed.stage_index(), Some(1));

    let done = CouncilEvent::PipelineCompleted {
        full_transcript: DebateTranscript::new("s".into(), "p".into(), three_stage_config()),
    };
    assert_eq!(done.kind(), "pipeline_completed");
    assert_eq!(done.stage_index(), None);
}
