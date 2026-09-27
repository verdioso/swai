import re

def fix_file(filepath):
    with open(filepath, 'r') as f:
        content = f.read()

    # Remove all duplicated max_tokens
    while "max_tokens: None, max_tokens: None" in content:
        content = content.replace("max_tokens: None, max_tokens: None", "max_tokens: None")

    # Add missing max_tokens before system_prompt
    content = re.sub(r'top_p: (.*?),\n(\s*)system_prompt: None', r'top_p: \1,\n\2max_tokens: None,\n\2system_prompt: None', content)
    
    with open(filepath, 'w') as f:
        f.write(content)

fix_file('core/src/proxy/tool_calling.rs')
fix_file('core/src/council/tests_pipeline.rs')
fix_file('core/src/council/tests_streaming.rs')
fix_file('core/src/council/tests.rs')
fix_file('core/src/proxy/council_route.rs')
