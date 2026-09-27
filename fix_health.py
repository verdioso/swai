import re

with open('app/src/window/health.rs', 'r') as f:
    content = f.read()

# Replace hardcoded 30 with dynamic read
content = re.sub(
    r'let monitor = swai_core::health_monitor::HealthMonitor::new\(port, 30\);',
    r'''let timeout = pm.lock().ok().and_then(|p| p.config().models.iter().find(|m| m.id == model_id).map(|m| m.health_timeout_sec)).unwrap_or(90);
            let monitor = swai_core::health_monitor::HealthMonitor::new(port, timeout);''',
    content
)

with open('app/src/window/health.rs', 'w') as f:
    f.write(content)

