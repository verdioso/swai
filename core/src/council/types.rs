//! SWAI — Council data types and serialization structures.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Execution mode for a council pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CouncilMode {
    /// Stages run sequentially, one after another.
    #[default]
    Sequential,
    /// All stages with the same role run concurrently.
    Concurrent,
    /// Choose between sequential and concurrent based on runtime conditions.
    Auto,
}

/// Role a council agent plays in a pipeline stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CouncilRole {
    Planner,
    Generator,
    Auditor,
    Synthesizer,
    Custom(String),
}

/// A single stage in a council pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineStage {
    /// Identifier for the model to use (e.g. "llama3-8b").
    pub model_id: String,
    /// Role this stage plays.
    #[serde(default = "default_generator")]
    pub role: CouncilRole,
    /// Prompt template string with `{input}` placeholder.
    #[serde(default)]
    pub prompt_template: String,
    /// Sampling temperature (0.0–2.0). Defaults to 0.7.
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Top-p sampling parameter. Defaults to 0.9.
    #[serde(default = "default_top_p")]
    pub top_p: f32,
    /// Optional system prompt override for this stage.
    #[serde(default)]
    pub system_prompt: Option<String>,
}

fn default_generator() -> CouncilRole {
    CouncilRole::Generator
}
fn default_temperature() -> f32 {
    0.7
}
fn default_top_p() -> f32 {
    0.9
}

/// Fallback behavior when a stage fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FallbackAction {
    /// Skip the failing stage and continue with the next.
    #[default]
    Skip,
    /// Retry the stage up to `max_retries` times.
    Retry { max_retries: u32 },
    /// Abort the entire pipeline on failure.
    Abort,
}

/// Complete council pipeline configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CouncilPipelineConfig {
    /// Ordered list of stages to execute.
    pub stages: Vec<PipelineStage>,
    /// Execution mode for the pipeline.
    #[serde(default)]
    pub mode: CouncilMode,
    /// What to do when a stage fails.
    #[serde(default)]
    pub fallback: FallbackAction,
    /// Per-role overrides (e.g. custom system prompts).
    #[serde(default)]
    pub role_overrides: HashMap<String, String>,
}

/// Result of a single council turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnResult {
    /// Sequence number of this turn.
    pub turn_index: usize,
    /// Role that produced this result.
    pub role: CouncilRole,
    /// Model ID used for generation.
    pub model_id: String,
    /// Generated output text.
    pub output: String,
    /// Wall-clock duration of the generation.
    #[serde(with = "duration_secs")]
    pub duration: Duration,
    /// Error details if the turn failed (None on success).
    pub error: Option<String>,
}

/// Serde helper for Duration as seconds f64.
mod duration_secs {
    use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(dur: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let secs = dur.as_secs_f64();
        secs.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = f64::deserialize(deserializer)?;
        Ok(Duration::from_secs_f64(secs))
    }
}

/// Full transcript of a council debate session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DebateTranscript {
    /// Unique session ID for this debate.
    pub session_id: String,
    /// Original input prompt that started the debate.
    pub input_prompt: String,
    /// Pipeline config used for this debate.
    pub config: CouncilPipelineConfig,
    /// All turns produced during the debate.
    pub turns: Vec<TurnResult>,
}

impl DebateTranscript {
    /// Create a new empty transcript for a session.
    pub fn new(session_id: String, input_prompt: String, config: CouncilPipelineConfig) -> Self {
        Self {
            session_id,
            input_prompt,
            config,
            turns: Vec::new(),
        }
    }

    /// Append a turn to the transcript.
    pub fn append_turn(&mut self, turn: TurnResult) {
        self.turns.push(turn);
    }

    /// Number of turns recorded.
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// Whether all turns succeeded (no errors).
    pub fn all_succeeded(&self) -> bool {
        self.turns.iter().all(|t| t.error.is_none())
    }
}

/// Outcome of a council debate execution.
#[derive(Debug, Clone)]
pub enum DebateOutcome {
    /// All stages completed successfully.
    Success {
        final_response: String,
        transcript: DebateTranscript,
    },
    /// One or more stages failed; best available draft returned with warnings.
    Partial {
        fallback_response: String,
        warnings: Vec<String>,
        transcript: DebateTranscript,
    },
    /// Pipeline aborted due to fatal error (e.g. generator failure with Abort fallback).
    Aborted {
        reason: String,
        transcript: DebateTranscript,
    },
}
