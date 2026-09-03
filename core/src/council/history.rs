//! SWAI — Debate transcript persistence layer.
//!
//! Save and load debate transcripts as JSON files from
//! `~/.local/share/swai/debates/<id>.json`.

use std::fs;
use std::path::PathBuf;

use crate::council::types::DebateTranscript;

/// Base directory for debate transcript storage.
pub fn debates_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("Cannot read HOME: {}", e))?;
    let dir = PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("swai")
        .join("debates");
    Ok(dir)
}

/// Ensure the debates directory exists.
pub fn ensure_dir() -> Result<PathBuf, String> {
    let dir = debates_dir()?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create debates dir: {}", e))?;
    Ok(dir)
}

/// Check if a debate transcript is transient (e.g. auxiliary title queries).
pub fn is_transient(transcript: &DebateTranscript) -> bool {
    let p = transcript.input_prompt.to_lowercase();
    if p.contains("write the title")
        || p.contains("predominant language")
        || p.contains("auxiliary title")
        || p.contains("<session>")
    {
        return true;
    }
    false
}

/// Generate a clean human-readable slug from an input prompt.
pub fn prompt_to_slug(prompt: &str) -> String {
    // Strip XML/HTML style tags like <session>
    let mut clean = String::new();
    let mut in_tag = false;
    for ch in prompt.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            clean.push(ch);
        }
    }

    let words: Vec<&str> = clean
        .split_whitespace()
        .filter(|w| !w.starts_with("http") && !w.starts_with('/') && w.len() > 1)
        .take(4)
        .collect();

    if words.is_empty() {
        return "session".to_string();
    }

    let raw = words.join("_").to_lowercase();
    raw.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// Save a debate transcript to disk.
///
/// Returns the path where the file was written.
pub fn save_transcript(transcript: &DebateTranscript) -> Result<PathBuf, String> {
    let dir = ensure_dir()?;
    let path = dir.join(format!("{}.json", transcript.session_id));
    let json = serde_json::to_string_pretty(transcript)
        .map_err(|e| format!("Failed to serialize transcript: {}", e))?;
    fs::write(&path, json).map_err(|e| format!("Failed to write transcript: {}", e))?;
    Ok(path)
}

/// Save intermediate transcript to both JSON debate storage and human-readable Markdown log.
pub fn save_intermediate(transcript: &DebateTranscript) {
    if !is_transient(transcript) {
        let _ = save_transcript(transcript);
        if let Some(mut log_dir) = dirs::data_local_dir() {
            log_dir.push("swai");
            log_dir.push("logs");
            log_dir.push("council_transcripts");
            let _ = std::fs::create_dir_all(&log_dir);
            let log_path = log_dir.join(format!("{}.md", transcript.session_id));
            let mut md = format!(
                "# Council Debate: {}\n\n## Original Prompt\n```\n{}\n```\n\n",
                transcript.session_id, transcript.input_prompt
            );
            for turn in &transcript.turns {
                md.push_str(&format!(
                    "## Turn {} - {:?} ({})\nDuration: {:.2?}\n\n```\n{}\n```\n\n---\n\n",
                    turn.turn_index, turn.role, turn.model_id, turn.duration, turn.output
                ));
            }
            let _ = std::fs::write(&log_path, md);
        }
    }
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
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();

    ids.sort();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::council::types::CouncilPipelineConfig;

    #[test]
    fn test_save_and_load_transcript() {
        let temp_id = format!("test_session_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let transcript = DebateTranscript::new(
            temp_id.clone(),
            "test prompt".to_string(),
            CouncilPipelineConfig::default(),
        );

        let save_res = save_transcript(&transcript);
        assert!(save_res.is_ok());

        let loaded_res = load_transcript(&temp_id);
        assert!(loaded_res.is_ok());
        let loaded = loaded_res.unwrap();
        assert_eq!(loaded.session_id, temp_id);
        assert_eq!(loaded.input_prompt, "test prompt");

        if let Ok(dir) = debates_dir() {
            let _ = std::fs::remove_file(dir.join(format!("{}.json", temp_id)));
        }
    }

    #[test]
    fn test_prompt_to_slug() {
        assert_eq!(prompt_to_slug("Write a robust LRU Cache in Rust"), "write_robust_lru_cache");
        assert_eq!(prompt_to_slug("<session>\nWrite a title\n</session>"), "write_title");
        assert_eq!(prompt_to_slug(""), "session");
    }

    #[test]
    fn test_is_transient() {
        let mut transcript = DebateTranscript::new(
            "test".to_string(),
            "Write the title for session".to_string(),
            CouncilPipelineConfig::default(),
        );
        assert!(is_transient(&transcript));

        transcript.input_prompt = "Write a fast LRU cache".to_string();
        assert!(!is_transient(&transcript));
    }
}
