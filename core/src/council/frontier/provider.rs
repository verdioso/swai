//! Frontier LLM provider metadata.

use serde::{Deserialize, Serialize};

/// A frontier LLM provider the Auditor can review with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontierProvider {
    /// Anthropic Claude (`anthropic.com`).
    Anthropic,
    /// OpenAI GPT (`api.openai.com`).
    OpenAI,
    /// Google Gemini (`generativelanguage.googleapis.com`).
    Gemini,
}

impl FrontierProvider {
    /// Human-readable label for the UI.
    pub fn display_name(self) -> &'static str {
        match self {
            FrontierProvider::Anthropic => "Anthropic (Claude)",
            FrontierProvider::OpenAI => "OpenAI (GPT)",
            FrontierProvider::Gemini => "Google (Gemini)",
        }
    }

    /// The base endpoint URL. `model` is interpolated for Gemini, which keys
    /// the method off the path.
    pub fn endpoint(self, model: &str) -> String {
        match self {
            FrontierProvider::Anthropic => "https://api.anthropic.com/v1/messages".into(),
            FrontierProvider::OpenAI => "https://api.openai.com/v1/chat/completions".into(),
            FrontierProvider::Gemini => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
            ),
        }
    }

    /// The default model id to use when the caller does not specify one.
    pub fn default_model(self) -> &'static str {
        match self {
            FrontierProvider::Anthropic => "claude-sonnet-5",
            FrontierProvider::OpenAI => "gpt-4o",
            FrontierProvider::Gemini => "gemini-2.5-pro",
        }
    }

    /// Authentication headers for this provider. The API key is substituted in;
    /// it is never persisted or logged by this module.
    pub fn auth_headers(self, key: &str) -> Vec<(String, String)> {
        match self {
            FrontierProvider::Anthropic => vec![
                ("x-api-key".to_string(), key.to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ],
            FrontierProvider::OpenAI => vec![("Authorization".to_string(), format!("Bearer {key}"))],
            FrontierProvider::Gemini => vec![("x-goog-api-key".to_string(), key.to_string())],
        }
    }
}
