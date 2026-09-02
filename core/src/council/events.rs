//! SWAI — Council async broadcast event definitions.
//!
//! Defines the `CouncilEvent` enum emitted over a `tokio::sync::broadcast`
//! channel so the UI can observe real-time progress during a council debate:
//! stage transitions, streaming token chunks, per-stage metrics, and the
//! final synthesized transcript.
//!
//! Emission order is strictly:
//! `StageStarted` -> `TokenChunk`(s) -> `StageCompleted` -> `PipelineCompleted`.

use crate::council::types::DebateTranscript;
use serde::{Deserialize, Serialize};

/// A real-time event emitted by the council pipeline.
///
/// Events are broadcast to every active subscriber (e.g. the UI) through a
/// `tokio::sync::broadcast::Sender<CouncilEvent>`. All variants are cheap to
/// clone and `Send + Sync`, so they can cross thread boundaries freely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CouncilEvent {
    /// A stage is about to run. Emitted right before initiating the HTTP
    /// connection to the stage's model port.
    StageStarted {
        /// Zero-based index of the stage within the pipeline.
        stage_index: usize,
        /// Role this stage plays.
        role: crate::council::types::CouncilRole,
        /// Identifier for the model being invoked.
        model_id: String,
        /// Human-readable model name.
        model_name: String,
    },
    /// A single streaming token chunk received from the model. Emitted for
    /// every SSE delta token produced by the active stage.
    TokenChunk {
        /// Zero-based index of the stage producing the token.
        stage_index: usize,
        /// The token text fragment.
        text: String,
    },
    /// A stage finished streaming. Emitted after the final token of a stage.
    StageCompleted {
        /// Zero-based index of the stage that completed.
        stage_index: usize,
        /// The full text produced by the stage.
        full_text: String,
        /// Wall-clock duration of the stage in seconds.
        duration_sec: f64,
        /// Tokens generated per second (0.0 when duration is zero).
        tok_per_sec: f64,
    },
    /// The entire pipeline finished. Emitted once, after the last stage.
    PipelineCompleted {
        /// The fully synthesized transcript of the debate.
        full_transcript: DebateTranscript,
    },
    /// The pipeline failed. Emitted when a stage errors (subject to the
    /// pipeline's fallback policy).
    PipelineFailed {
        /// Zero-based index of the stage that failed.
        stage_index: usize,
        /// Human-readable error description.
        error: String,
    },
}

impl CouncilEvent {
    /// A short, stable category label for the event. Useful for the UI to
    /// route events to the correct panel without pattern matching.
    pub fn kind(&self) -> &'static str {
        match self {
            CouncilEvent::StageStarted { .. } => "stage_started",
            CouncilEvent::TokenChunk { .. } => "token_chunk",
            CouncilEvent::StageCompleted { .. } => "stage_completed",
            CouncilEvent::PipelineCompleted { .. } => "pipeline_completed",
            CouncilEvent::PipelineFailed { .. } => "pipeline_failed",
        }
    }

    /// The zero-based stage index associated with the event, if any.
    ///
    /// `PipelineCompleted` and `PipelineFailed` do not map to a single stage,
    /// so they return `None`.
    pub fn stage_index(&self) -> Option<usize> {
        match self {
            CouncilEvent::StageStarted { stage_index, .. }
            | CouncilEvent::TokenChunk { stage_index, .. }
            | CouncilEvent::StageCompleted { stage_index, .. }
            | CouncilEvent::PipelineFailed { stage_index, .. } => Some(*stage_index),
            CouncilEvent::PipelineCompleted { .. } => None,
        }
    }
}
