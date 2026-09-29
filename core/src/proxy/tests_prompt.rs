//! Tests for prompt and workspace extraction in proxy.

#[cfg(test)]
mod tests {
    use crate::proxy::prompt::*;

    #[test]
    fn test_extract_workspace_top_level_cwd() {
        let json = serde_json::json!({
            "cwd": "/mnt/orico/Documents/ApplicationsRAW/swai",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/mnt/orico/Documents/ApplicationsRAW/swai".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_system_string() {
        let json = serde_json::json!({
            "system": "You are Claude Code.\nWorking directory: /var/log\nOther info",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/var/log".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_system_array() {
        let json = serde_json::json!({
            "system": [
                {"type": "text", "text": "Environment context:\nWorking directory: /tmp\n"}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/tmp".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_user_message_xml_tag() {
        let json = serde_json::json!({
            "messages": [
                {"role": "user", "content": "<cwd>/tmp</cwd>\nFix this bug"}
            ]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/tmp".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_user_message_blocks() {
        let json = serde_json::json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Current working directory: /tmp\nProceed"}
                    ]
                }
            ]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/tmp".into())
        );
    }

    #[test]
    fn test_extract_workspace_from_context_object() {
        let json = serde_json::json!({
            "context": {"cwd": "/tmp"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = serde_json::to_vec(&json).unwrap();
        assert_eq!(
            extract_workspace_from_body(&body),
            Some("/tmp".into())
        );
    }
}
