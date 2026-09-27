import re

with open('core/src/council/tests.rs', 'r') as f:
    content = f.read()

# Remove all max_tokens: None, anywhere
content = re.sub(r'\s*max_tokens:[^,]+,', '', content)

# Now just add max_tokens: None, right after PipelineStage {
content = content.replace("PipelineStage {", "PipelineStage { max_tokens: None,")

with open('core/src/council/tests.rs', 'w') as f:
    f.write(content)
