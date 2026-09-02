//! SWAI — Arena live-stream unit tests.
//!
//! Exercises the pure `StageState` state machine and the `types.rs` event
//! translation, verifying stage bubble text accumulation and stream state
//! transitions without requiring a GTK display.

use super::stream::StageState;
use super::types::{event_to_action, stage_index_of, ArenaStreamAction, StageStatus};
use std::collections::BTreeMap;
use swai_core::council::CouncilEvent;

/// A full three-stage debate: generator -> auditor -> synthesizer.
fn full_pipeline_actions() -> Vec<ArenaStreamAction> {
    let events = vec![
        CouncilEvent::StageStarted {
            stage_index: 0,
            role: swai_core::council::CouncilRole::Generator,
            model_id: "gen".into(),
            model_name: "gen".into(),
        },
        CouncilEvent::TokenChunk {
            stage_index: 0,
            text: "The ".into(),
        },
        CouncilEvent::TokenChunk {
            stage_index: 0,
            text: "answer".into(),
        },
        CouncilEvent::StageCompleted {
            stage_index: 0,
            full_text: "The answer".into(),
            duration_sec: 1.0,
            tok_per_sec: 2.0,
        },
        CouncilEvent::StageStarted {
            stage_index: 1,
            role: swai_core::council::CouncilRole::Auditor,
            model_id: "critic".into(),
            model_name: "critic".into(),
        },
        CouncilEvent::TokenChunk {
            stage_index: 1,
            text: "Review".into(),
        },
        CouncilEvent::StageCompleted {
            stage_index: 1,
            full_text: "Review".into(),
            duration_sec: 0.5,
            tok_per_sec: 2.0,
        },
        CouncilEvent::StageStarted {
            stage_index: 2,
            role: swai_core::council::CouncilRole::Synthesizer,
            model_id: "synth".into(),
            model_name: "synth".into(),
        },
        CouncilEvent::TokenChunk {
            stage_index: 2,
            text: "Consensus".into(),
        },
        CouncilEvent::StageCompleted {
            stage_index: 2,
            full_text: "Consensus".into(),
            duration_sec: 2.0,
            tok_per_sec: 0.5,
        },
        CouncilEvent::PipelineCompleted {
            full_transcript: swai_core::council::DebateTranscript::new(
                "debate-1".into(),
                "prompt".into(),
                Default::default(),
            ),
        },
    ];
    events.iter().filter_map(|e| event_to_action(e)).collect()
}

#[test]
fn test_stage_started_sets_generating_and_footer() {
    let mut state = StageState::default();
    assert!(!state.is_generating());
    assert_eq!(state.footer, "");

    state.apply(&ArenaStreamAction::StageStarted {
        stage_index: 0,
        role_label: "Generator".into(),
        model_id: "gen".into(),
    });

    assert!(state.is_generating());
    assert_eq!(state.stage_index, Some(0));
    assert_eq!(state.role_label.as_deref(), Some("Generator"));
    assert_eq!(state.model_id.as_deref(), Some("gen"));
    assert_eq!(state.footer, "generating…");
    assert!(state.text.is_empty());
}

#[test]
fn test_token_accumulation_only_for_active_stage() {
    let mut state = StageState::default();
    state.apply(&ArenaStreamAction::StageStarted {
        stage_index: 0,
        role_label: "Generator".into(),
        model_id: "gen".into(),
    });

    // Tokens for the active stage accumulate in order.
    state.apply(&ArenaStreamAction::AppendToken {
        stage_index: 0,
        text: "Hello ".into(),
    });
    state.apply(&ArenaStreamAction::AppendToken {
        stage_index: 0,
        text: "world".into(),
    });
    assert_eq!(state.text, "Hello world");

    // A token for an inactive stage is dropped.
    state.apply(&ArenaStreamAction::AppendToken {
        stage_index: 5,
        text: "ignored".into(),
    });
    assert_eq!(state.text, "Hello world");
}

#[test]
fn test_stage_completion_stops_pulsing() {
    let mut state = StageState::default();
    state.apply(&ArenaStreamAction::StageStarted {
        stage_index: 0,
        role_label: "Generator".into(),
        model_id: "gen".into(),
    });
    state.apply(&ArenaStreamAction::AppendToken {
        stage_index: 0,
        text: "draft".into(),
    });
    assert!(state.is_generating());

    state.apply(&ArenaStreamAction::SetStageStatus {
        stage_index: 0,
        status: StageStatus::Completed {
            full_text: "draft".into(),
        },
    });

    assert!(!state.is_generating());
    assert_eq!(state.footer, "complete");
    assert_eq!(
        state.status,
        StageStatus::Completed {
            full_text: "draft".into()
        }
    );
}

#[test]
fn test_stage_failure_sets_error_footer() {
    let mut state = StageState::default();
    state.apply(&ArenaStreamAction::StageStarted {
        stage_index: 1,
        role_label: "Auditor".into(),
        model_id: "critic".into(),
    });

    state.apply(&ArenaStreamAction::SetStageStatus {
        stage_index: 1,
        status: StageStatus::Failed {
            error: "timeout".into(),
        },
    });

    assert!(!state.is_generating());
    assert_eq!(state.footer, "failed: timeout");
}

#[test]
fn test_full_pipeline_state_transitions() {
    let actions = full_pipeline_actions();
    // Every CouncilEvent must translate to a UI action (PipelineCompleted is
    // the only non-mutating event and is filtered out by `event_to_action`).
    assert_eq!(actions.len(), 10);

    // The real window assigns one StageBubbleCard per stage index, so mirror
    // that here: group actions by stage and drive an independent card per
    // stage. This verifies both text accumulation and per-stage state
    // transitions exactly as the live UI behaves.
    let mut cards: std::collections::BTreeMap<usize, StageState> = BTreeMap::new();
    for a in &actions {
        let idx = stage_index_of(a);
        cards.entry(idx).or_default().apply(a);
    }

    // Generator card: started, received tokens, completed, no longer pulsing.
    let gen = cards.get(&0).expect("generator card");
    assert_eq!(gen.stage_index, Some(0));
    assert!(gen.text.contains("The answer"));
    assert!(!gen.is_generating());
    assert_eq!(gen.footer, "complete");

    // Auditor card: started, received tokens, completed.
    let audit = cards.get(&1).expect("auditor card");
    assert_eq!(audit.stage_index, Some(1));
    assert!(audit.text.contains("Review"));
    assert!(!audit.is_generating());
    assert_eq!(audit.footer, "complete");

    // Synthesizer card: started, received tokens, completed.
    let synth = cards.get(&2).expect("synthesizer card");
    assert_eq!(synth.stage_index, Some(2));
    assert!(synth.text.contains("Consensus"));
    assert!(!synth.is_generating());
    assert_eq!(synth.footer, "complete");
}

#[test]
fn test_pipeline_completed_produces_no_action() {
    let event = CouncilEvent::PipelineCompleted {
        full_transcript: swai_core::council::DebateTranscript::new(
            "s".into(),
            "p".into(),
            Default::default(),
        ),
    };
    assert_eq!(event_to_action(&event), None);
}
