//! Request-building and response-text extraction.

use serde_json::json;

use super::provider::FrontierProvider;

/// Build the provider-specific request: `(url, headers, body)`.
///
/// This is a pure function so it can be unit-tested without a network.
pub fn build_request_body(
    provider: FrontierProvider,
    key: &str,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> (String, Vec<(String, String)>, String) {
    match provider {
        FrontierProvider::Anthropic => {
            let body = json!({
                "model": model,
                "max_tokens": 4096,
                "system": system_prompt,
                "messages": [{"role": "user", "content": user_prompt}],
            });
            let mut headers = provider.auth_headers(key);
            headers.push(("content-type".to_string(), "application/json".to_string()));
            (provider.endpoint(model), headers, body.to_string())
        }
        FrontierProvider::OpenAI => {
            let body = json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_prompt},
                ],
                "response_format": {"type": "json_object"},
            });
            let mut headers = provider.auth_headers(key);
            headers.push(("content-type".to_string(), "application/json".to_string()));
            (provider.endpoint(model), headers, body.to_string())
        }
        FrontierProvider::Gemini => {
            let body = json!({
                "contents": [{"role": "user", "parts": [{"text": user_prompt}]}],
                "systemInstruction": {"parts": [{"text": system_prompt}]},
                "generationConfig": {"responseMimeType": "application/json"},
            });
            let mut headers = provider.auth_headers(key);
            headers.push(("content-type".to_string(), "application/json".to_string()));
            (provider.endpoint(model), headers, body.to_string())
        }
    }
}

/// Extract the model's textual answer from a provider-specific response body.
/// Returns `None` if no text can be found.
pub fn extract_response_text(provider: FrontierProvider, response: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(response).ok()?;
    match provider {
        FrontierProvider::Anthropic => {
            // {"content": [{"type":"text","text":"..."}]}
            let parts: Vec<String> = value
                .get("content")?
                .as_array()?
                .iter()
                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                .map(str::to_string)
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        FrontierProvider::OpenAI => value
            .get("choices")?
            .as_array()?
            .first()?
            .get("message")?
            .get("content")
            .and_then(|c| c.as_str())
            .map(str::to_string),
        FrontierProvider::Gemini => {
            let parts: Vec<String> = value
                .get("candidates")?
                .as_array()?
                .first()?
                .get("content")?
                .get("parts")?
                .as_array()?
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .map(str::to_string)
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
    }
}
