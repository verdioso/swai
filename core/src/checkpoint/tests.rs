#[cfg(test)]
mod tests {
    use crate::checkpoint::*;

    #[test]
    fn test_session_checkpoint_new_is_empty() {
        let sc = SessionCheckpoint::new("test-session".to_string());
        assert!(sc.is_empty());
        assert_eq!(sc.len(), 0);
        assert_eq!(sc.format_for_injection(), None);
    }

    #[test]
    fn test_session_checkpoint_add_entry() {
        let mut sc = SessionCheckpoint::new("test-session".to_string());
        sc.add_entry(vec!["Read src/lib.rs".to_string()]);
        assert_eq!(sc.len(), 1);
        assert!(!sc.is_empty());

        // Check the entry index and timestamp format.
        let entry = &sc.entries[0];
        assert_eq!(entry.index, 1);
        assert!(!entry.timestamp.is_empty());
        assert_eq!(entry.summary_lines, vec!["Read src/lib.rs"]);
    }

    #[test]
    fn test_session_checkpoint_format_for_injection_single_entry() {
        let mut sc = SessionCheckpoint::new("test".to_string());
        sc.add_entry(vec![
            "Read src/lib.rs, core/src/config.rs".to_string(),
            "Added pub mod update_checker".to_string(),
        ]);

        let formatted = sc.format_for_injection().unwrap();
        assert!(formatted.starts_with("[Session checkpoint"));
        assert!(formatted.contains("Note: this is a condensed action log, not literal file content. If you need exact field names, types, function signatures, or other precise code details from a file listed below, re-read that file — do not reconstruct it from memory."));
        assert!(formatted.contains("1. Read src/lib.rs, core/src/config.rs"));
        assert!(formatted.contains("2. Added pub mod update_checker"));
        assert!(formatted.contains("[End checkpoint"));
    }

    #[test]
    fn test_session_checkpoint_format_for_injection_multiple_entries() {
        let mut sc = SessionCheckpoint::new("test".to_string());
        sc.add_entry(vec!["Read src/lib.rs".to_string()]);
        sc.add_entry(vec![
            "Edited main.rs".to_string(),
            "Ran command: cargo build".to_string(),
        ]);

        let formatted = sc.format_for_injection().unwrap();
        // First entry lines should appear before second entry lines.
        let pos_first = formatted.find("Read src/lib.rs").unwrap();
        let pos_second_start = formatted.find("Edited main.rs").unwrap();
        assert!(pos_first < pos_second_start);

        // Should contain numbered lines from both entries.
        assert!(formatted.contains("1. Read src/lib.rs"));
        assert!(formatted.contains("2. Edited main.rs"));
        assert!(formatted.contains("3. Ran command: cargo build"));
    }

    #[test]
    fn test_session_checkpoint_format_for_injection_empty_returns_none() {
        let sc = SessionCheckpoint::new("test".to_string());
        assert_eq!(sc.format_for_injection(), None);
    }

    #[test]
    fn test_session_checkpoint_sequential_compactions_append() {
        let mut sc = SessionCheckpoint::new("session-1".to_string());

        // First compaction.
        sc.add_entry(vec!["Read src/lib.rs".to_string()]);
        assert_eq!(sc.entries[0].index, 1);

        // Second compaction — should not overwrite.
        sc.add_entry(vec!["Edited main.rs".to_string()]);
        assert_eq!(sc.entries.len(), 2);
        assert_eq!(sc.entries[1].index, 2);

        // Third compaction.
        sc.add_entry(vec!["Ran command: cargo test".to_string()]);
        assert_eq!(sc.entries.len(), 3);
        assert_eq!(sc.entries[2].index, 3);

        // All entries preserved.
        let formatted = sc.format_for_injection().unwrap();
        assert!(formatted.contains("1. Read src/lib.rs"));
        assert!(formatted.contains("2. Edited main.rs"));
        assert!(formatted.contains("3. Ran command: cargo test"));
    }

    #[test]
    fn test_checkpoint_registry_get_or_create() {
        let registry = CheckpointRegistry::new();

        // First access creates a new session.
        let sc1 = registry.get_or_create("session-a");
        assert_eq!(sc1.session_id, "session-a");
        assert!(sc1.is_empty());

        // Second access returns the same session (same ID).
        let sc2 = registry.get_or_create("session-a");
        assert_eq!(sc2.session_id, "session-a");
    }

