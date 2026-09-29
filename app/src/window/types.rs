use swai_core::process_manager::{ModelState, ProcessError};

#[derive(Debug)]
pub enum ChannelMessage {
    /// A switch (start or switch_model) completed.
    SwitchCompleted {
        target_id: String,
        result: Result<(), ProcessError>,
    },
    /// A stop completed.
    StopCompleted {
        running_id: String,
        result: Result<(), ProcessError>,
    },
    /// A restart was manually triggered by the user via the Restart button.
    RestartRequested { model_id: String },
    /// Intermediate state update from health monitor polling.
    /// Used to drive Starting → Loading → Ready transitions in the UI.
    StateUpdate { model_id: String, state: ModelState },
}

/// Messages sent from UI dialogs (import wizard) to the main GUI thread.
#[derive(Debug, Clone)]
pub enum ImportMessage {
    /// A new model was imported and its card should be appended.
    ModelImported {
        model: swai_core::config::ModelConfig,
    },
    /// A model's details (name, port) were updated - refresh the card label live.
    ModelNameUpdated { id: String, name: String, port: u16 },
    /// A model was deleted - remove its card from the UI.
    ModelDeleted { id: String },
}

/// Context update sent from the polling thread to the main loop.
#[derive(Debug, Clone)]
pub struct SlotUpdate {
    pub model_id: String,
    pub tokens_used: usize,
    pub n_ctx: usize,
    pub predicted_per_second: f64,
    pub prompt_per_second: f64,
    pub prompt_tokens: usize,
    pub decoded_tokens: usize,
    pub is_processing: bool,
    /// Live elapsed duration in seconds. `None` when no active request.
    /// `Some(0.0)` means the request just started. `Some(t)` is the latched total
    /// when generation finishes (processing → idle).
    pub elapsed_duration_sec: Option<f64>,
    pub total_tokens: usize,
}

/// A polled /slots response for a single model.
#[derive(Debug, Clone)]
pub struct SlotInfo {
    pub tokens_used: usize,
    pub n_ctx: usize,
    pub predicted_per_second: f64,
    pub prompt_per_second: f64,
    pub prompt_tokens: usize,
    pub decoded_tokens: usize,
    /// Whether the slot is currently processing (has active tokens).
    pub is_processing: bool,
}

/// Stage telemetry in a Council debate.
#[derive(Debug, Clone, Default)]
pub struct StageTelemetry {
    pub stage_name: String,
    #[allow(dead_code)]
    pub model_id: String,
    pub output_tokens: usize,
    pub duration_sec: f64,
    #[allow(dead_code)]
    pub speed: f64,
}

/// Persistent telemetry metrics for a single model or council pipeline.
#[derive(Debug, Clone, Default)]
pub struct ModelTelemetry {
    #[allow(dead_code)]
    pub model_id: String,
    pub prompt_tokens: usize,
    pub prompt_speed: f64,
    pub prompt_duration_sec: f64,
    pub decode_tokens: usize,
    pub decode_speed: f64,
    pub elapsed_duration_sec: Option<f64>,
    pub is_processing: bool,
    pub council_stages: Vec<StageTelemetry>,
    pub current_stage: Option<String>,
}
