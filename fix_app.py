import re
import os

def fix_file(filepath):
    if not os.path.exists(filepath): return
    with open(filepath, 'r') as f:
        content = f.read()

    # CouncilRole::Synthesizer => 3, or similar
    content = re.sub(r'CouncilRole::Synthesizer\s*=>.*?,\n?', '', content)
    
    # 3 => CouncilRole::Synthesizer,
    content = re.sub(r'3\s*=>\s*CouncilRole::Synthesizer.*?,?\n?', '', content)
    
    # "Synthesizer" string in array
    content = content.replace(', "Synthesizer"', '')
    content = content.replace('"Synthesizer", ', '')
    content = content.replace('"Synthesizer"', '')

    # PipelineStage missing max_tokens
    content = re.sub(r'top_p: (.*?),\n(\s*)system_prompt: None', r'top_p: \1,\n\2max_tokens: None,\n\2system_prompt: None', content)

    with open(filepath, 'w') as f:
        f.write(content)

fix_file('app/src/preferences/council_tab.rs')
fix_file('app/src/arena/view.rs')

# Let's see what else failed
# app/src/preferences/council_tab.rs:232:31 `roles` not found in this scope
# Ah, I deleted the declaration of roles entirely with my sed command `/"Synthesizer"/d`.
# I will just restore council_tab.rs and patch it correctly.
