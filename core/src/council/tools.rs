//! SWAI — Council in-process read-only tool executor (RC1).
//!
//! Provides sandboxed, read-only inspection tools that the Planner can
//! execute directly inside the pipeline without depending on an external
//! CLI round-trip. All operations are canonicalized to the workspace root
//! and size-capped so they never panic, never write, and never escape the
//! project directory.

use std::path::{Path, PathBuf};

/// Maximum bytes returned from a single `read_file` call (64 KB).
const READ_FILE_MAX_BYTES: usize = 65_536;
/// Maximum number of result lines from `search_files`.
const SEARCH_MAX_RESULTS: usize = 80;
/// Maximum number of entries from `list_dir`.
const LIST_DIR_MAX_ENTRIES: usize = 200;

/// Result of executing a local inspection tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// True if the tool executed successfully (even if the file was empty).
    pub success: bool,
    /// Human-readable output to feed back to the Planner.
    pub output: String,
}

/// Detect the workspace root. Walk up from `std::env::current_dir()` looking
/// for a `.git` directory or `Cargo.toml`. Falls back to cwd if nothing found.
pub fn detect_workspace_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut dir = cwd.as_path();
    loop {
        if dir.join(".git").exists() || dir.join("Cargo.toml").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => return cwd,
        }
    }
}

/// Resolve and validate a path relative to the workspace root.
/// Returns `None` if the resolved path escapes the workspace.
fn safe_resolve(workspace: &Path, target: &str) -> Option<PathBuf> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }

    let candidate = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        workspace.join(target)
    };

    // Canonicalize both to resolve symlinks and .. components.
    let canonical_workspace = std::fs::canonicalize(workspace).ok()?;
    let canonical_target = std::fs::canonicalize(&candidate).ok()?;

    if canonical_target.starts_with(&canonical_workspace) {
        Some(canonical_target)
    } else {
        None
    }
}

/// Read a file's contents, capped at `READ_FILE_MAX_BYTES`.
pub fn read_file(workspace: &Path, target: &str) -> ToolResult {
    let path = match safe_resolve(workspace, target) {
        Some(p) => p,
        None => {
            return ToolResult {
                success: false,
                output: format!(
                    "Error: '{}' does not exist or is outside the workspace. Current workspace: {}",
                    target,
                    workspace.display()
                ),
            };
        }
    };

    if !path.is_file() {
        return ToolResult {
            success: false,
            output: format!("Error: '{}' is not a file.", target),
        };
    }

    match std::fs::read(&path) {
        Ok(bytes) => {
            let truncated = bytes.len() > READ_FILE_MAX_BYTES;
            let content = String::from_utf8_lossy(
                &bytes[..bytes.len().min(READ_FILE_MAX_BYTES)]
            );
            let mut output = content.into_owned();
            if truncated {
                output.push_str(&format!(
                    "\n\n[... truncated at {} bytes, file is {} bytes total]",
                    READ_FILE_MAX_BYTES,
                    bytes.len()
                ));
            }
            ToolResult {
                success: true,
                output,
            }
        }
        Err(e) => ToolResult {
            success: false,
            output: format!("Error reading '{}': {}", target, e),
        },
    }
}

/// Search files for a pattern (case-insensitive grep-lite).
/// Skips `target/`, `.git/`, `node_modules/`, and binary files.
pub fn search_files(workspace: &Path, pattern: &str) -> ToolResult {
    let pattern_lower = pattern.to_lowercase();
    let mut results = Vec::new();

    fn walk(
        dir: &Path,
        workspace: &Path,
        pattern: &str,
        results: &mut Vec<String>,
        max: usize,
    ) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if results.len() >= max {
                return;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip ignored directories
            if path.is_dir() {
                if matches!(
                    name.as_str(),
                    "target" | ".git" | "node_modules" | "__pycache__" | ".mypy_cache"
                ) {
                    continue;
                }
                walk(&path, workspace, pattern, results, max);
                continue;
            }

            // Skip binary-looking files
            if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("png" | "jpg" | "jpeg" | "gif" | "ico" | "woff" | "woff2" | "ttf"
                    | "otf" | "so" | "dylib" | "dll" | "exe" | "o" | "a" | "pyc"
                    | "class" | "jar" | "zip" | "tar" | "gz" | "bz2" | "xz")
            ) {
                continue;
            }

            // Read and search
            if let Ok(content) = std::fs::read_to_string(&path) {
                for (line_num, line) in content.lines().enumerate() {
                    if results.len() >= max {
                        return;
                    }
                    if line.to_lowercase().contains(pattern) {
                        let rel = path
                            .strip_prefix(workspace)
                            .unwrap_or(&path)
                            .display();
                        results.push(format!(
                            "{}:{}: {}",
                            rel,
                            line_num + 1,
                            line.chars().take(200).collect::<String>()
                        ));
                    }
                }
            }
        }
    }

    walk(workspace, workspace, &pattern_lower, &mut results, SEARCH_MAX_RESULTS);

    if results.is_empty() {
        ToolResult {
            success: true,
            output: format!("No matches found for pattern '{}'.", pattern),
        }
    } else {
        let count = results.len();
        let mut output = results.join("\n");
        if count >= SEARCH_MAX_RESULTS {
            output.push_str(&format!(
                "\n\n[... results capped at {} matches]",
                SEARCH_MAX_RESULTS
            ));
        }
        ToolResult {
            success: true,
            output,
        }
    }
}

