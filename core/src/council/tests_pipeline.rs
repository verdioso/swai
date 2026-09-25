use super::pipeline::*;
use super::types::*;
use super::Executor;

struct MockExecutor {
    responses: Vec<String>,
    failures: Vec<usize>,
}

impl MockExecutor {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses,
            failures: Vec::new(),
        }
    }

    fn with_failures(mut self, failures: Vec<usize>) -> Self {
        self.failures = failures;
        self
    }
}

impl Executor for MockExecutor {
    fn execute(&self, stage: &PipelineStage, _input: &str) -> Result<String, String> {
        let idx = if stage.role == CouncilRole::Generator {
            0
        } else if stage.role == CouncilRole::Auditor {
            1
        } else {
            2
        };

        if self.failures.contains(&idx) {
            return Err("mock stage failed".to_string());
        }

        if idx < self.responses.len() {
            Ok(self.responses[idx].clone())
        } else {
            Ok("mock response".to_string())
        }
    }
}

#[test]
fn test_execute_empty_pipeline_aborts() {
    let config = CouncilPipelineConfig::default();
    let engine = CouncilEngine::new(config, MockExecutor::new(vec![]));
    let outcome = engine.execute("test prompt");
    matches!(outcome, DebateOutcome::Aborted { .. });
}

#[test]
fn test_execute_single_generator_success() {
    let config = CouncilPipelineConfig {
        stages: vec![PipelineStage {
            model_id: "llama3".into(),
            role: CouncilRole::Generator,
            prompt_template: "Generate: {input}".into(),
            temperature: 0.7,
            top_p: 0.9,
            system_prompt: None,
        }],
        ..Default::default()
    };

    let engine = CouncilEngine::new(config, MockExecutor::new(vec!["Hello world".into()]));
    match engine.execute("test prompt") {
        DebateOutcome::Success { final_response, .. } => {
            assert_eq!(final_response, "Hello world");
        }
        other => panic!("Expected Success, got {:?}", other),
    }
}

#[test]
fn test_execute_with_auditor_loop_success() {
    let config = CouncilPipelineConfig {
        stages: vec![
            PipelineStage {
                model_id: "llama3".into(),
                role: CouncilRole::Generator,
                prompt_template: "Generate: {input}".into(),
                temperature: 0.7,
                top_p: 0.9,
                system_prompt: None,
            },
            PipelineStage {
                model_id: "mistral".into(),
                role: CouncilRole::Auditor,
                prompt_template: "Audit: {input}".into(),
                temperature: 0.3,
                top_p: 0.8,
                system_prompt: None,
            },
        ],
        ..Default::default()
    };

    let engine = CouncilEngine::new(
        config,
        MockExecutor::new(vec![
            "Generated draft".into(),
            "STATUS: APPROVED".into(),
        ]),
    );

    match engine.execute("test prompt") {
        DebateOutcome::Success { final_response, .. } => {
            assert_eq!(final_response, "Generated draft");
        }
        other => panic!("Expected Success, got {:?}", other),
    }
}

#[test]
fn test_execute_auditor_failure_with_skip_fallback() {
    let config = CouncilPipelineConfig {
        stages: vec![
            PipelineStage {
                model_id: "llama3".into(),
                role: CouncilRole::Generator,
                prompt_template: "Generate: {input}".into(),
                temperature: 0.7,
                top_p: 0.9,
                system_prompt: None,
            },
            PipelineStage {
                model_id: "mistral".into(),
                role: CouncilRole::Auditor,
                prompt_template: "Audit: {input}".into(),
                temperature: 0.3,
                top_p: 0.8,
                system_prompt: None,
            },
        ],
        fallback: FallbackAction::Skip,
        ..Default::default()
    };

    let engine = CouncilEngine::new(
        config,
        MockExecutor::new(vec!["Generated draft".into()]).with_failures(vec![1]),
    );

    match engine.execute("test prompt") {
        DebateOutcome::Partial {
            fallback_response,
            warnings,
            ..
        } => {
            assert_eq!(fallback_response, "Generated draft");
            assert!(!warnings.is_empty(), "Expected warning on skipped auditor");
        }
        other => panic!("Expected Partial, got {:?}", other),
    }
}

#[test]
fn test_execute_generator_failure_with_abort() {
    let config = CouncilPipelineConfig {
        stages: vec![PipelineStage {
            model_id: "llama3".into(),
            role: CouncilRole::Generator,
            prompt_template: "Generate: {input}".into(),
            temperature: 0.7,
            top_p: 0.9,
            system_prompt: None,
        }],
        fallback: FallbackAction::Abort,
        ..Default::default()
    };

    let engine = CouncilEngine::new(config, MockExecutor::new(vec![]).with_failures(vec![0]));

    match engine.execute("test prompt") {
        DebateOutcome::Aborted { reason, .. } => {
            assert!(reason.contains("stage failure"));
        }
        other => panic!("Expected Aborted, got {:?}", other),
    }
}
