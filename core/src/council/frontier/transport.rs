//! HTTP transport abstraction (injectable for tests).

use super::error::AuditorError;

/// Abstraction over the HTTP POST used to reach a frontier endpoint.
///
/// Keeping this behind a trait lets the client be unit-tested with a mock
/// transport (no network) while the production path uses [`ReqwestTransport`].
pub trait HttpTransport: Send + Sync {
    /// Perform a POST and return the raw response body on success, or an error
    /// string on failure.
    fn post(&self, url: &str, headers: &[(String, String)], body: &str) -> Result<String, String>;
}

/// Production [`HttpTransport`] backed by `reqwest` (blocking).
pub struct ReqwestTransport {
    client: reqwest::blocking::Client,
}

impl ReqwestTransport {
    /// Build a transport with a 60-second request timeout.
    pub fn new() -> Result<Self, AuditorError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| AuditorError::Transport(e.to_string()))?;
        Ok(Self { client })
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new().expect("failed to build default ReqwestTransport")
    }
}

impl HttpTransport for ReqwestTransport {
    fn post(&self, url: &str, headers: &[(String, String)], body: &str) -> Result<String, String> {
        let mut req = self.client.post(url).body(body.to_string());
        for (name, value) in headers {
            req = req.header(name.as_str(), value.as_str());
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            let snippet = &text[..text.len().min(300)];
            return Err(format!("HTTP {status}: {snippet}"));
        }
        Ok(text)
    }
}
