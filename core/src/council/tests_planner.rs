//! Tests for Council Planner stage.

#[cfg(test)]
mod tests {
    use crate::council::planner::*;

    #[test]
    fn test_planner_prompt_construction() {
        let p = build_planner_prompt("Add auth", "- write_file: write a file");
        assert!(p.contains("Lead Software Architect"));
        assert!(p.contains("Add auth"));
        assert!(p.contains("write_file"));
    }

    #[test]
    fn test_parse_planner_directive_clean_json() {
        let json = r#"{
            "tool": "write_file",
            "target": "core/Cargo.toml",
            "action": "Add keyring dependency",
            "rules": "Atomic edit"
        }"#;
        let parsed = parse_planner_directive(json).expect("should parse");
        assert_eq!(parsed.tool, "write_file");
        assert_eq!(parsed.target, "core/Cargo.toml");
        assert_eq!(parsed.action, "Add keyring dependency");
    }

    #[test]
    fn test_parse_planner_directive_without_rules() {
        let json = r#"{
            "tool": "read_file",
            "target": "PLAN/PHASES/phase35.md",
            "action": "Read the Phase 35 specification"
        }"#;
        let parsed = parse_planner_directive(json).expect("should parse without rules");
        assert_eq!(parsed.tool, "read_file");
        assert_eq!(parsed.target, "PLAN/PHASES/phase35.md");
    }

    #[test]
    fn test_parse_planner_directive_with_surrounding_text() {
        let text = "Here is my architecture plan:\n```json\n{\"tool\": \"write_file\", \"target\": \"src/main.rs\", \"action\": \"Update main\", \"rules\": \"None\"}\n```\nProceed with care.";
        let parsed = parse_planner_directive(text).expect("should parse embedded JSON");
        assert_eq!(parsed.tool, "write_file");
        assert_eq!(parsed.target, "src/main.rs");
    }

    #[test]
    fn test_build_scoped_generator_prompt() {
        let dir = PlannerDirective {
            tool: "write_file".into(),
            target: "core/Cargo.toml".into(),
            action: "Add keyring dependency".into(),
            rules: "Only Cargo.toml".into(),
        };
        let prompt = build_scoped_generator_prompt(&dir, "Add keyring and implement secure store");
        assert!(prompt.contains("core/Cargo.toml"));
        assert!(prompt.contains("Do NOT touch any other files"));
    }

    #[test]
    fn test_immediate_inspection_directive() {
        let dir = PlannerDirective {
            tool: "read_file".into(),
            target: "PLAN/PHASES/phase35.md".into(),
            action: "Read spec".into(),
            rules: "None".into(),
        };
        assert!(is_immediate_inspection_directive(&dir));
        let tool_call = format_immediate_tool_call(&dir);
        assert!(tool_call.contains("read_file"));
        assert!(tool_call.contains("PLAN/PHASES/phase35.md"));

        // Mutating bash command should NOT be an immediate inspection directive
        let mkdir_dir = PlannerDirective {
            tool: "bash".into(),
            target: "mkdir -p core/src/council/frontier".into(),
            action: "Create directory".into(),
            rules: "None".into(),
        };
        assert!(!is_immediate_inspection_directive(&mkdir_dir));
    }

    #[test]
    fn test_parse_planner_directive_nested_args_with_trailing_brace() {
        let text = "Let me start with council mod.rs\n```json\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"core/src/council/mod.rs\"}}}\n```";
        let parsed = parse_planner_directive(text).expect("should parse nested tool call with trailing brace");
        assert_eq!(parsed.tool, "read_file");
        assert_eq!(parsed.target, "core/src/council/mod.rs");
        assert!(is_immediate_inspection_directive(&parsed));
    }

    #[test]
    fn test_planner_prompt_with_execution_history() {
        let p = build_planner_prompt("Execution History & Tool Results:\ncore/Cargo.toml edited", "");
        assert!(p.contains("determine the NEXT unexecuted atomic step"));
        assert!(!p.contains("determine the FIRST immediate atomic step"));
    }

    #[test]
    fn test_parse_audit_verdict_approved() {
        let v = parse_audit_verdict("Critique here\n{\"status\": \"approved\", \"critique\": \"LGTM\"}").unwrap();
        assert!(v.approved);
        assert_eq!(v.critique, "LGTM");
    }

    #[test]
    fn test_parse_audit_verdict_changes_needed() {
        let v = parse_audit_verdict("{\"status\": \"changes_needed\", \"critique\": \"bug\"}").unwrap();
        assert!(!v.approved);
    }
}
