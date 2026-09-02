//! Context-size inference & script synchronization helpers.
//!
//! Parses `--ctx-size <N>`, `-c <N>`, `--ctx_size <N>`, `--ctx-size=<N>`,
//! and `CTX_SIZE=<N>` patterns from bash scripts, and rewrites them while
//! preserving surrounding formatting and comments.

use std::path::Path;
use std::sync::OnceLock;

/// Regex matching `--ctx-size <N>`, `--ctx_size <N>`, `--ctx-size=<N>`,
/// `--ctx_size=<N>`. Matches the flag and captures the numeric value.
fn ctx_size_long_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"--(ctx.size|ctx_size)[= \t]+(\d+)")
            .expect("ctx_size_long regex must compile")
    })
}

/// Regex matching `-c <N>`. Standalone single-char short flag.
fn ctx_size_short_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(^|[ \t])-c[= \t]+(\d+)").expect("ctx_size_short regex must compile")
    })
}

/// Regex matching `CTX_SIZE=<N>` (variable assignment, optional `export`).
fn ctx_size_var_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"(export[ \t]+)?CTX_SIZE=(\d+)"#)
            .expect("ctx_size_var regex must compile")
    })
}

/// Detect the context size from a bash script's content.
///
/// Looks for `--ctx-size <N>`, `-c <N>`, `--ctx_size <N>`, `--ctx-size=<N>`,
/// or `CTX_SIZE=<N>`. Returns `None` if no valid ctx-size is found.
///
/// Ignores commented lines (lines whose first non-whitespace character is `#`).
pub fn detect_ctx_size(script_content: &str) -> Option<usize> {
    for line in script_content.lines() {
        let trimmed = line.trim();
        // Skip comment-only lines.
        if trimmed.starts_with('#') {
            continue;
        }

        if let Some(size) = find_ctx_size_in_line(trimmed) {
            return Some(size);
        }
    }
    None
}

/// Look for a ctx-size match in a single non-comment line.
fn find_ctx_size_in_line(line: &str) -> Option<usize> {
    // 1. Long flag: --ctx-size N, --ctx_size N, --ctx-size=N, --ctx_size=N
    if let Some(caps) = ctx_size_long_re().captures(line) {
        if let Some(m) = caps.get(2) {
            if let Ok(n) = m.as_str().parse::<usize>() {
                return Some(n);
            }
        }
    }

    // 2. Short flag: -c N (only when not preceded by another `-` to avoid
    //    matching `--something`).
    if let Some(caps) = ctx_size_short_re().captures(line) {
        if let Some(m) = caps.get(2) {
            if let Ok(n) = m.as_str().parse::<usize>() {
                return Some(n);
            }
        }
    }

    // 3. Variable assignment: CTX_SIZE=N or export CTX_SIZE=N
    if let Some(caps) = ctx_size_var_re().captures(line) {
        if let Some(m) = caps.get(2) {
            if let Ok(n) = m.as_str().parse::<usize>() {
                return Some(n);
            }
        }
    }

    None
}

/// Synchronize the context size inside a `.sh` launch script.
///
/// Replaces the existing `--ctx-size` / `-c` / `--ctx_size` / `--ctx-size=` /
/// `CTX_SIZE=` argument with the new value while preserving comments,
/// formatting, and surrounding text.
///
/// Returns an error if the script cannot be read or written.
pub fn sync_ctx_size_in_script(script_path: &Path, new_ctx: usize) -> Result<(), String> {
    let new_ctx_str = new_ctx.to_string();

    // Read current script content.
    let content = std::fs::read_to_string(script_path)
        .map_err(|e| format!("Failed to read script {}: {}", script_path.display(), e))?;

    let mut updated_lines: Vec<String> = Vec::with_capacity(content.len().saturating_div(16));

    // Track whether the original file ended with a newline so we can
    // preserve it exactly.
    let had_trailing_newline = content.ends_with('\n');

    // Split on '\n' but keep the trailing newline marker if present.
    let body: &str = if had_trailing_newline {
        &content[..content.len() - 1]
    } else {
        &content
    };

    for line in body.split('\n') {
        let trimmed = line.trim();

        // Skip comment-only lines entirely — never rewrite a comment.
        if trimmed.starts_with('#') {
            updated_lines.push(line.to_string());
            continue;
        }

        let mut current = line.to_string();

        // 1. Long flag: --ctx-size N, --ctx_size N, --ctx-size=N, --ctx_size=N
        current = replace_first(&current, ctx_size_long_re(), &new_ctx_str);

        // 2. Short flag: -c N
        current = replace_first(&current, ctx_size_short_re(), &new_ctx_str);

        // 3. Variable assignment: CTX_SIZE=N or export CTX_SIZE=N
        current = replace_first(&current, ctx_size_var_re(), &new_ctx_str);

        updated_lines.push(current);
    }

    let mut new_content = updated_lines.join("\n");
    if had_trailing_newline {
        new_content.push('\n');
    }

    // Only write if something actually changed — avoids unnecessary mtime bumps.
    if new_content != content {
        std::fs::write(script_path, &new_content)
            .map_err(|e| format!("Failed to write script {}: {}", script_path.display(), e))?;
        tracing::info!(
            "Synced ctx-size {} into script {}",
            new_ctx_str,
            script_path.display()
        );
    } else {
        tracing::debug!(
            "No ctx-size assignments found in {}; skipping rewrite.",
            script_path.display()
        );
    }

    Ok(())
}

