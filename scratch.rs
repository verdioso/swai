#[derive(Debug)]
pub struct PlannerDirective {
    pub tool: String,
    pub target: String,
    pub action: String,
    pub rules: String,
}

fn parse_planner_directive(output: &str) -> Option<PlannerDirective> {
    if let Some(start) = output.find("<tool_call>") {
        if let Some(end) = output[start..].find("</tool_call>") {
            // Wait, start + end + 12 could panic if it exceeds string length!
            let block = &output[start..std::cmp::min(start + end + 12, output.len())];
            let tool_name = if let Some(fn_start) = block.find("<function=") {
                let rest = &block[fn_start + 10..];
                rest.split('>').next().unwrap_or("").trim().to_string()
            } else {
                String::new()
            };
            let target = if let Some(p_start) = block.find("<parameter=") {
                let rest = &block[p_start..];
                if let Some(val_start) = rest.find('>') {
                    let val_body = &rest[val_start + 1..];
                    if let Some(val_end) = val_body.find("</parameter>") {
                        val_body[..val_end].trim().to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            if !tool_name.is_empty() {
                return Some(PlannerDirective {
                    tool: tool_name,
                    target,
                    action: "Execute direct tool call from Architect".into(),
                    rules: "Atomic execution".into(),
                });
            }
        }
    }
    None
}

fn main() {
    let text = r#"Let me start by reading the phase35.md file to understand what Phase 35.2 requires.
<tool_call>
<function=Bash>
<parameter=arguments>
{"command": "cat PLAN/PHASES/phase35.md 2>/dev/null || find / -name phase35.md 2>/dev/null | head -5"}
</parameter>
</function>
</tool_call>"#;
    println!("{:?}", parse_planner_directive(text));
}
