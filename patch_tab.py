import re
with open('app/src/preferences/council_tab.rs', 'r') as f:
    c = f.read()

c = re.sub(r'CouncilRole::Synthesizer\s*=>.*?,\n?', '', c)
c = re.sub(r'3\s*=>\s*CouncilRole::Synthesizer.*?,?\n?', '', c)
c = c.replace('"Planner", "Generator", "Auditor", "Synthesizer"', '"Planner", "Generator", "Auditor"')
c = re.sub(r'swai_core::council::CouncilRole::Synthesizer.*?,', '', c)
c = re.sub(r'top_p: (.*?),\n(\s*)system_prompt: None', r'top_p: \1,\n\2max_tokens: None,\n\2system_prompt: None', c)

with open('app/src/preferences/council_tab.rs', 'w') as f:
    f.write(c)

