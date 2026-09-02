use super::types::ChannelMessage;
use std::sync::{Arc, Mutex};
use swai_core::process_manager::{ModelState, ProcessManager};

/// Spawn a health monitor thread for the given model.
pub fn spawn_health_monitor(
    pm: Arc<Mutex<ProcessManager>>,
    sender: std::sync::mpsc::Sender<ChannelMessage>,
    model_id: String,
) {
    let (health_tx, health_rx) = std::sync::mpsc::channel::<ModelState>();

    let already_running = pm
        .lock()
        .ok()
        .map(|p| p.find_running_model(&model_id).is_some())
        .unwrap_or(false);

    if already_running {
        let port = pm.lock().ok().and_then(|p| p.get_port_for_model(&model_id));

        if let Some(port) = port {
            let monitor = swai_core::health_monitor::HealthMonitor::new(port, 30);
            std::thread::spawn(move || {
                monitor.wait_until_ready_with_updates(health_tx);
            });
        }
    } else {
        let health_pm = Arc::clone(&pm);
        let health_model_id = model_id.clone();
        std::thread::spawn(move || {
            if let Ok(mut pm) = health_pm.lock() {
                let _ = pm.start_model_and_report(&health_model_id, health_tx);
            }
        });
    }

    while let Ok(state) = health_rx.recv() {
        let is_ready = matches!(state, ModelState::Ready);
        let is_err = matches!(state, ModelState::Error(_));

        let _ = sender.send(ChannelMessage::StateUpdate {
            model_id: model_id.clone(),
            state,
        });

        if is_ready || is_err {
            let result = if is_ready {
                Ok(())
            } else {
                Err(
                    swai_core::process_manager::error::ProcessError::HealthCheckFailed(
                        "Startup failed".into(),
                    ),
                )
            };
            let _ = sender.send(ChannelMessage::SwitchCompleted {
                target_id: model_id.clone(),
                result,
            });
        }
    }
}

/// Check for SWAI updates in the background.
pub fn check_for_update_background(github_repo: &str, current_version: &str) {
    let bg_github_repo = github_repo.to_string();
    let bg_current_version = current_version.to_string();

    std::thread::spawn(move || {
        let result =
            crate::update_checker::check_for_updates_blocking(&bg_github_repo, &bg_current_version);

        match result {
            crate::update_checker::UpdateCheckResult::UpdateAvailable { version, .. } => {
                tracing::info!("Background update check: update available v{}", version);
            }
            crate::update_checker::UpdateCheckResult::NoUpdate => {
                tracing::debug!("Background update check: no update available");
            }
            crate::update_checker::UpdateCheckResult::Error(e) => {
                tracing::warn!("Background update check failed: {}", e);
            }
        }
    });
}
