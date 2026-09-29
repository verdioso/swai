//! Tests for Council Tools module.

#[cfg(test)]
mod tests {
    use crate::council::tools::*;
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
