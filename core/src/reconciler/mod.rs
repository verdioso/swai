//! Startup reconciliation — checks for models that may have been running
//! before SWAI started and determines the initial state.
//!
//! Also provides an unmanaged-server prober that scans common LLM ports on
//! localhost, filters out those already configured in `Config::models`, and
//! performs a short HTTP GET `/v1/models` to discover model names from any
//! OpenAI-compatible or Ollama server found listening.

use crate::config::Config;
use crate::process_manager::{PortState, ProcessError};
use serde::Deserialize;
use std::collections::HashSet;
use std::net::TcpStream;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Common LLM server ports to probe during unmanaged-server discovery.
const COMMON_LLM_PORTS: &[u16] = &[8000, 8080, 8081, 11434];

/// Information about an unmanaged (not in Config::models) LLM server found
/// on a candidate port during probing.
#[derive(Debug, Clone)]
pub struct UnmanagedModelInfo {
    /// The local TCP port the server is listening on.
    pub port: u16,
    /// The discovered model name extracted from the server's `/v1/models`
    /// response (e.g. `"gpt-3.5-turbo"`, `"llama-3"`, etc.).
    pub model_name: String,
}

/// Build an HTTP client with a 500ms timeout for probing candidate ports.
/// The short timeout prevents hanging on unresponsive listeners during the
/// startup scan.
fn probe_http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .expect("reqwest client build failed")
}

/// Result of the startup reconciliation.
#[derive(Debug)]
pub enum ReconcileResult {
    /// Exactly one model is running — return its id.
    OneRunning(String),
    /// Multiple models are running — surface a warning.
    MultipleRunning(Vec<String>),
    /// No models are running — all start in Stopped state.
    NoneRunning,
}

/// Reconciler — checks for running models at startup.
pub struct Reconciler {
    config: Config,
}

impl Reconciler {
    /// Create a new reconciler from a loaded config.
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Access the current (reconciled) config.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Run the reconciliation.
    pub fn reconcile(&self) -> Result<ReconcileResult, ProcessError> {
        let mut running_ids: Vec<String> = Vec::new();

        for model in &self.config.models {
            match PortState::from_port_check(model.port) {
                PortState::OccupiedByModel => {
                    debug!("found running model '{}' on port {}", model.id, model.port);
                    running_ids.push(model.id.clone());
                }
                PortState::OccupiedByUnknown(pid) => {
                    warn!(
                        "port {} occupied by unknown process (pid: {})",
                        model.port, pid
                    );
                }
                PortState::Free => {}
            }
        }

        let result = match running_ids.len() {
            0 => ReconcileResult::NoneRunning,
            1 => ReconcileResult::OneRunning(running_ids.into_iter().next().unwrap()),
            _ => ReconcileResult::MultipleRunning(running_ids),
        };

        info!("reconciliation result: {:?}", result);
        Ok(result)
    }

    /// Probe common local ports for unmanaged LLM servers that are not present
    /// in `Config::models`.
    ///
    /// For each candidate port in [`COMMON_LLM_PORTS`] that is **not** already
    /// used by a configured model, this function:
    /// 1. Sends an HTTP GET `/v1/models` with a 500ms timeout.
    /// 2. Parses the JSON response body looking for either:
    ///    - OpenAI-compatible format: `data[0].id`
    ///    - Ollama-compatible format: `models[0].name`
    /// 3. Returns a `Vec<UnmanagedModelInfo>` containing the port and discovered
    ///    model name for every responding server found.
    pub fn probe_unmanaged_servers(&self) -> Vec<UnmanagedModelInfo> {
        let configured_ports: HashSet<u16> = self.config.models.iter().map(|m| m.port).collect();

        let client = probe_http_client();
        let mut results = Vec::new();

        for &port in COMMON_LLM_PORTS {
            // Skip ports already configured
            if configured_ports.contains(&port) {
                debug!("skipping configured port {}", port);
                continue;
            }

            // Quick TCP connect check — avoids an HTTP round-trip on closed ports
            if TcpStream::connect(format!("127.0.0.1:{}", port)).is_err() {
                continue;
            }

            let url = format!("http://127.0.0.1:{}/v1/models", port);
            debug!("probing {} for unmanaged server", url);

            if let Ok(resp) = client.get(&url).send() {
                if !resp.status().is_success() {
                    continue;
                }

                if let Ok(body) = resp.text() {
                    if let Some(name) = extract_model_name_from_response(&body) {
                        info!("discovered unmanaged model '{}' on port {}", name, port);
                        results.push(UnmanagedModelInfo {
                            port,
                            model_name: name,
                        });
                    }
                }
            }
        }

        if !results.is_empty() {
            info!("found {} unmanaged LLM server(s)", results.len());
        }

        results
    }

