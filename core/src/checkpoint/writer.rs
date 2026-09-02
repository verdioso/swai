use std::io::Write;

use super::entry::{CheckpointEntry, SessionCheckpoint};

pub struct CheckpointWriter {
    /// The absolute path to the checkpoint file.
    file_path: std::path::PathBuf,
    /// Mutex-guarded state to serialize writes across threads.
    state: std::sync::Arc<std::sync::Mutex<CheckpointWriterState>>,
}

struct CheckpointWriterState {
    /// Whether the header has been written yet.
    header_written: bool,
}

impl std::fmt::Debug for CheckpointWriterState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointWriterState")
            .field("header_written", &self.header_written)
            .finish()
    }
}

impl CheckpointWriter {
    /// Create a new writer for the given session ID.
    ///
    /// The checkpoint file path is `~/.local/share/swai/checkpoints/<session_id>.md`.
    /// The parent directory is created automatically if it does not exist.
    pub fn new(session_id: &str) -> std::io::Result<Self> {
        Self::new_in_dir(Self::default_base_dir(), session_id)
    }

    /// Create a new writer for the given session ID in a specific directory.
    pub fn new_in_dir(base: std::path::PathBuf, session_id: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(&base)?;
        let file_path = base.join(format!("{}.md", session_id));
        Ok(Self {
            file_path,
            state: std::sync::Arc::new(std::sync::Mutex::new(CheckpointWriterState {
                header_written: false,
            })),
        })
    }

    /// Return the default base directory for checkpoint files.
    ///
    /// `~/.local/share/swai/checkpoints/` (or `$XDG_DATA_HOME/swai/checkpoints/`).
    pub fn default_base_dir() -> std::path::PathBuf {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            std::path::PathBuf::from(&xdg)
                .join("swai")
                .join("checkpoints")
        } else if let Ok(home) = std::env::var("HOME") {
            std::path::PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("swai")
                .join("checkpoints")
        } else {
            std::path::PathBuf::from("checkpoints")
        }
    }

    /// Return the path to this writer's checkpoint file.
    pub fn file_path(&self) -> &std::path::Path {
        &self.file_path
    }

    /// Count how many checkpoint sections are already present in the on-disk file.
    pub fn existing_checkpoint_count(&self) -> usize {
        if let Ok(content) = std::fs::read_to_string(&self.file_path) {
            content
                .lines()
                .filter(|l| l.trim_start().starts_with("## Checkpoint #"))
                .count()
        } else {
            0
        }
    }

    /// Determine the next checkpoint index (1-based) based on on-disk history.
    pub fn next_checkpoint_index(&self) -> usize {
        self.existing_checkpoint_count() + 1
    }

    /// Write (or append) a new checkpoint entry to disk.
    pub fn write_entry(&self, entry: &CheckpointEntry) -> std::io::Result<()> {
        self.write_entry_with_objective(entry, None)
    }

    /// Write (or append) a new checkpoint entry to disk with optional initial objective.
    pub fn write_entry_with_objective(
        &self,
        entry: &CheckpointEntry,
        objective: Option<&str>,
    ) -> std::io::Result<()> {
        let _lock = self.state.lock().map_err(|e| {
            std::io::Error::other(format!("checkpoint writer lock poisoned: {}", e))
        })?;

        // Derive session_id from the file name (strip .md).
        let session_id = self
            .file_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        if !self.file_path.exists() {
            // First write: create the file with the header.
            let mut header = format!(
                "# SWAI Session Checkpoint Log\n\
                 **Session ID:** `{}`\n",
                session_id,
            );
            if let Some(obj) = objective {
                header.push_str(&format!("**Initial Objective:** `{}`\n", obj));
            }
            header.push_str(&format!(
                "**Last Updated:** `{}Z`\n",
                chrono::Utc::now().to_rfc3339()
            ));
            std::fs::write(&self.file_path, &header)?;
        }

        // Compute actual 1-based checkpoint index
        let actual_index = if entry.index > 0 {
            entry.index
        } else {
            self.next_checkpoint_index()
        };

        // Append a new checkpoint section.
        let section = format!(
            "\n## Checkpoint #{} ({} messages compacted)\n",
            actual_index,
            entry.summary_lines.len()
        );
        let mut lines = String::new();
        for (i, line) in entry.summary_lines.iter().enumerate() {
            lines.push_str(&format!("{}. {}\n", i + 1, line));
        }

        // Append to existing file without destroying prior sections
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)?;
        file.write_all(format!("{}{}", section, lines).as_bytes())?;

        Ok(())
    }

    /// Write a complete checkpoint log from a `SessionCheckpoint` (all entries).
    ///
    /// This overwrites the file entirely with all entries formatted as sections.
    pub fn write_snapshot(&self, session: &SessionCheckpoint) -> std::io::Result<()> {
        let content = session.to_disk_format();
        std::fs::write(&self.file_path, content)?;
        Ok(())
    }

    /// Read the checkpoint file contents, returning an empty string if it
    /// does not exist.
    /// Load existing SessionCheckpoint from disk if present.
    pub fn load_session(&self) -> Option<SessionCheckpoint> {
        let content = self.read_contents();
        if content.is_empty() {
            return None;
        }

        let session_id = self
            .file_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        let mut session = SessionCheckpoint::new(session_id);

        let mut current_entry: Option<CheckpointEntry> = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("**Initial Objective:** `") {
                if let Some(end) = rest.rfind('`') {
                    session.initial_objective = Some(rest[..end].to_string());
                }
            } else if trimmed.starts_with("## Checkpoint #") {
                if let Some(entry) = current_entry.take() {
                    session.entries.push(entry);
                }
                let idx = session.entries.len() + 1;
                current_entry = Some(CheckpointEntry {
                    index: idx,
                    timestamp: String::new(),
                    summary_lines: Vec::new(),
                });
            } else if let Some(ref mut entry) = current_entry {
                // Parse lines like "1. Read src/lib.rs"
                if let Some(dot_pos) = trimmed.find(". ") {
                    let prefix = &trimmed[..dot_pos];
                    if prefix.chars().all(|c| c.is_ascii_digit()) {
                        let text = trimmed[dot_pos + 2..].to_string();
                        entry.summary_lines.push(text);
                    }
                }
            }
        }

        if let Some(entry) = current_entry {
            session.entries.push(entry);
        }

        Some(session)
    }

    pub fn read_contents(&self) -> String {
        std::fs::read_to_string(&self.file_path).unwrap_or_default()
    }
}
