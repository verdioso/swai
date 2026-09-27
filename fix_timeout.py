import re

with open('core/src/process_manager/manager.rs', 'r') as f:
    content = f.read()

old_block = """        // Extract the port from config for health monitoring
        if let Some(port) = self
            .config
            .models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.port)
        {
            let monitor = HealthMonitor::new(port, 30);"""

new_block = """        // Extract the port from config for health monitoring
        if let Some(model_cfg) = self
            .config
            .models
            .iter()
            .find(|m| m.id == id)
        {
            let timeout = model_cfg.health_timeout_sec;
            let monitor = HealthMonitor::new(model_cfg.port, timeout);"""

content = content.replace(old_block, new_block)

with open('core/src/process_manager/manager.rs', 'w') as f:
    f.write(content)