    /// Reconcile ctx_size values from script files on disk with config.toml.
    ///
    /// When SWAI boots, it scans each configured model's `.sh` script for a
    /// `--ctx-size`, `-c`, `--ctx_size`, `--ctx-size=`, or `CTX_SIZE=`
    /// argument. If the script specifies a context size that differs from the
    /// value in `config.toml`, this function updates the config in memory and
    /// writes it back to disk so the running config reflects what the script
    /// actually uses.
    ///
    /// This handles the case where a user edits the launch script externally
    /// (e.g., to bump context window from 65536 to 131072) without going
    /// through SWAI's Edit dialog.
    pub fn reconcile_ctx_sizes(&mut self) -> Vec<String> {
        use crate::import_wizard::detect_ctx_size;

        let config_path = match Config::resolve_path() {
            Some(p) => p,
            None => {
                warn!("no config file found, skipping ctx-size reconciliation");
                return vec![];
            }
        };

        let mut reconciled = Vec::new();

        for model in &mut self.config.models {
            // Read the script file and detect its ctx-size.
            let script_content = match std::fs::read_to_string(&model.script_path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "failed to read script {} for model '{}': {}",
                        model.script_path.display(),
                        model.id,
                        e
                    );
                    continue;
                }
            };

            let script_ctx = match detect_ctx_size(&script_content) {
                Some(size) => size,
                None => continue, // No ctx-size in script, nothing to reconcile.
            };

            // If the script's ctx-size differs from config, update config.
            if script_ctx != model.ctx_size {
                info!(
                    "reconciling ctx_size for model '{}': script has {}, config has {}",
                    model.id, script_ctx, model.ctx_size
                );
                reconciled.push(format!(
                    "model '{}' ctx_size: {} -> {}",
                    model.id, model.ctx_size, script_ctx
                ));
                model.ctx_size = script_ctx;
            }
        }

        if !reconciled.is_empty() {
            // Write updated config to disk.
            if let Ok(content) = toml::to_string_pretty(&self.config) {
                if let Err(e) = std::fs::write(&config_path, &content) {
                    warn!("failed to write reconciled config: {}", e);
                } else {
                    info!(
                        "reconciled ctx_sizes for {} model(s) and wrote config",
                        reconciled.len()
                    );
                }
            }
        }

        reconciled
    }
}

/// OpenAI-compatible `/v1/models` response shape (only the `data` array).
#[derive(Deserialize)]
struct OpenAIModelsResponse {
    data: Option<Vec<OpenAIModelEntry>>,
}

#[derive(Deserialize)]
struct OpenAIModelEntry {
    id: Option<String>,
}

/// Ollama-compatible `/api/tags` response shape.
#[derive(Deserialize)]
struct OllamaTagsResponse {
    models: Option<Vec<OllamaModelEntry>>,
}

#[derive(Deserialize)]
struct OllamaModelEntry {
    name: Option<String>,
}

/// Extract a model name from an HTTP `/v1/models` or `/api/tags` response body.
///
/// Tries OpenAI-compatible format first (`data[0].id`), then falls back to
/// Ollama-compatible format (`models[0].name`). Returns `None` if the payload
/// cannot be parsed or contains no usable model name.
fn extract_model_name_from_response(body: &str) -> Option<String> {
    // Try OpenAI-compatible format: {"data": [{"id": "..."}]}
    if let Ok(parsed) = serde_json::from_str::<OpenAIModelsResponse>(body) {
        if let Some(data) = parsed.data {
            if let Some(first) = data.first() {
                if let Some(id) = &first.id {
                    if !id.is_empty() {
                        return Some(id.clone());
                    }
                }
            }
        }
    }

    // Try Ollama-compatible format: {"models": [{"name": "..."}]}
    if let Ok(parsed) = serde_json::from_str::<OllamaTagsResponse>(body) {
        if let Some(models) = parsed.models {
            if let Some(first) = models.first() {
                if let Some(name) = &first.name {
                    if !name.is_empty() {
                        return Some(name.clone());
                    }
                }
            }
        }
    }

    None
}

/// Extension trait for PortState to check if a port is occupied by a llama-server.
trait PortCheck {
    fn from_port_check(port: u16) -> PortState;
}

impl PortCheck for PortState {
    fn from_port_check(port: u16) -> Self {
        // Try to connect to the port
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            // Port is occupied — probe it for /v1/models
            let url = format!("http://127.0.0.1:{}/v1/models", port);
            if let Ok(resp) = probe_http_client().get(&url).send() {
                if resp.status().is_success() {
                    // Check if it's a valid llama-server response
                    if let Ok(body) = resp.text() {
                        if body.contains("\"id\"") && body.contains("model") {
                            return PortState::OccupiedByModel;
                        }
                    }
                }
            }
            // Occupied but not a llama-server — try to get pid
            if let Ok(pid) = crate::process_manager::ProcessManager::get_port_pid(port) {
                return PortState::OccupiedByUnknown(pid);
            }
            PortState::OccupiedByUnknown(0)
        } else {
            PortState::Free
        }
    }
}

#[cfg(test)]
mod tests;