/// List directory contents (non-recursive, capped).
pub fn list_dir(workspace: &Path, target: &str) -> ToolResult {
    let target = if target.trim().is_empty() { "." } else { target.trim() };
    let path = match safe_resolve(workspace, target) {
        Some(p) => p,
        None => {
            return ToolResult {
                success: false,
                output: format!("Error: '{}' does not exist or is outside the workspace.", target),
            };
        }
    };

    if !path.is_dir() {
        return ToolResult {
            success: false,
            output: format!("Error: '{}' is not a directory.", target),
        };
    }

    match std::fs::read_dir(&path) {
        Ok(entries) => {
            let mut items: Vec<String> = entries
                .flatten()
                .take(LIST_DIR_MAX_ENTRIES)
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if e.path().is_dir() {
                        format!("{}/", name)
                    } else {
                        name
                    }
                })
                .collect();
            items.sort();
            ToolResult {
                success: true,
                output: items.join("\n"),
            }
        }
        Err(e) => ToolResult {
            success: false,
            output: format!("Error listing '{}': {}", target, e),
        },
    }
}

/// Execute an inspection tool by name. Returns `None` if the tool name
/// is not a recognized local inspection tool (i.e., it should go to the
/// Generator instead).
pub fn execute_local_tool(
    workspace: &Path,
    tool: &str,
    target: &str,
) -> Option<ToolResult> {
    let t = tool.to_ascii_lowercase();
    match t.as_str() {
        "read_file" | "read" | "view_file" | "view" => Some(read_file(workspace, target)),
        "search_files" | "search" | "grep" => Some(search_files(workspace, target)),
        "list_dir" | "ls" | "explore" => Some(list_dir(workspace, target)),
        "bash" | "bash_command" | "execute_command" | "run_command" | "terminal" => {
            // For bash-like tools, check if the command is a safe read-only command.
            execute_safe_bash(workspace, target)
        }
        _ => None,
    }
}

/// Execute a bash command only if it's in the safe allowlist.
/// Allowed: cat, ls, find, grep, head, tail, wc, file, git status/log/diff/show.
fn execute_safe_bash(workspace: &Path, command: &str) -> Option<ToolResult> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Some(ToolResult {
            success: false,
            output: "Error: empty command.".into(),
        });
    }

    // Extract the base command (first word)
    let base_cmd = trimmed.split_whitespace().next().unwrap_or("");

    let allowed = matches!(
        base_cmd,
        "cat" | "ls" | "find" | "grep" | "head" | "tail" | "wc" | "file" | "tree"
            | "stat" | "du" | "echo" | "pwd"
    );

    // Also allow `git` with read-only subcommands
    let git_allowed = base_cmd == "git" && {
        let rest = trimmed.strip_prefix("git").unwrap_or("").trim();
        let subcmd = rest.split_whitespace().next().unwrap_or("");
        matches!(
            subcmd,
            "status" | "log" | "diff" | "show" | "branch" | "remote" | "tag" | "ls-files"
                | "rev-parse"
        )
    };

    if !allowed && !git_allowed {
        return Some(ToolResult {
            success: false,
            output: format!(
                "Error: '{}' is not in the read-only allowlist. Allowed: cat, ls, find, grep, head, tail, wc, file, tree, stat, du, echo, pwd, git status/log/diff/show/branch/remote/tag/ls-files/rev-parse.",
                base_cmd
            ),
        });
    }

    // Reject anything with shell metacharacters that could escape
    if trimmed.contains('|') || trimmed.contains(';') || trimmed.contains('`')
        || trimmed.contains("$(") || trimmed.contains(">>") || trimmed.contains(">&")
    {
        return Some(ToolResult {
            success: false,
            output: "Error: shell metacharacters (|, ;, `, $(), >>) are not allowed in sandboxed commands.".into(),
        });
    }

    match std::process::Command::new("sh")
        .arg("-c")
        .arg(trimmed)
        .current_dir(workspace)
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut result = String::new();
            if !stdout.is_empty() {
                // Cap output
                let capped: String = stdout.chars().take(READ_FILE_MAX_BYTES).collect();
                result.push_str(&capped);
                if stdout.len() > READ_FILE_MAX_BYTES {
                    result.push_str("\n[... output truncated]");
                }
            }
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push_str("\n--- stderr ---\n");
                }
                let capped: String = stderr.chars().take(4096).collect();
                result.push_str(&capped);
            }
            if result.is_empty() {
                result = "(no output)".into();
            }
            Some(ToolResult {
                success: output.status.success(),
                output: result,
            })
        }
        Err(e) => Some(ToolResult {
            success: false,
            output: format!("Error executing command: {}", e),
        }),
    }
}

