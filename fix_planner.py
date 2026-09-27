import re

with open('core/src/council/planner.rs', 'r') as f:
    content = f.read()

content = content.replace('|| t == "run_command"', '|| t == "list_dir"\n        || t == "ls"\n        || t == "run_command"')

with open('core/src/council/planner.rs', 'w') as f:
    f.write(content)
