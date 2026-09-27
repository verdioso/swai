import re
with open('app/src/arena/view.rs', 'r') as f:
    c = f.read()

c = re.sub(r'CouncilRole::Synthesizer\s*=>\s*\(.*?\),', '', c)

with open('app/src/arena/view.rs', 'w') as f:
    f.write(c)

