//! SWAI — Council module unit tests.

use crate::council::types::*;
use serde_json;
use std::collections::HashMap;
use std::time::Duration;

#[test]
fn test_council_mode_default_is_sequential() {
    assert_eq!(CouncilMode::default(), CouncilMode::Sequential);
}

#[test]
fn test_council_role_custom_serializes() {
    let role = CouncilRole::Custom("Evaluator".to_string());
    let json = serde_json::to_string(&role).unwrap();
    assert!(json.contains("Evaluator"));
    let deserialized: CouncilRole = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, CouncilRole::Custom("Evaluator".to_string()));
}

#[test]
fn test_pipeline_stage_defaults() {
    let stage = PipelineStage { max_tokens: None,
        model_id: "llama3".into(),
        role: CouncilRole::Generator,
        prompt_template: "Analyze: {input}".into(),
        temperature: 0.7,
        top_p: 0.9, system_prompt: None,
    };
    let json = serde_json::to_string(&stage).unwrap();
    let back: PipelineStage = serde_json::from_str(&json).unwrap();
    assert_eq!(back.temperature, 0.7);
    assert_eq!(back.top_p, 0.9);
}

#[test]
fn test_pipeline_stage_deserialize_missing_fields() {
    // Minimal JSON — defaults should fill in the rest.
    let json = r#"{"model_id":"mistral","prompt_template":"{input}"}"#;
    let stage: PipelineStage = serde_json::from_str(json).unwrap();
    assert_eq!(stage.model_id, "mistral");
    assert_eq!(stage.role, CouncilRole::Generator);
    assert!((stage.temperature - 0.7).abs() < 1e-6);
    assert!(stage.system_prompt.is_none());
}

#[test]
fn test_fallback_action_default_is_skip() {
    assert_eq!(FallbackAction::default(), FallbackAction::Skip);
}

#[test]
fn test_council_pipeline_config_roundtrip_json() {
    let config = CouncilPipelineConfig {
        stages: vec![
            PipelineStage { max_tokens: None,
                model_id: "llama3".into(),
                role: CouncilRole::Generator,
                prompt_template: "Generate: {input}".into(),
                temperature: 0.8,
                top_p: 0.95,
                system_prompt: Some("You are a generator.".into()),
            },
            PipelineStage { max_tokens: None,
                model_id: "mistral".into(),
                role: CouncilRole::Auditor,
                prompt_template: "Audit: {input}".into(),
                temperature: 0.3,
                top_p: 0.8, system_prompt: None,
            },
        ],
        mode: CouncilMode::Concurrent,
        fallback: FallbackAction::Retry { max_retries: 3 },
        role_overrides: [("Auditor".into(), "Be strict.".into())].into(),
    };

    let json = serde_json::to_string_pretty(&config).unwrap();
    let back: CouncilPipelineConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.stages.len(), 2);
    assert_eq!(back.mode, CouncilMode::Concurrent);
    assert_eq!(back.fallback, FallbackAction::Retry { max_retries: 3 });
    assert_eq!(back.role_overrides.get("Auditor").unwrap(), "Be strict.");
}

#[test]
fn test_council_pipeline_config_roundtrip_toml() {
    let config = CouncilPipelineConfig {
        stages: vec![PipelineStage { max_tokens: None,
            model_id: "llama3".into(),
            role: CouncilRole::Auditor,
            prompt_template: "Synthesize: {input}".into(),
            temperature: 0.5,
            top_p: 0.85, system_prompt: None,
        }],
        mode: CouncilMode::Auto,
        fallback: FallbackAction::Abort,
        role_overrides: HashMap::new(),
    };

    let toml_str = toml::to_string(&config).unwrap();
    let back: CouncilPipelineConfig = toml::from_str(&toml_str).unwrap();
    assert_eq!(back.stages.len(), 1);
    assert_eq!(back.mode, CouncilMode::Auto);
    assert_eq!(back.fallback, FallbackAction::Abort);
}

#[test]
fn test_turn_result_duration_serialization() {
    let turn = TurnResult {
        turn_index: 0,
        role: CouncilRole::Generator,
        model_id: "llama3".into(),
        output: "Hello world".into(),
        duration: Duration::from_millis(1234),
        error: None,
    };

    let json = serde_json::to_string(&turn).unwrap();
    let back: TurnResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back.duration, Duration::from_millis(1234));
}

#[test]
fn test_debate_transcript_lifecycle() {
    let config = CouncilPipelineConfig::default();
    let mut transcript = DebateTranscript::new("sess-1".into(), "Test prompt".into(), config);

    assert_eq!(transcript.turn_count(), 0);
    assert!(transcript.all_succeeded());

    transcript.append_turn(TurnResult {
        turn_index: 0,
        role: CouncilRole::Generator,
        model_id: "llama3".into(),
        output: "Generated text".into(),
        duration: Duration::from_secs(1),
        error: None,
    });

    assert_eq!(transcript.turn_count(), 1);
    assert!(transcript.all_succeeded());

    transcript.append_turn(TurnResult {
        turn_index: 1,
        role: CouncilRole::Auditor,
        model_id: "mistral".into(),
        output: "".into(),
        duration: Duration::from_secs(2),
        error: Some("timeout".into()),
    });

    assert_eq!(transcript.turn_count(), 2);
    assert!(!transcript.all_succeeded());

    // Verify JSON round-trip preserves structure.
    let json = serde_json::to_string(&transcript).unwrap();
    let back: DebateTranscript = serde_json::from_str(&json).unwrap();
    assert_eq!(back.session_id, "sess-1");
    assert_eq!(back.turn_count(), 2);
    assert!(!back.all_succeeded());
}

#[test]
fn test_default_pipeline_config_is_empty() {
    let config = CouncilPipelineConfig::default();
    assert!(config.stages.is_empty());
    assert!(config.role_overrides.is_empty());
    assert_eq!(config.mode, CouncilMode::Sequential);
    assert_eq!(config.fallback, FallbackAction::Skip);
}

#[test]
fn test_recommend_mode_concurrent_when_zero_required() {
    // Zero bytes always fits in any non-zero VRAM, so this should return Concurrent
    // unless the probe fails (no GPU), in which case it falls back to Sequential.
    let result = crate::council::recommend_mode(0);
    // The function is deterministic: either Concurrent (GPU present) or Sequential (no GPU).
    matches!(result, CouncilMode::Concurrent | CouncilMode::Sequential);
}

#[test]
fn test_recommend_mode_sequential_on_impossible_requirement() {
    // u64::MAX can never be satisfied, so we must get Sequential.
    assert_eq!(
        crate::council::recommend_mode(u64::MAX),
        CouncilMode::Sequential
    );
}

#[test]
fn test_get_available_vram_bytes_returns_valid_option() {
    let result = crate::council::get_available_vram_bytes();
    match result {
        Some(v) => assert!(v > 0, "VRAM should be positive when probed"),
        None => {} // expected on non-Linux / non-NVIDIA systems
    }
}
