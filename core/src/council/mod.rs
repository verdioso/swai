//! SWAI — Council multi-agent orchestration module.
//!
//! Defines data types, pipeline configuration, and debate transcript
//! structures for coordinating multiple LLM agents in a council pattern.

pub mod events;
pub mod pipeline;
pub mod streaming;
pub mod types;
pub mod vram;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_pipeline;
#[cfg(test)]
mod tests_sse;
#[cfg(test)]
mod tests_streaming;

pub use events::CouncilEvent;
pub use pipeline::{CouncilEngine, CouncilError, Executor};
pub use types::{
    CouncilMode, CouncilPipelineConfig, CouncilRole, DebateOutcome, DebateTranscript,
    FallbackAction, PipelineStage, TurnResult,
};
pub use vram::{get_available_vram_bytes, recommend_mode};
