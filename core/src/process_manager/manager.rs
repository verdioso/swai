use crate::health_monitor::HealthMonitor;
use std::net::TcpStream;
use std::time::Duration;
use tracing::{error, info, warn};

use super::error::ProcessError;
use super::guard::{LinuxProcessGuard, ProcessGuard};
use super::types::{ModelState, PortState, RunningModel};
use crate::config::Config;

pub struct ProcessManager {
    config: Config,
    running_models: Vec<RunningModel>,
    /// Index of the primary (first-started) model in `running_models`.
    /// `None` when no models are running.
    primary_index: Option<usize>,
}

impl ProcessManager {
    /// Create a new process manager from a loaded config.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            running_models: Vec::new(),
            primary_index: None,
        }
    }

    /// Access the underlying config for reconciliation at startup.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Maximum number of concurrent models allowed (from preferences).
    pub fn max_concurrent_models(&self) -> usize {
        self.config.max_concurrent_models()
    }

    /// Update the in-memory config (e.g. after saving preferences).
    pub fn update_config(&mut self, new_config: Config) {
        self.config = new_config;
    }

    /// Number of currently running models.
    pub fn running_count(&self) -> usize {
        self.running_models.len()
    }

    /// Add a newly imported model to the in-memory config.
    pub fn add_model(&mut self, model: crate::config::ModelConfig) {
        if !self.config.models.iter().any(|m| m.id == model.id) {
            self.config.models.push(model);
        }
    }

    /// Remove a model by id from the in-memory config.
    ///
    /// If the model is currently running, stops it first (graceful shutdown).
    /// Returns `Err` if the model is not found in config.
    pub fn remove_model(&mut self, id: &str) -> Result<(), String> {
        if self.running_models.iter().any(|m| m.id == id) {
            info!("stopping model '{}' before removal", id);
            if let Err(e) = self.stop_model(id, false) {
                warn!("failed to stop model '{}' before removal: {}", id, e);
            }
        }

        // Remove from config.models.
        let initial_len = self.config.models.len();
        self.config.models.retain(|m| m.id != id);

        if self.config.models.len() == initial_len {
            return Err(format!("model '{}' not found in config", id));
        }

        if let Err(e) = self.config.save_to_disk() {
            warn!(
                "failed to save config to disk after removing model '{}': {}",
                id, e
            );
        }

        info!("removed model '{}' from config", id);
        Ok(())
    }

    /// Start a model by id. Returns `Err` if the concurrent model limit is reached.
    pub fn start_model(&mut self, id: &str) -> Result<(), ProcessError> {
        // Check concurrent model limit
        let max = self.max_concurrent_models();
        if self.running_models.len() >= max {
            return Err(ProcessError::AnotherModelRunning);
        }

        // Find the model config
        let model_config = self
            .config
            .models
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| ProcessError::NotRunning(id.to_string()))?;

        // Check port — zombie port handling
        match Self::check_port(model_config.port) {
            PortState::Free => {}
            PortState::OccupiedByModel => {
                return Err(ProcessError::HealthCheckFailed(format!(
                    "model '{}' is already running on port {}",
                    id, model_config.port
                )));
            }
            PortState::OccupiedByUnknown(pid) => {
                return Err(ProcessError::PortOccupiedByUnknownProcess {
                    pid,
                    port: model_config.port,
                });
            }
        }

        // Spawn the process (synchronously before entering async context)
        let log_dir = self.config.log_dir();
        let guard_result =
            LinuxProcessGuard::setup(&model_config.script_path, model_config.port, &log_dir);

        match guard_result {
            Ok(guard) => {
                info!("started model '{}' on port {}", id, model_config.port);
                let idx = self.running_models.len();
                let is_primary = self.primary_index.is_none();
                self.running_models.push(RunningModel {
                    id: id.to_string(),
                    guard: Box::new(guard),
                    state: ModelState::Starting,
                });
                if is_primary {
                    self.primary_index = Some(idx);
                }
                Ok(())
            }
            Err(e) => {
                error!("failed to start model '{}': {}", id, e);
                Err(e)
            }
        }
    }

    /// Stop a model by id.
    /// If `fast_shutdown` is true, escalate to SIGKILL after 500ms instead of
    /// waiting up to `shutdown_timeout_sec`, releasing GPU memory instantly.
    pub fn stop_model(&mut self, id: &str, fast_shutdown: bool) -> Result<(), ProcessError> {
        let idx = self
            .running_models
            .iter()
            .position(|m| m.id == id)
            .ok_or_else(|| ProcessError::NotRunning(id.to_string()))?;

        // Get the port from the config for the running model
        let port = self
            .config
            .models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.port)
            .ok_or_else(|| ProcessError::NotRunning(id.to_string()))?;

        // Terminate the process group
        self.running_models
            .remove(idx)
            .guard
            .terminate(fast_shutdown)?;

        // Confirm port is free via TCP bind retry (up to 10s)
        Self::wait_port_free(port, Duration::from_secs(10))?;

        // Adjust primary_index if needed
        if self.primary_index == Some(idx) {
            self.primary_index = None;
        } else if let Some(ref mut pi) = self.primary_index {
            if *pi > idx {
                *pi -= 1;
            }
        }

        info!("stopped model '{}'", id);
        Ok(())
    }

    /// Stop all running models.
    pub fn stop_all(&mut self, fast_shutdown: bool) -> Result<(), ProcessError> {
        // Stop in reverse order so primary_index adjustments stay valid.
        while let Some(running) = self.running_models.pop() {
            let port = self
                .config
                .models
                .iter()
                .find(|m| m.id == running.id)
                .map(|m| m.port)
                .ok_or_else(|| ProcessError::NotRunning(running.id.clone()))?;
            running.guard.terminate(fast_shutdown)?;
            Self::wait_port_free(port, Duration::from_secs(10))?;
        }
        self.primary_index = None;
        Ok(())
    }

    /// Switch from one model to another (atomic sequence).
    ///
    /// Stops the current model, waits 500ms for CUDA context release,
    /// then starts the new model. If any step fails, affected state is cleaned up.
    pub fn switch_model(&mut self, from_id: &str, to_id: &str) -> Result<(), ProcessError> {
        // Step 1: stop the current model with fast shutdown
        self.stop_model(from_id, true)?;

        // Step 2: short delay for CUDA context release
        std::thread::sleep(Duration::from_millis(500));

        // Step 3: start the new model
        match self.start_model(to_id) {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Get the currently running models.
    pub fn get_running_models(&self) -> &[RunningModel] {
        &self.running_models
    }

    /// Get the primary (first-started) running model, if any.
    pub fn get_primary_model(&self) -> Option<&RunningModel> {
        self.primary_index.and_then(|i| self.running_models.get(i))
    }

    /// Get the primary running model id, if any.
    pub fn get_primary_model_id(&self) -> Option<&str> {
        self.primary_index
            .and_then(|i| self.running_models.get(i).map(|m| m.id.as_str()))
    }

    /// Find a running model by id.
    pub fn find_running_model(&self, id: &str) -> Option<&RunningModel> {
        self.running_models.iter().find(|m| m.id == id)
    }

    /// Get all running model IDs.
    pub fn running_model_ids(&self) -> Vec<String> {
        self.running_models.iter().map(|m| m.id.clone()).collect()
    }

    /// Build a map of all running model IDs to their configured ports.
    ///
    /// Used by the proxy to register all concurrently running models so that
    /// dynamic routing can dispatch requests to the correct model based on the
    /// `model` field in incoming requests.
    pub fn running_model_ports(&self) -> Vec<(String, u16)> {
        self.running_models
            .iter()
            .filter_map(|m| {
                self.config
                    .models
                    .iter()
                    .find(|c| c.id == m.id)
                    .map(|c| (m.id.clone(), c.port))
            })
            .collect()
    }

    /// Get the port for a running model by id.
    pub fn get_port_for_model(&self, id: &str) -> Option<u16> {
        self.config
            .models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.port)
    }

    /// Set a running model (used during reconciliation at startup).
    pub fn set_running_model(&mut self, model: RunningModel) {
        let idx = self.running_models.len();
        if self.primary_index.is_none() {
            self.primary_index = Some(idx);
        }
        self.running_models.push(model);
    }

    /// Check if a port is free or occupied by a llama-server process.
    pub fn check_port(port: u16) -> PortState {
        // Try to bind the port
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            // Port is occupied — probe it for /v1/models with a short timeout
            // to prevent indefinite hangs on unresponsive listeners.
            let url = format!("http://127.0.0.1:{}/v1/models", port);
            if let Ok(client) = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
            {
                if let Ok(resp) = client.get(&url).send() {
                    if resp.status().is_success() {
                        // Check if it's a llama-server response (has /v1/models endpoint)
                        if let Ok(body) = resp.text() {
                            if body.contains("\"id\"") && body.contains("model") {
                                return PortState::OccupiedByModel;
                            }
                        }
                    }
                }
            }
            // Occupied but not a llama-server — try to get pid
            if let Ok(pid) = Self::get_port_pid(port) {
                return PortState::OccupiedByUnknown(pid);
            }
            PortState::OccupiedByUnknown(0)
        } else {
            PortState::Free
        }
    }

    /// Wait for a port to become free.
    fn wait_port_free(port: u16, timeout: Duration) -> Result<(), ProcessError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if TcpStream::connect(format!("127.0.0.1:{}", port)).is_err() {
                return Ok(()); // port is free
            }
            if std::time::Instant::now() >= deadline {
                return Err(ProcessError::PortStillOccupied(port));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Get the PID of a process bound to a port (best effort).
    ///
    /// Parses `/proc/net/tcp` to find the inode for the given port, then
    /// searches `/proc/*/fd` for that inode to identify the owning PID.
    pub fn get_port_pid(port: u16) -> Result<u32, ProcessError> {
        // Use /proc/net/tcp to find the PID — this is Linux-specific
        #[cfg(target_os = "linux")]
        {
            let tcp_path = "/proc/net/tcp";
            if let Ok(content) = std::fs::read_to_string(tcp_path) {
                for line in content.lines().skip(1) {
                    // Format: sl local_address remote_address ... state ... inode ...
                    // Fields:  0 1              2          ... 7      ... 9
                    // parts[9] is the inode — need >= 10 fields total.
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 10 {
                        let local_addr = parts[1];
                        let local_port = u16::from_str_radix(
                            local_addr.split(':').nth(1).unwrap_or_default(),
                            16,
                        )
                        .ok();
                        if local_port == Some(port) {
                            // Parse inode to find the PID
                            let inode = parts[9];
                            if let Ok(inode_num) = inode.parse::<u32>() {
                                // Search /proc/*/fd for this inode
                                if let Ok(entries) = std::fs::read_dir("/proc") {
                                    for entry in entries.flatten() {
                                        let fd_path = entry.path().join("fd");
                                        if let Ok(fds) = std::fs::read_dir(&fd_path) {
                                            for fd_entry in fds.flatten() {
                                                if let Ok(link) =
                                                    std::fs::read_link(fd_entry.path())
                                                {
                                                    if link
                                                        .to_string_lossy()
                                                        .contains(&inode_num.to_string())
                                                    {
                                                        // entry.file_name() returns the bare numeric directory name
                                                        // (e.g. "1234"), not a path with a prefix.
                                                        if let Ok(pid) = entry
                                                            .file_name()
                                                            .to_string_lossy()
                                                            .parse::<u32>()
                                                        {
                                                            return Ok(pid);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Err(ProcessError::Io(std::io::Error::other(
            "couldn't determine port PID",
        )))
    }

    /// Start a model and report intermediate health states through a channel.
    ///
    /// This is the full startup flow for the UI:
    /// 1. Call `start_model()` (process spawns, state = Starting)
    /// 2. Spawn a background thread that polls `/v1/models` via `HealthMonitor`
    /// 3. Send `StateUpdate(Starting)` → `StateUpdate(Loading)` → ... → `StateUpdate(Ready|Error)`
    /// 4. Send `SwitchCompleted` with the final result
    ///
    /// The caller should spawn this on a background thread and not await it.
    pub fn start_model_and_report(
        &mut self,
        id: &str,
        tx: std::sync::mpsc::Sender<ModelState>,
    ) -> Result<(), ProcessError> {
        // Start the model (spawns process, sets state = Starting)
        self.start_model(id)?;

        // Extract the port from config for health monitoring
        if let Some(port) = self
            .config
            .models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.port)
        {
            let monitor = HealthMonitor::new(port, 30);
            std::thread::spawn(move || {
                monitor.wait_until_ready_with_updates(tx);
            });
        }

        Ok(())
    }

    /// Resolve a model identifier (id or name) to the port of its running instance.
    pub fn resolve_running_port(&self, identifier: &str) -> Option<u16> {
        for model in &self.running_models {
            if model.id == identifier {
                return self.get_port_for_model(&model.id);
            }
            if let Some(cfg) = self.config.models.iter().find(|c| c.id == model.id) {
                if cfg.name == identifier {
                    return Some(cfg.port);
                }
            }
        }
        None
    }
}