    #[test]
    fn test_checkpoint_registry_multiple_sessions() {
        let registry = CheckpointRegistry::new();

        // Use get_or_create to get a mutable reference through the lock.
        {
            let mut sessions = registry.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .entry("session-a".to_string())
                .or_insert_with(|| SessionCheckpoint::new("session-a".to_string()))
                .add_entry(vec!["Action A".to_string()]);
        }

        {
            let mut sessions = registry.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .entry("session-b".to_string())
                .or_insert_with(|| SessionCheckpoint::new("session-b".to_string()))
                .add_entry(vec!["Action B1".to_string(), "Action B2".to_string()]);
        }

        // Verify both sessions exist.
        let format_all = registry.format_all();
        assert_eq!(format_all.len(), 2);
    }

    #[test]
    fn test_checkpoint_registry_remove() {
        let registry = CheckpointRegistry::new();

        {
            let mut sessions = registry.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .entry("session-x".to_string())
                .or_insert_with(|| SessionCheckpoint::new("session-x".to_string()))
                .add_entry(vec!["Something".to_string()]);
        }

        assert_eq!(registry.format_all().len(), 1);

        registry.remove("session-x");
        assert_eq!(registry.format_all().len(), 0);
    }

    #[test]
    fn test_checkpoint_entry_serde_roundtrip() {
        let entry = CheckpointEntry {
            index: 42,
            timestamp: "2026-08-14T12:00:00+00:00".to_string(),
            summary_lines: vec![
                "Read src/main.rs".to_string(),
                "Edited config.toml".to_string(),
            ],
        };

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: CheckpointEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.index, 42);
        assert_eq!(deserialized.timestamp, entry.timestamp);
        assert_eq!(deserialized.summary_lines, entry.summary_lines);
    }

    #[test]
    fn test_checkpoint_registry_format_all_skips_empty() {
        let registry = CheckpointRegistry::new();

        // Create an empty session (no entries) — modify via direct lock access.
        {
            let mut sessions = registry.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .entry("empty-session".to_string())
                .or_insert_with(|| SessionCheckpoint::new("empty-session".to_string()));
        }

        // Create a session with entries.
        {
            let mut sessions = registry.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .entry("active-session".to_string())
                .or_insert_with(|| SessionCheckpoint::new("active-session".to_string()))
                .add_entry(vec!["Something happened".to_string()]);
        }

        let formatted = registry.format_all();
        assert_eq!(formatted.len(), 1);
        assert_eq!(formatted[0].0, "active-session");
    }

    // ─── CheckpointWriter disk persistence tests ──────────────────────

    #[test]
    fn test_checkpoint_writer_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let writer =
            CheckpointWriter::new_in_dir(tmp.path().to_path_buf(), "test-session").unwrap();

        let entry = CheckpointEntry {
            index: 1,
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary_lines: vec![
                "Read src/lib.rs".to_string(),
                "Added pub mod config;".to_string(),
            ],
        };
        writer.write_entry(&entry).unwrap();

        let content = writer.read_contents();
        assert!(content.contains("SWAI Session Checkpoint Log"));
        assert!(content.contains("test-session"));
        assert!(content.contains("Read src/lib.rs"));
    }

    #[test]
    fn test_checkpoint_writer_incremental_append() {
        let tmp = tempfile::tempdir().unwrap();
        let writer =
            CheckpointWriter::new_in_dir(tmp.path().to_path_buf(), "append-session").unwrap();

        // First entry creates the file with header.
        let entry1 = CheckpointEntry {
            index: 1,
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary_lines: vec!["Read src/lib.rs".to_string(), "Edited main.rs".to_string()],
        };
        writer.write_entry(&entry1).unwrap();

        // Second entry appends.
        let entry2 = CheckpointEntry {
            index: 2,
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary_lines: vec!["Ran command: cargo build".to_string()],
        };
        writer.write_entry(&entry2).unwrap();

        let content = writer.read_contents();
        // Both entries should be present.
        assert!(content.contains("Read src/lib.rs"));
        assert!(content.contains("Edited main.rs"));
        assert!(content.contains("Ran command: cargo build"));
        // Should have two checkpoint sections.
        assert!(content.contains("## Checkpoint #1"));
        assert!(content.contains("## Checkpoint #2"));
    }

    #[test]
    fn test_checkpoint_writer_snapshot_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let writer =
            CheckpointWriter::new_in_dir(tmp.path().to_path_buf(), "snapshot-session").unwrap();

        // Write some initial entries via write_entry.
        let entry1 = CheckpointEntry {
            index: 1,
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary_lines: vec!["Old action".to_string()],
        };
        writer.write_entry(&entry1).unwrap();

        // Now write a full snapshot (overwrites).
        let mut session = SessionCheckpoint::new("snapshot-session".to_string());
        session.add_entry(vec!["New action A".to_string(), "New action B".to_string()]);
        session.add_entry(vec!["Another action".to_string()]);
        writer.write_snapshot(&session).unwrap();

        let content = writer.read_contents();
        assert!(content.contains("New action A"));
        assert!(content.contains("New action B"));
        assert!(content.contains("Another action"));
        assert!(!content.contains("Old action"));
        // Should have two checkpoint sections from the snapshot.
        assert!(content.contains("## Checkpoint #1"));
        assert!(content.contains("## Checkpoint #2"));
    }

    #[test]
    fn test_checkpoint_writer_read_nonexistent_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let writer =
            CheckpointWriter::new_in_dir(tmp.path().to_path_buf(), "nonexistent-session").unwrap();
        assert_eq!(writer.read_contents(), "");
    }

    #[test]
    fn test_checkpoint_writer_to_disk_format() {
        let mut sc = SessionCheckpoint::new("format-test".to_string());
        sc.add_entry(vec![
            "Read src/lib.rs, core/src/config.rs".to_string(),
            "Added pub mod update_checker;".to_string(),
        ]);
        sc.add_entry(vec!["Ran cargo check --workspace: passed".to_string()]);

        let formatted = sc.to_disk_format();
        assert!(formatted.starts_with("# SWAI Session Checkpoint Log"));
        assert!(formatted.contains("`format-test`"));
        assert!(formatted.contains("## Checkpoint #1 (2 messages compacted)"));
        assert!(formatted.contains("## Checkpoint #2 (1 messages compacted)"));
        assert!(formatted.contains("1. Read src/lib.rs, core/src/config.rs"));
        assert!(formatted.contains("2. Added pub mod update_checker;"));
        assert!(formatted.contains("3. Ran cargo check --workspace: passed"));
    }

    #[test]
    fn test_checkpoint_writer_default_base_dir() {
        let base = CheckpointWriter::default_base_dir();
        // Should end with checkpoints/
        assert!(
            base.to_string_lossy().ends_with("checkpoints")
                || base.to_string_lossy().ends_with("checkpoints\\")
        );
    }

    #[test]
    fn test_checkpoint_writer_multiple_instances_append_without_overwriting() {
        let tmp = tempfile::tempdir().unwrap();

        // First request / compaction event (creates file)
        let writer1 =
            CheckpointWriter::new_in_dir(tmp.path().to_path_buf(), "multi-compaction").unwrap();
        let idx1 = writer1.next_checkpoint_index();
        assert_eq!(idx1, 1);
        let entry1 = CheckpointEntry {
            index: idx1,
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary_lines: vec!["Read core/src/lib.rs".to_string()],
        };
        writer1
            .write_entry_with_objective(&entry1, Some("Build feature X"))
            .unwrap();

        // Second request / compaction event (fresh writer instance, must NOT overwrite)
        let writer2 =
            CheckpointWriter::new_in_dir(tmp.path().to_path_buf(), "multi-compaction").unwrap();
        let idx2 = writer2.next_checkpoint_index();
        assert_eq!(idx2, 2);
        let entry2 = CheckpointEntry {
            index: idx2,
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary_lines: vec!["Read core/src/config.rs".to_string()],
        };
        writer2
            .write_entry_with_objective(&entry2, Some("Build feature X"))
            .unwrap();

        // Third request / compaction event (fresh writer instance)
        let writer3 =
            CheckpointWriter::new_in_dir(tmp.path().to_path_buf(), "multi-compaction").unwrap();
        let idx3 = writer3.next_checkpoint_index();
        assert_eq!(idx3, 3);
        let entry3 = CheckpointEntry {
            index: idx3,
            timestamp: chrono::Utc::now().to_rfc3339(),
            summary_lines: vec!["Wrote core/src/feature.rs".to_string()],
        };
        writer3
            .write_entry_with_objective(&entry3, Some("Build feature X"))
            .unwrap();

        let content = writer3.read_contents();
        assert!(content.contains("**Initial Objective:** `Build feature X`"));
        assert!(content.contains("## Checkpoint #1"));
        assert!(content.contains("Read core/src/lib.rs"));
        assert!(content.contains("## Checkpoint #2"));
        assert!(content.contains("Read core/src/config.rs"));
        assert!(content.contains("## Checkpoint #3"));
        assert!(content.contains("Wrote core/src/feature.rs"));
        assert_eq!(writer3.existing_checkpoint_count(), 3);
    }
}
