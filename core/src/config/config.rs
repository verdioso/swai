use serde::Deserialize;
use std::path::{Path, PathBuf};

use super::error::ConfigError;
use super::model::ModelConfig;
use super::preferences::{GlobalSettings, PreferencesConfig};
use crate::council::CouncilPipelineConfig;

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct Config {
    pub schema_version: u8,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    #[serde(default)]
    pub global: GlobalSettings,
    #[serde(default)]
    pub preferences: PreferencesConfig,
    #[serde(default)]
    pub council: CouncilPipelineConfig,
}

impl Config {
    /// Resolve the config file path from XDG directories.
    pub fn resolve_path() -> Option<PathBuf> {
        // Try $XDG_CONFIG_HOME/swai/config.toml first
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let p = PathBuf::from(&xdg).join("swai").join("config.toml");
            if p.exists() {
                return Some(p);
            }
        }

        // Fallback to ~/.config/swai/config.toml
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(&home)
                .join(".config")
                .join("swai")
                .join("config.toml");
            if p.exists() {
                return Some(p);
            }
        }

        None
    }

    /// Load and validate configuration from the resolved path.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::resolve_path().ok_or(ConfigError::NotFound)?;
        let content = std::fs::read_to_string(&path)?;

        let raw: Self = toml::from_str(&content).map_err(ConfigError::TomlParse)?;
        Self::validate(&raw, &path)
    }

    /// Save configuration to the resolved config.toml file on disk.
    pub fn save_to_disk(&self) -> Result<(), String> {
        let path =
            Self::resolve_path().ok_or_else(|| "No config.toml path resolved".to_string())?;
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("Serialization error: {}", e))?;
        std::fs::write(&path, content).map_err(|e| format!("File write error: {}", e))?;
        Ok(())
    }

    /// Validate a loaded config.
    pub fn validate(config: &Self, _config_path: &Path) -> Result<Self, ConfigError> {
        let mut seen_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();
        for model in &config.models {
            if !seen_ports.insert(model.port) {
                return Err(ConfigError::Validation(format!(
                    "duplicate port {} for models '{}' and '{}'",
                    model.port,
                    config
                        .models
                        .iter()
                        .find(|m| m.port == model.port && m.id != model.id)
                        .map(|m| &m.id)
                        .unwrap_or(&model.id),
                    model.id
                )));
            }
        }

        for model in &config.models {
            if !model.script_path.exists() {
                tracing::warn!(
                    "script not found: {} (configured for model '{}')",
                    model.script_path.display(),
                    model.id,
                );
            }
        }

        Ok(config.clone())
    }

    /// Get the default log directory.
    pub fn default_log_dir() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".local/share/swai/logs/")
        } else {
            PathBuf::from("logs/")
        }
    }

    /// Get the default proxy port.
    pub fn default_proxy_port() -> u16 {
        9080
    }

    /// Get the default auto-restart setting.
    pub fn default_auto_restart_on_context_full() -> bool {
        true
    }

    /// Get the effective log directory.
    pub fn log_dir(&self) -> PathBuf {
        self.global
            .log_dir
            .clone()
            .unwrap_or_else(Self::default_log_dir)
    }

    /// Get the effective proxy port.
    pub fn proxy_port(&self) -> u16 {
        self.global
            .proxy_port
            .unwrap_or_else(Self::default_proxy_port)
    }

    /// Get the effective auto-restart setting.
    pub fn auto_restart_on_context_full(&self) -> bool {
        self.global
            .auto_restart_on_context_full
            .unwrap_or_else(Self::default_auto_restart_on_context_full)
    }

    /// Get the effective auto-follow-logs preference.
    pub fn auto_follow_logs(&self) -> bool {
        self.preferences.auto_follow_logs
    }

    /// Get the effective enable-notifications preference.
    pub fn enable_notifications(&self) -> bool {
        self.preferences.enable_notifications
    }

    /// Get the effective notify-on-switch preference.
    pub fn notify_on_switch(&self) -> bool {
        self.preferences.notify_on_switch
    }

    /// Get the effective autostart-on-login preference.
    pub fn autostart_on_login(&self) -> bool {
        self.preferences.autostart_on_login
    }

    /// Get the effective max-concurrent-models preference.
    pub fn max_concurrent_models(&self) -> usize {
        self.preferences.max_concurrent_models
    }

    /// Get the configured checkpoint summarizer model id (if any).
    ///
    /// When `None`, summarization is routed to the active/primary model.
    /// When set, that specific model handles summarization requests so the
    /// primary model's context is not consumed by compaction overhead.
    pub fn checkpoint_summarizer_model(&self) -> Option<&str> {
        self.preferences.checkpoint_summarizer_model.as_deref()
    }

    /// Get the effective enable-checkpointing preference.
    ///
    /// When `true` (default), the proxy intercepts file-write tool calls to
    /// generate diffs and injects loop-breaker directives. When `false`, the
    /// proxy acts as a transparent router with no interception overhead.
    pub fn enable_checkpointing(&self) -> bool {
        self.preferences.enable_checkpointing
    }

    /// Get the effective enable-council preference.
    ///
    /// When `true` (default), the proxy intercepts council-model requests and
    /// runs them through the synthetic debate pipeline. When `false`, council
    /// requests are routed directly to the target model with zero debate
    /// overhead.
    pub fn enable_council(&self) -> bool {
        self.preferences.enable_council
    }

    /// Get the effective compaction trigger threshold percentage.
    ///
    /// Range: 50% (aggressive) to 85% (retains more history). Default: 70%.
    pub fn compaction_threshold_pct(&self) -> u8 {
        self.preferences.compaction_threshold_pct
    }

    /// Get all configured models as a list of (id, name) pairs.
    ///
    /// Used by the Preferences UI to populate dropdown selectors that let
    /// users choose a model for a specific role (e.g., checkpoint summarizer).
    pub fn configured_models(&self) -> Vec<(&str, &str)> {
        self.models
            .iter()
            .map(|m| (m.id.as_str(), m.name.as_str()))
            .collect()
    }
}

/// Returns an example config.toml for first-run reference.
pub fn example_config() -> &'static str {
    include_str!("../../../config.toml.example")
}
