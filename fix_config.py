with open('/home/denisjosifoski/.config/swai/config.toml', 'r') as f:
    content = f.read()

import re
content = re.sub(r'\[council\].*', '', content, flags=re.DOTALL)

council_config = """[council]
mode = "Sequential"
fallback = "Skip"

[[council.stages]]
model_id = "run-ornith-1.5-opt"
role = "Planner"
prompt_template = ""
temperature = 0.699999988079071
top_p = 0.8999999761581421

[[council.stages]]
model_id = "run-qwen25-coder-7b"
role = "Generator"
prompt_template = ""
temperature = 0.699999988079071
top_p = 0.8999999761581421

[[council.stages]]
model_id = "run-ornith-1.5-opt"
role = "Auditor"
prompt_template = ""
temperature = 0.699999988079071
top_p = 0.8999999761581421

[council.role_overrides]
"""

with open('/home/denisjosifoski/.config/swai/config.toml', 'w') as f:
    f.write(content + council_config)
