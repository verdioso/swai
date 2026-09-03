use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use swai_core::config::Config;
use swai_core::process_manager::ProcessManager;
use swai_core::proxy::ProxyState;

use super::health::spawn_health_monitor;
use super::types::ChannelMessage;
use crate::logs_panel::LogViewerWindow;
use crate::model_card::{CardState, ModelCard};

pub fn wire_card_handlers(
    card: &mut ModelCard,
    cards: &Rc<RefCell<Vec<ModelCard>>>,
    process_manager: &Arc<Mutex<ProcessManager>>,
    proxy_state: &Rc<Option<Arc<Mutex<ProxyState>>>>,
    sender: &Sender<ChannelMessage>,
    _keep_alive_ref: &Rc<RefCell<Option<Arc<AtomicBool>>>>,
    log_viewer: &Rc<RefCell<Option<LogViewerWindow>>>,
    config: &Config,
) {
    let model_id_toggle = card.config().id.clone();
    let model_id_restart = card.config().id.clone();
    let card_ka = Arc::new(AtomicBool::new(false));

    // ── Toggle handler ───────────────────────────────────
    {
        let card_ka_toggle = Arc::clone(&card_ka);
        let cards_inner = Rc::clone(cards);
        let sender_inner = sender.clone();
        let pm_ref = Arc::clone(process_manager);
        let proxy_for_toggle = Rc::clone(proxy_state);
        let sender_health_for_toggle = sender.clone();
        let model_id = model_id_toggle;

        card.set_toggle_handler(move |on| {
            let proxy_for_handler = proxy_for_toggle.as_ref().as_ref().map(Arc::clone);
            let cards_inner = cards_inner.borrow();
            if on {
                let target_card = match cards_inner.iter().find(|c| c.config().id == model_id) {
                    Some(c) => c,
                    None => return,
                };
                if target_card.state().is_transitioning() {
                    return;
                }
                target_card.set_starting();

                let max_concurrent = pm_ref.lock().map(|pm| pm.max_concurrent_models()).unwrap_or(1);
                let active_count = cards_inner.iter().filter(|c| {
                    matches!(c.state(), CardState::Starting | CardState::Loading | CardState::Ready)
                }).count();

                if active_count >= max_concurrent {
                    for c in cards_inner.iter() {
                        if matches!(c.state(), CardState::Stopped | CardState::Error(_)) {
                            c.disable_toggle();
                        }
                    }
                }

                card_ka_toggle.store(true, Ordering::SeqCst);
                let bg_model_id = model_id.clone();
                let pm_thread = Arc::clone(&pm_ref);
                let sender_thread = sender_inner.clone();
                let ka_thread = Arc::clone(&card_ka_toggle);
                let sender_health_for_thread = sender_health_for_toggle.clone();

                std::thread::spawn(move || {
                    let result = {
                        let mut pm_lock = match pm_thread.lock() {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                        if pm_lock.get_primary_model_id() == Some(bg_model_id.as_str()) {
                            return;
                        }
                        let running_count = pm_lock.running_count();
                        let max_concurrent = pm_lock.max_concurrent_models();

                        if running_count < max_concurrent {
                            pm_lock.start_model(&bg_model_id)
                        } else {
                            let primary_id =
                                pm_lock.get_primary_model_id().unwrap_or("").to_string();
                            if !primary_id.is_empty() {
                                pm_lock.switch_model(&primary_id, &bg_model_id)
                            } else {
                                pm_lock.start_model(&bg_model_id)
                            }
                        }
                    };

                    let is_ok = result.is_ok();
                    if is_ok {
                        let pm_health = Arc::clone(&pm_thread);
                        let sender_health = sender_health_for_thread.clone();
                        let model_id_health = bg_model_id.clone();
                        spawn_health_monitor(pm_health, sender_health, model_id_health);
                    } else {
                        let _ = sender_thread.send(ChannelMessage::SwitchCompleted {
                            target_id: bg_model_id,
                            result,
                        });
                    }

                    if is_ok {
                        if let Some(ref proxy) = proxy_for_handler {
                            let running = pm_thread
                                .lock()
                                .ok()
                                .map(|pm| pm.running_model_ports())
                                .unwrap_or_default();
                            proxy
                                .lock()
                                .unwrap_or_else(|e| {
                                    tracing::error!("proxy state lock poisoned");
                                    e.into_inner()
                                })
                                .sync_models(running);
                        }
                    }

                    if is_ok {
                        while ka_thread.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                });
            } else {
                card_ka_toggle.store(false, Ordering::SeqCst);

                let pm_thread = Arc::clone(&pm_ref);
                let sender_thread = sender_inner.clone();
                let proxy_thread = proxy_for_toggle.as_ref().as_ref().map(Arc::clone);
                let bg_model_id = model_id.clone();

                std::thread::spawn(move || {
                    let mut pm_lock = match pm_thread.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    let result = pm_lock.stop_model(&bg_model_id, true);
                    let is_ok = result.is_ok();
                    let _ = sender_thread.send(ChannelMessage::StopCompleted {
                        running_id: bg_model_id,
                        result,
                    });

                    if is_ok {
                        if let Some(ref proxy) = proxy_thread {
                            let running = pm_lock.running_model_ports();
                            proxy
                                .lock()
                                .unwrap_or_else(|e| {
                                    tracing::error!("proxy state lock poisoned");
                                    e.into_inner()
                                })
                                .sync_models(running);
                        }
                    }
                });
            }
        });
    }

    // ── Restart button handler ───────────────────────────
    {
        let card_ka_restart = Arc::clone(&card_ka);
        let cards_restart = Rc::clone(cards);
        let sender_restart = sender.clone();
        let pm_restart = Arc::clone(process_manager);
        let proxy_for_restart = Rc::clone(proxy_state);
        let model_id = model_id_restart;

        card.restart_button.connect_clicked(move |_| {
            let proxy_thread = proxy_for_restart.as_ref().as_ref().map(Arc::clone);
            let cards_inner = cards_restart.borrow();
            let target = match cards_inner.iter().find(|c| c.config().id == model_id) {
                Some(c) => c,
                None => return,
            };

            if target.state().is_transitioning() || target.restart_requested() {
                return;
            }

            target.disable_restart();

            card_ka_restart.store(true, Ordering::SeqCst);
            let bg_model_id = model_id.clone();
            let pm_thread = Arc::clone(&pm_restart);
            let sender_thread = sender_restart.clone();
            let ka_thread = Arc::clone(&card_ka_restart);
            let proxy_restart_thread = proxy_thread.as_ref().map(Arc::clone);

            std::thread::spawn(move || {
                let _ = sender_thread.send(ChannelMessage::RestartRequested {
                    model_id: bg_model_id.clone(),
                });

                let result = {
                    let mut pm_lock = match pm_thread.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };

                    // Stop the model regardless of whether it's primary or secondary.
                    let _ = pm_lock.stop_model(&bg_model_id, true);
                    std::thread::sleep(std::time::Duration::from_millis(500));

                    pm_lock.start_model(&bg_model_id)
                };

                let is_ok = result.is_ok();
                if is_ok {
                    let pm_health = Arc::clone(&pm_thread);
                    let sender_health = sender_thread.clone();
                    let model_id_health = bg_model_id.clone();
                    spawn_health_monitor(pm_health, sender_health, model_id_health);
                } else {
                    let _ = sender_thread.send(ChannelMessage::SwitchCompleted {
                        target_id: bg_model_id,
                        result,
                    });
                }

                if is_ok {
                    if let Some(ref proxy) = proxy_restart_thread {
                        let running = pm_thread
                            .lock()
                            .ok()
                            .map(|pm| pm.running_model_ports())
                            .unwrap_or_default();
                        proxy
                            .lock()
                            .unwrap_or_else(|e| {
                                tracing::error!("proxy state lock poisoned");
                                e.into_inner()
                            })
                            .sync_models(running);
                    }
                }

                if is_ok {
                    while ka_thread.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            });
        });
    }

    // ── Logs button handler ────────────────────────────────
    {
        let card_config = card.config().clone();
        let log_viewer_ref = Rc::clone(log_viewer);
        let log_dir = config.log_dir();
        let all_models = config.models.clone();

        card.set_logs_handler(move || {
            let viewer = LogViewerWindow::new(
                &card_config.name,
                &card_config.script_path,
                &log_dir,
                &card_config.id,
                &all_models,
            );
            viewer.present();
            *log_viewer_ref.borrow_mut() = Some(viewer);
        });
    }
}
