//! SWAI — Debate transcript persistence layer.
//!
//! Re-exports debate transcript persistence helpers from `swai_core::council::history`.

pub use swai_core::council::history::{
    debates_dir, list_debates, load_transcript, save_transcript,
};

use std::fs;
use gtk4 as gtk;
use gtk::prelude::*;

/// Delete a debate transcript by session ID.
#[allow(dead_code)]
pub fn delete_transcript(id: &str) -> Result<(), String> {
    let dir = debates_dir()?;
    let path = dir.join(format!("{}.json", id));

    if !path.exists() {
        return Err(format!("Debate not found: {}", id));
    }

    fs::remove_file(&path).map_err(|e| format!("Failed to delete file: {}", e))?;
    Ok(())
}

/// Populate the debate listbox with saved debates.
pub fn populate_debate_list(listbox: &gtk4::ListBox) {
    while let Some(child) = listbox.first_child() {
        listbox.remove(&child);
    }

    if let Ok(debates) = list_debates() {
        for id in debates {
            let row = gtk4::ListBoxRow::new();
            let label = gtk4::Label::new(Some(&id));
            label.set_xalign(0.0);
            label.set_margin_start(12);
            label.set_margin_end(12);
            label.set_margin_top(6);
            label.set_margin_bottom(6);

            row.add_css_class("selectable");
            row.set_activatable(true);
            row.set_child(Some(&label));
            listbox.append(&row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use swai_core::council::{
        CouncilPipelineConfig, CouncilRole, DebateTranscript, PipelineStage, TurnResult,
    };

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
        // Should not error even if debates exist or directory is empty.
        let debates = list_debates();
        assert!(debates.is_ok());
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
