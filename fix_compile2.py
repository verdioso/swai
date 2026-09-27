import re

def fix_file(filepath):
    with open(filepath, 'r') as f:
        content = f.read()

    # Find all PipelineStage { ... } and add max_tokens: None if missing
    def repl(m):
        block = m.group(0)
        if "max_tokens:" not in block:
            return block.replace("system_prompt:", "max_tokens: None,\n                        system_prompt:")
        return block
    
    content = re.sub(r'PipelineStage\s*\{[^}]*\}', repl, content)
    
    with open(filepath, 'w') as f:
        f.write(content)

fix_file('core/src/council/executor.rs')
fix_file('core/src/council/tests.rs')
fix_file('core/src/proxy/tool_calling.rs')