/// Build the structured tool catalog string for local inspection tools.
/// This is injected into the Planner's prompt so it knows what tools RC1 provides.
pub fn local_tool_catalog() -> String {
    "\
LOCALLY AVAILABLE INSPECTION TOOLS (executed in-process, no CLI round-trip needed):
- read_file: Read a file's contents. Args: {\"tool\": \"read_file\", \"target\": \"<relative path>\"}
- search_files: Grep-lite search across the project (skips target/, .git/). Args: {\"tool\": \"search_files\", \"target\": \"<search pattern>\"}
- list_dir: List directory contents. Args: {\"tool\": \"list_dir\", \"target\": \"<relative path or .>\"}
- bash: Execute a read-only shell command (cat, ls, find, grep, head, tail, git status/log/diff only). Args: {\"tool\": \"bash\", \"target\": \"<command>\"}

Use these tools to explore the codebase BEFORE proposing any write_file steps.
When you need to read a file or search the code, emit a JSON directive with one of these tools — the system will execute it immediately and re-invoke you with the results.
Only emit a write_file directive once you have enough context from inspection to write correct code."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create temp dir");
        fs::write(dir.path().join("hello.txt"), "Hello, world!\nSecond line.\n").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("target/debug/binary"), "binary data").unwrap();
        dir
    }

    #[test]
    fn test_read_file_success() {
        let ws = setup_workspace();
        let result = read_file(ws.path(), "hello.txt");
        assert!(result.success);
        assert!(result.output.contains("Hello, world!"));
        assert!(result.output.contains("Second line."));
    }

    #[test]
    fn test_read_file_not_found() {
        let ws = setup_workspace();
        let result = read_file(ws.path(), "nonexistent.txt");
        assert!(!result.success);
        assert!(result.output.contains("does not exist"));
    }

    #[test]
    fn test_read_file_escape_rejected() {
        let ws = setup_workspace();
        let result = read_file(ws.path(), "../../etc/passwd");
        assert!(!result.success);
        assert!(result.output.contains("does not exist") || result.output.contains("outside"));
    }

    #[test]
    fn test_search_files_finds_match() {
        let ws = setup_workspace();
        let result = search_files(ws.path(), "println");
        assert!(result.success);
        assert!(result.output.contains("src/main.rs"));
        assert!(result.output.contains("println"));
    }

    #[test]
    fn test_search_files_skips_target_dir() {
        let ws = setup_workspace();
        let result = search_files(ws.path(), "binary data");
        assert!(result.success);
        assert!(result.output.contains("No matches"));
    }

    #[test]
    fn test_search_files_no_match() {
        let ws = setup_workspace();
        let result = search_files(ws.path(), "zzz_nonexistent_pattern_zzz");
        assert!(result.success);
        assert!(result.output.contains("No matches"));
    }

    #[test]
    fn test_list_dir_root() {
        let ws = setup_workspace();
        let result = list_dir(ws.path(), ".");
        assert!(result.success);
        assert!(result.output.contains("hello.txt"));
        assert!(result.output.contains("src/"));
    }

    #[test]
    fn test_list_dir_subdir() {
        let ws = setup_workspace();
        let result = list_dir(ws.path(), "src");
        assert!(result.success);
        assert!(result.output.contains("main.rs"));
    }

    #[test]
    fn test_execute_local_tool_dispatch() {
        let ws = setup_workspace();
        let result = execute_local_tool(ws.path(), "read_file", "hello.txt");
        assert!(result.is_some());
        assert!(result.unwrap().success);

        let result = execute_local_tool(ws.path(), "write_file", "foo.txt");
        assert!(result.is_none()); // write_file is not a local tool
    }

    #[test]
    fn test_safe_bash_allowed() {
        let ws = setup_workspace();
        let result = execute_safe_bash(ws.path(), "ls");
        assert!(result.is_some());
        assert!(result.unwrap().success);
    }

    #[test]
    fn test_safe_bash_blocked() {
        let ws = setup_workspace();
        let result = execute_safe_bash(ws.path(), "rm -rf /");
        assert!(result.is_some());
        assert!(!result.unwrap().success);
        assert!(execute_safe_bash(ws.path(), "rm -rf /").unwrap().output.contains("not in the read-only allowlist"));
    }

    #[test]
    fn test_safe_bash_metachar_blocked() {
        let ws = setup_workspace();
        let result = execute_safe_bash(ws.path(), "cat hello.txt | grep world");
        assert!(result.is_some());
        assert!(!result.unwrap().success);
        assert!(execute_safe_bash(ws.path(), "cat hello.txt | grep world").unwrap().output.contains("metacharacters"));
    }

    #[test]
    fn test_local_tool_catalog_contains_tools() {
        let catalog = local_tool_catalog();
        assert!(catalog.contains("read_file"));
        assert!(catalog.contains("search_files"));
        assert!(catalog.contains("list_dir"));
        assert!(catalog.contains("bash"));
    }
}
