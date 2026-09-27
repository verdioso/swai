import re

# 1. Add workspace field to CouncilEngine in pipeline.rs
with open('core/src/council/pipeline.rs', 'r') as f:
    pipeline_rs = f.read()

pipeline_rs = pipeline_rs.replace(
    'pub struct CouncilEngine<E: Executor> {\n    pub config: CouncilPipelineConfig,\n    pub executor: E,\n    events: Option<tokio::sync::broadcast::Sender<CouncilEvent>>,\n}',
    'pub struct CouncilEngine<E: Executor> {\n    pub config: CouncilPipelineConfig,\n    pub executor: E,\n    events: Option<tokio::sync::broadcast::Sender<CouncilEvent>>,\n    pub workspace: Option<std::path::PathBuf>,\n}'
)

pipeline_rs = pipeline_rs.replace(
    'events: None,\n        }',
    'events: None,\n            workspace: None,\n        }'
)

pipeline_rs = pipeline_rs.replace(
    'events: Some(events),\n        }',
    'events: Some(events),\n            workspace: None,\n        }'
)

workspace_method = """
    pub fn with_workspace(mut self, workspace: std::path::PathBuf) -> Self {
        self.workspace = Some(workspace);
        self
    }
"""

pipeline_rs = pipeline_rs.replace(
    'pub fn with_events(',
    workspace_method + '\n    pub fn with_events('
)

pipeline_rs = pipeline_rs.replace(
    'let workspace = crate::council::tools::detect_workspace_root();',
    'let workspace = self.workspace.clone().unwrap_or_else(|| crate::council::tools::detect_workspace_root());'
)

with open('core/src/council/pipeline.rs', 'w') as f:
    f.write(pipeline_rs)

# 2. Add extract_workspace_from_body to prompt.rs
with open('core/src/proxy/prompt.rs', 'r') as f:
    prompt_rs = f.read()

if "pub fn extract_workspace_from_body" not in prompt_rs:
    workspace_fn = """
pub fn extract_workspace_from_body(body: &[u8]) -> Option<String> {
    let json_val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let messages = json_val.get("messages").and_then(|m| m.as_array())?;
    for msg in messages {
        if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
            if let Some(text) = super::prompt::extract_message_text(msg) {
                if let Some(idx) = text.find("Working directory: ") {
                    let start = idx + "Working directory: ".len();
                    let end = text[start..].find('\\n').map(|i| start + i).unwrap_or(text.len());
                    return Some(text[start..end].trim().to_string());
                }
            }
        }
    }
    None
}
"""
    prompt_rs += workspace_fn
    with open('core/src/proxy/prompt.rs', 'w') as f:
        f.write(prompt_rs)

# 3. Update council_route.rs
with open('core/src/proxy/council_route.rs', 'r') as f:
    route_rs = f.read()

route_rs = route_rs.replace(
    'let engine = CouncilEngine::with_events(pipeline_config, executor, tx);',
    'let mut engine = CouncilEngine::with_events(pipeline_config, executor, tx);\n    if let Some(ws) = crate::proxy::prompt::extract_workspace_from_body(request_body) {\n        engine = engine.with_workspace(std::path::PathBuf::from(ws));\n    }'
)

with open('core/src/proxy/council_route.rs', 'w') as f:
    f.write(route_rs)