/// Apply a single regex replacement to `input`, replacing only the capture
/// group that holds the numeric value with `replacement`. All other parts of
/// the match (prefix, surrounding text) are preserved verbatim.
///
/// Returns the original string unchanged when no match is found.
fn replace_first(input: &str, re: &regex::Regex, replacement: &str) -> String {
    let caps = match re.captures(input) {
        Some(c) => c,
        None => return input.to_string(),
    };

    // Find the group that contains just the bare digits (the numeric value).
    // Group 1 is the prefix (e.g. "export " or whitespace), group 2 is the value.
    // Prefer group 2 (the numeric capture).
    let value_group = if caps.len() > 2 { 2 } else { 1 };

    if let Some(m) = caps.get(value_group) {
        let mut result = input.to_string();
        result.replace_range(m.range(), replacement);
        return result;
    }

    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_ctx_size_long_flag() {
        let script = "#!/bin/bash\nllama-server --ctx-size 131072\n";
        assert_eq!(detect_ctx_size(script), Some(131072));
    }

    #[test]
    fn test_detect_ctx_size_short_flag() {
        let script = "#!/bin/bash\nllama-server -c 65536\n";
        assert_eq!(detect_ctx_size(script), Some(65536));
    }

    #[test]
    fn test_detect_ctx_size_underscore_flag() {
        let script = "#!/bin/bash\nllama-server --ctx_size 262144\n";
        assert_eq!(detect_ctx_size(script), Some(262144));
    }

    #[test]
    fn test_detect_ctx_size_equals_flag() {
        let script = "#!/bin/bash\nllama-server --ctx-size=32768\n";
        assert_eq!(detect_ctx_size(script), Some(32768));
    }

    #[test]
    fn test_detect_ctx_size_env_var() {
        let script = "#!/bin/bash\nCTX_SIZE=131072\nllama-server --ctx-size $CTX_SIZE\n";
        assert_eq!(detect_ctx_size(script), Some(131072));
    }

    #[test]
    fn test_detect_ctx_size_export_var() {
        let script = "#!/bin/bash\nexport CTX_SIZE=262144\n";
        assert_eq!(detect_ctx_size(script), Some(262144));
    }

    #[test]
    fn test_detect_ctx_size_comment_ignored() {
        let script = "#!/bin/bash\n# --ctx-size 999999\nllama-server --ctx-size 65536\n";
        assert_eq!(detect_ctx_size(script), Some(65536));
    }

    #[test]
    fn test_detect_ctx_size_no_match() {
        let script = "#!/bin/bash\necho hello\n";
        assert_eq!(detect_ctx_size(script), None);
    }

    #[test]
    fn test_detect_ctx_size_empty() {
        assert_eq!(detect_ctx_size(""), None);
    }

    #[test]
    fn test_detect_ctx_size_indented_comment() {
        let script = "#!/bin/bash\n  # --ctx-size 999999\nllama-server --ctx-size 65536\n";
        assert_eq!(detect_ctx_size(script), Some(65536));
    }

    #[test]
    fn test_detect_ctx_size_precedence_long_over_short() {
        // When both --ctx-size and -c are present, --ctx-size should win
        // because it's checked first.
        let script = "#!/bin/bash\nllama-server -c 1000 --ctx-size 65536\n";
        assert_eq!(detect_ctx_size(script), Some(65536));
    }

    #[test]
    fn test_detect_ctx_size_only_short_flag() {
        let script = "#!/bin/bash\nllama-server -c 32768\n";
        assert_eq!(detect_ctx_size(script), Some(32768));
    }

    #[test]
    fn test_detect_ctx_size_equals_underscore() {
        let script = "#!/bin/bash\nllama-server --ctx_size=131072\n";
        assert_eq!(detect_ctx_size(script), Some(131072));
    }

    #[test]
    fn test_sync_ctx_size_long_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("test.sh");
        std::fs::write(
            &script_path,
            "#!/bin/bash\nexec llama-server --ctx-size 65536 --port 8080\n",
        )
        .unwrap();

        sync_ctx_size_in_script(&script_path, 131072).unwrap();

        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("--ctx-size 131072"),
            "Expected --ctx-size 131072, got:\n{}",
            content
        );
        assert!(
            content.contains("--port 8080"),
            "Port should be preserved, got:\n{}",
            content
        );
    }

    #[test]
    fn test_sync_ctx_size_short_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("test.sh");
        std::fs::write(&script_path, "#!/bin/bash\nexec llama-server -c 65536\n").unwrap();

        sync_ctx_size_in_script(&script_path, 262144).unwrap();

        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("-c 262144"),
            "Expected -c 262144, got:\n{}",
            content
        );
    }

    #[test]
    fn test_sync_ctx_size_env_var() {
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("test.sh");
        std::fs::write(
            &script_path,
            "#!/bin/bash\nCTX_SIZE=65536\nexec llama-server --ctx-size $CTX_SIZE\n",
        )
        .unwrap();

        sync_ctx_size_in_script(&script_path, 131072).unwrap();

        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("CTX_SIZE=131072"),
            "Expected CTX_SIZE=131072, got:\n{}",
            content
        );
        // The --ctx-size $CTX_SIZE line should remain unchanged.
        assert!(
            content.contains("--ctx-size $CTX_SIZE"),
            "Expected --ctx-size $CTX_SIZE preserved, got:\n{}",
            content
        );
    }

    #[test]
    fn test_sync_ctx_size_preserves_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("test.sh");
        std::fs::write(
            &script_path,
            "#!/bin/bash\n# This sets the context size\n--ctx-size 65536\n# End of config\n",
        )
        .unwrap();

        sync_ctx_size_in_script(&script_path, 131072).unwrap();

        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("# This sets the context size"),
            "Comment should be preserved, got:\n{}",
            content
        );
        assert!(
            content.contains("# End of config"),
            "Comment should be preserved, got:\n{}",
            content
        );
        assert!(
            content.contains("--ctx-size 131072"),
            "Value should be updated, got:\n{}",
            content
        );
    }

    #[test]
    fn test_sync_ctx_size_no_match_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("test.sh");
        let original = "#!/bin/bash\necho hello\n";
        std::fs::write(&script_path, original).unwrap();

        sync_ctx_size_in_script(&script_path, 131072).unwrap();

        let content = std::fs::read_to_string(&script_path).unwrap();
        assert_eq!(content, original);
    }

    #[test]
    fn test_sync_ctx_size_equals_format() {
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("test.sh");
        std::fs::write(
            &script_path,
            "#!/bin/bash\nexec llama-server --ctx-size=65536\n",
        )
        .unwrap();

        sync_ctx_size_in_script(&script_path, 262144).unwrap();

        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("--ctx-size=262144"),
            "Expected --ctx-size=262144, got:\n{}",
            content
        );
    }

    #[test]
    fn test_sync_ctx_size_export_var() {
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("test.sh");
        std::fs::write(
            &script_path,
            "#!/bin/bash\nexport CTX_SIZE=65536\nexec llama-server\n",
        )
        .unwrap();

        sync_ctx_size_in_script(&script_path, 131072).unwrap();

        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("export CTX_SIZE=131072"),
            "Expected export CTX_SIZE=131072, got:\n{}",
            content
        );
    }

    #[test]
    fn test_sync_ctx_size_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("test.sh");
        std::fs::write(
            &script_path,
            "#!/bin/bash\nexec llama-server --ctx-size 65536\n",
        )
        .unwrap();

        sync_ctx_size_in_script(&script_path, 131072).unwrap();
        let content1 = std::fs::read_to_string(&script_path).unwrap();

        // Running again with the same value should not change the file.
        sync_ctx_size_in_script(&script_path, 131072).unwrap();
        let content2 = std::fs::read_to_string(&script_path).unwrap();

        assert_eq!(content1, content2);
    }
}
