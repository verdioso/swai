use gio::prelude::*;
use gio::Notification;
use glib::object::Cast;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::window::types::{ChannelMessage, SlotUpdate};
use swai_core::process_manager::ProcessManager;
use swai_core::proxy::ProxyState;

/// Rate-limit auto-restart: minimum 30 seconds between restarts per model.
static LAST_RESTART: AtomicU64 = AtomicU64::new(0);

/// Triggers an automatic restart of a model when its context usage reaches 98%.
pub fn trigger_auto_restart(
    pm: &Arc<Mutex<ProcessManager>>,
    sender: &Sender<ChannelMessage>,
    slot_sender: &Sender<SlotUpdate>,
    proxy_state: &Option<Arc<Mutex<ProxyState>>>,
    model_id: &str,
    enable_notifications: bool,
    n_ctx: usize,
) {
    let now_sec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let last = LAST_RESTART.load(Ordering::Relaxed);
    if now_sec.saturating_sub(last) < 30 {
        return;
    }
    LAST_RESTART.store(now_sec, Ordering::Relaxed);

    // Immediately reset the UI progress bar tokens to 0 so stale 98% metrics don't re-trigger.
    let _ = slot_sender.send(SlotUpdate {
        model_id: model_id.to_string(),
        tokens_used: 0,
        n_ctx,
        predicted_per_second: 0.0,
        prompt_per_second: 0.0,
        prompt_tokens: 0,
        decoded_tokens: 0,
        is_processing: false,
        elapsed_duration_sec: None,
    });

    let bg_model_id = model_id.to_string();
    let bg_pm = Arc::clone(pm);
    let bg_sender = sender.clone();
    let bg_proxy = proxy_state.clone();
    let bg_enable_notifications = enable_notifications;

    std::thread::spawn(move || {
        let _ = bg_sender.send(ChannelMessage::RestartRequested {
            model_id: bg_model_id.clone(),
        });

        let result = {
            let mut pm_lock = match bg_pm.lock() {
                Ok(g) => g,
                Err(_) => return,
            };

            // Stop the model regardless of whether it's primary or secondary.
            let _ = pm_lock.stop_model(&bg_model_id, true);
            std::thread::sleep(std::time::Duration::from_millis(1500));

            pm_lock.start_model(&bg_model_id)
        };

        let is_ok = result.is_ok();

        // The health monitor drives the final state.
        // Only send SwitchCompleted on failure.
        if !is_ok {
            let _ = bg_sender.send(ChannelMessage::SwitchCompleted {
                target_id: bg_model_id.clone(),
                result,
            });
        }

        if is_ok && bg_enable_notifications {
            // Schedule notification on the main (GLib) thread.
            let auto_restart_body = format!("Context full - {} restarted", bg_model_id);
            glib::idle_add_once(move || {
                notify("SWAI", &auto_restart_body);
            });

            if let Some(ref proxy) = bg_proxy {
                let mut ps = proxy.lock().unwrap_or_else(|e| {
                    tracing::error!("auto-restart: proxy state lock poisoned");
                    e.into_inner()
                });
                let running = bg_pm
                    .lock()
                    .ok()
                    .map(|pm| pm.running_model_ports())
                    .unwrap_or_default();
                ps.sync_models(running);
            }
        }
    });
}

/// Send a native desktop toast notification across GNOME, KDE, and all Linux desktops.
pub fn notify(title: &str, body: &str) {
    // Try notify-send first (direct DBus notification to org.freedesktop.Notifications for GNOME/KDE).
    let res = std::process::Command::new("notify-send")
        .arg("-i")
        .arg("swai")
        .arg("-a")
        .arg("SWAI")
        .arg(title)
        .arg(body)
        .status();

    if res.is_ok() && res.unwrap().success() {
        return;
    }

    // Fallback to GIO Application notification if notify-send is unavailable.
    let notification = Notification::new("");
    notification.set_title(title);
    notification.set_body(Some(body));
    notification.set_icon(&gio::ThemedIcon::new("swai"));

    let app = gio::Application::default().and_then(|app| app.downcast::<adw::Application>().ok());

    if let Some(app) = app {
        app.send_notification(Some("swai-notification"), &notification);
    }
}
