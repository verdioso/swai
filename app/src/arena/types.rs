//! SWAI — Arena live-stream UI models.
//!
//! UI-side representations of the live council pipeline. These types are the
//! bridge between the background `CouncilEvent` broadcast (produced by
//! `swai_core::council`) and the GTK widgets that render a debate as it
//! streams. They are plain data: no GTK, no threading — just the shape of the
//! state the live view needs to mutate.
//!
//! The authoritative module name for the streaming layer is `stream.rs`; this
//! file only defines the data contracts it consumes.

/// Lifecycle of a single stage within a live debate.
///
/// Mirrors the stage transition implied by the `CouncilEvent` stream: a stage
/// `Started` streams `TokenChunk`s and then settles into `Completed` (or
/// `Failed`). The UI uses this to drive the pulse badge and status label.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StageStatus {
    /// Stage is running and accepting tokens.
    #[default]
    Running,
    /// Stage finished successfully with a final text.
    Completed { full_text: String },
    /// Stage errored; carries the human-readable failure.
    Failed { error: String },
}

/// An action the live view applies to its per-stage card state.
///
/// Each action is cheap to clone and maps directly onto a `CouncilEvent`
/// variant, so the GLib-channel bridge can translate one event into one action
/// without allocating extra bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub enum ArenaStreamAction {
    /// Begin a stage at `stage_index` for the given role + model label.
    StageStarted {
        /// Zero-based stage index within the pipeline.
        stage_index: usize,
        /// Human-readable role label (e.g. "Generator", "Auditor", "Synthesizer").
        role_label: String,
        /// Model id shown in the card header.
        model_id: String,
    },
    /// Append `text` to the running buffer of the stage at `stage_index`.
    AppendToken {
        /// Zero-based stage index.
        stage_index: usize,
        /// Token fragment to append.
        text: String,
    },
    /// Mark the stage at `stage_index` completed with its final text.
    SetStageStatus {
        /// Zero-based stage index.
        stage_index: usize,
        /// Terminal status for the stage.
        status: StageStatus,
    },
    /// A human ("Chime In") intervened before the stage at `stage_index`,
    /// optionally injecting authoritative guidance. `feedback` is empty when
    /// the human resumed without changes. Rendered with a distinct badge.
    HumanIntervention {
        /// Zero-based stage index the pipeline is about to resume into.
        stage_index: usize,
        /// The human's guidance text, if any.
        feedback: String,
    },
}

/// Map a core `CouncilRole` to a short, stable UI label.
///
/// Kept here (rather than in `stream.rs`) so both the translation layer and the
/// widget share a single source of truth for role names.
pub fn role_label(role: &swai_core::council::CouncilRole) -> String {
    match role {
        swai_core::council::CouncilRole::Planner => "Planner".to_string(),
        swai_core::council::CouncilRole::Generator => "Generator".to_string(),
        swai_core::council::CouncilRole::Auditor => "Auditor".to_string(),
        swai_core::council::CouncilRole::Synthesizer => "Synthesizer".to_string(),
        swai_core::council::CouncilRole::Custom(name) => format!("Custom: {name}"),
    }
}

/// Translate a core `CouncilEvent` into the UI action the live view applies.
///
/// Returns `None` for events that carry no per-stage UI mutation
/// (`PipelineCompleted`, `PipelineFailed` without a stage index), letting the
/// caller ignore them without special-casing.
pub fn event_to_action(event: &swai_core::council::CouncilEvent) -> Option<ArenaStreamAction> {
    use swai_core::council::CouncilEvent;
    match event {
        CouncilEvent::StageStarted {
            stage_index,
            role,
            model_id,
            ..
        } => Some(ArenaStreamAction::StageStarted {
            stage_index: *stage_index,
            role_label: role_label(role),
            model_id: model_id.clone(),
        }),
        CouncilEvent::TokenChunk {
            stage_index, text, ..
        } => Some(ArenaStreamAction::AppendToken {
            stage_index: *stage_index,
            text: text.clone(),
        }),
        CouncilEvent::StageCompleted {
            stage_index,
            full_text,
            ..
        } => Some(ArenaStreamAction::SetStageStatus {
            stage_index: *stage_index,
            status: StageStatus::Completed {
                full_text: full_text.clone(),
            },
        }),
        CouncilEvent::PipelineFailed {
            stage_index, error, ..
        } => Some(ArenaStreamAction::SetStageStatus {
            stage_index: *stage_index,
            status: StageStatus::Failed {
                error: error.clone(),
            },
        }),
        CouncilEvent::PipelineCompleted { .. } => None,
        CouncilEvent::HumanIntervention {
            stage_index,
            feedback,
        } => Some(ArenaStreamAction::HumanIntervention {
            stage_index: *stage_index,
            feedback: feedback.clone(),
        }),
    }
}

