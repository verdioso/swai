#!/bin/bash
sed -i 's/let prompt = format!(/let directive_str = state.planner_directive.as_ref().map(|d| format!("Architect'\''s Atomic Step:\\n- Tool: {}\\n- Target: {}\\n- Action: {}\\n- Constraints: {}", d.tool, d.target, d.action, d.rules)).unwrap_or_else(|| "No specific plan provided.".into());\n\n        let prompt = format!(/' core/src/council/pipeline.rs

sed -i 's/Original prompt:\\n{}\\n\\nDraft to audit:/Original prompt:\\n{}\\n\\n{}\\n\\nDraft to audit:/' core/src/council/pipeline.rs

sed -i 's/state.transcript.input_prompt/state.transcript.input_prompt, directive_str/' core/src/council/pipeline.rs
