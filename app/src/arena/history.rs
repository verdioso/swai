#![allow(dead_code, unused)]
//! SWAI — Debate transcript persistence layer.
//!
//! Save and load debate transcripts as JSON files from
//! `~/.local/share/swai/debates/<id>.json`.

use std::fs;
use std::path::PathBuf;

use swai_core::council::DebateTranscript;

/// Base directory for debate transcript storage.
fn debates_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("Cannot read HOME: {}", e))?;
    let dir = PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("swai")
        .join("debates");
    Ok(dir)
}

/// Ensure the debates directory exists.
fn ensure_dir() -> Result<PathBuf, String> {
    let dir = debates_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create debates dir: {}", e))?;
    Ok(dir)
}

/// Save a debate transcript to disk.
///
/// Returns the path where the file was written.
pub fn save_transcript(transcript: &DebateTranscript) -> Result<PathBuf, String> {
    let dir = ensure_dir()?;
    let path = dir.join(format!("{}.json", transcript.session_id));

    let json = serde_json::to_string_pretty(transcript)
        .map_err(|e| format!("JSON serialization error: {}", e))?;

    fs::write(&path, json).map_err(|e| format!("Failed to write file: {}", e))?;
    Ok(path)
}

/// Load a debate transcript from disk by session ID.
pub fn load_transcript(id: &str) -> Result<DebateTranscript, String> {
    let dir = debates_dir()?;
    let path = dir.join(format!("{}.json", id));

    if !path.exists() {
        return Err(format!("Debate not found: {}", id));
    }

    let json = fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {}", e))?;
    let transcript: DebateTranscript =
        serde_json::from_str(&json).map_err(|e| format!("JSON deserialization error: {}", e))?;

    Ok(transcript)
}

/// List all saved debate session IDs, sorted alphabetically.
pub fn list_debates() -> Result<Vec<String>, String> {
    let dir = debates_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut ids: Vec<String> = fs::read_dir(&dir)
        .map_err(|e| format!("Failed to read debates dir: {}", e))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                path.file_stem().and_then(|s| s.to_str()).map(String::from)
            } else {
                None
            }
        })
        .collect();

    ids.sort();
    Ok(ids)
}

/// Delete a debate transcript by session ID.
pub fn delete_transcript(id: &str) -> Result<(), String> {
    let dir = debates_dir()?;
    let path = dir.join(format!("{}.json", id));

    if !path.exists() {
        return Err(format!("Debate not found: {}", id));
    }

    fs::remove_file(&path).map_err(|e| format!("Failed to delete file: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use swai_core::council::{CouncilPipelineConfig, CouncilRole, PipelineStage, TurnResult};

    fn make_test_transcript(id: &str) -> DebateTranscript {
        let config = CouncilPipelineConfig {
            stages: vec![PipelineStage {
                model_id: "test-model".into(),
                role: CouncilRole::Generator,
                prompt_template: String::new(),
                temperature: 0.7,
                top_p: 0.9,
                system_prompt: None,
            }],
            ..Default::default()
        };

        let mut transcript = DebateTranscript::new(id.into(), "Test prompt".into(), config);
        transcript.append_turn(TurnResult {
            turn_index: 0,
            role: CouncilRole::Generator,
            model_id: "test-model".into(),
            output: "Generated text".into(),
            duration: Duration::from_secs(1),
            error: None,
        });
        transcript
    }

    #[test]
    fn test_save_and_load_transcript() {
        let transcript = make_test_transcript("test-save-load");
        let path = save_transcript(&transcript).unwrap();

        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with("test-save-load.json"));

        let loaded = load_transcript("test-save-load").unwrap();
        assert_eq!(loaded.session_id, "test-save-load");
        assert_eq!(loaded.input_prompt, "Test prompt");
        assert_eq!(loaded.turn_count(), 1);
        assert_eq!(loaded.turns[0].output, "Generated text");

        // Cleanup.
        let _ = delete_transcript("test-save-load");
    }

    #[test]
    fn test_load_nonexistent_transcript() {
        let result = load_transcript("nonexistent-id-12345");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_list_debates_empty() {
        // Should not error even if directory doesn't exist.
        let debates = list_debates().unwrap();
        assert!(debates.is_empty());
    }

    #[test]
    fn test_delete_transcript() {
        let transcript = make_test_transcript("test-delete");
        save_transcript(&transcript).unwrap();

        delete_transcript("test-delete").unwrap();

        let result = load_transcript("test-delete");
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_nonexistent_transcript() {
        let result = delete_transcript("nonexistent-delete-12345");
        assert!(result.is_err());
    }
}