/// Extract the stage index an action targets.
///
/// Kept in this pure module (rather than the GTK-dependent `window.rs`) so both
/// the bridge and the tests share one source of truth.
#[allow(dead_code)]
pub fn stage_index_of(action: &ArenaStreamAction) -> usize {
    match action {
        ArenaStreamAction::StageStarted { stage_index, .. }
        | ArenaStreamAction::AppendToken { stage_index, .. }
        | ArenaStreamAction::SetStageStatus { stage_index, .. }
        | ArenaStreamAction::HumanIntervention { stage_index, .. } => *stage_index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swai_core::council::CouncilEvent;

    fn gen_started() -> CouncilEvent {
        CouncilEvent::StageStarted {
            stage_index: 0,
            role: swai_core::council::CouncilRole::Generator,
            model_id: "gen".into(),
            model_name: "gen".into(),
        }
    }

    #[test]
    fn test_role_labels() {
        assert_eq!(
            role_label(&swai_core::council::CouncilRole::Generator),
            "Generator"
        );
        assert_eq!(
            role_label(&swai_core::council::CouncilRole::Auditor),
            "Auditor"
        );
        assert_eq!(
            role_label(&swai_core::council::CouncilRole::Synthesizer),
            "Synthesizer"
        );
        assert_eq!(
            role_label(&swai_core::council::CouncilRole::Custom("critic".into())),
            "Custom: critic"
        );
    }

    #[test]
    fn test_event_to_action_mapping() {
        let started = event_to_action(&gen_started()).unwrap();
        assert_eq!(
            started,
            ArenaStreamAction::StageStarted {
                stage_index: 0,
                role_label: "Generator".into(),
                model_id: "gen".into(),
            }
        );

        let chunk = event_to_action(&CouncilEvent::TokenChunk {
            stage_index: 1,
            text: "hello".into(),
        })
        .unwrap();
        assert_eq!(
            chunk,
            ArenaStreamAction::AppendToken {
                stage_index: 1,
                text: "hello".into(),
            }
        );

        let done = event_to_action(&CouncilEvent::StageCompleted {
            stage_index: 1,
            full_text: "done".into(),
            duration_sec: 1.0,
            tok_per_sec: 1.0,
        })
        .unwrap();
        assert_eq!(
            done,
            ArenaStreamAction::SetStageStatus {
                stage_index: 1,
                status: StageStatus::Completed {
                    full_text: "done".into(),
                },
            }
        );

        let failed = event_to_action(&CouncilEvent::PipelineFailed {
            stage_index: 2,
            error: "boom".into(),
        })
        .unwrap();
        assert_eq!(
            failed,
            ArenaStreamAction::SetStageStatus {
                stage_index: 2,
                status: StageStatus::Failed {
                    error: "boom".into(),
                },
            }
        );

        // Terminal events carry no per-stage mutation.
        assert_eq!(
            event_to_action(&CouncilEvent::PipelineCompleted {
                full_transcript: swai_core::council::DebateTranscript::new(
                    "s".into(),
                    "p".into(),
                    Default::default()
                )
            }),
            None
        );

        // Human intervention maps to a dedicated action carrying the guidance.
        let intervention = event_to_action(&CouncilEvent::HumanIntervention {
            stage_index: 2,
            feedback: "Handle edge cases".into(),
        })
        .unwrap();
        assert_eq!(
            intervention,
            ArenaStreamAction::HumanIntervention {
                stage_index: 2,
                feedback: "Handle edge cases".into(),
            }
        );
        // Resuming without changes carries empty feedback.
        let no_feedback = event_to_action(&CouncilEvent::HumanIntervention {
            stage_index: 2,
            feedback: String::new(),
        })
        .unwrap();
        assert_eq!(
            no_feedback,
            ArenaStreamAction::HumanIntervention {
                stage_index: 2,
                feedback: String::new()
            }
        );
    }
}
