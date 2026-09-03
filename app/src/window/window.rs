use adw::prelude::*;
use adw::ApplicationWindow;
use gtk::{Application, Box as GtkBox, MessageDialog, MessageType, Orientation, ResponseType};
use gtk4 as gtk;
use ksni::blocking::Handle;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use swai_core::config::Config;
use swai_core::process_manager::ProcessManager;
use swai_core::proxy::ProxyState;
use swai_core::reconciler::Reconciler;

use crate::logs_panel::LogViewerWindow;
use crate::model_card::ModelCard;
use crate::tray::{TrayAction, WindowAction};

use super::adoption::{build_adoption_banner, restore_running_models, show_adopt_model_dialog};
use super::card_wiring::wire_card_handlers;
use super::footer::{build_cards_container, build_footer_bar, reorder_card_container};
use super::header::{build_header_bar, wire_actions};
use super::health::check_for_update_background;
use super::poller::spawn_context_poller;
use super::styles::load_css;
use super::timeout::{attach_timeout_handler, TimeoutContext};
use super::types::{ChannelMessage, ImportMessage, SlotUpdate};

/// The main application window.
#[allow(dead_code)]
pub struct MainWindow {
    widget: ApplicationWindow,
    cards: Rc<RefCell<Vec<ModelCard>>>,
    current_keep_alive: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    config: Config,
    config_path: std::path::PathBuf,
    proxy_state: Option<Arc<Mutex<ProxyState>>>,
    log_viewer: Rc<RefCell<Option<LogViewerWindow>>>,
    close_requested: Rc<RefCell<bool>>,
    process_manager: Arc<Mutex<ProcessManager>>,
    tray_handle: Option<Handle<crate::tray::SwaiTray>>,
    tray_host_available: bool,
    import_sender: std::sync::mpsc::Sender<ImportMessage>,
    footer_proxy_label: gtk::Label,
    footer_model_label: gtk::Label,
    unmanaged_banner: Option<adw::Banner>,
    pub bottom_deck: super::bottom_deck::BottomDeck,
    /// Cached Debate Arena window handle for single-instance window reuse.
    pub debate_arena: std::rc::Rc<std::cell::RefCell<Option<crate::arena::ArenaWindow>>>,
}

impl MainWindow {
    pub fn new(
        app: &Application,
        config: Config,
        proxy_state: Option<Arc<Mutex<ProxyState>>>,
    ) -> Self {
        gtk::Window::set_default_icon_name("swai");
        load_css();

        let widget = ApplicationWindow::builder()
            .application(app)
            .title("SWAI")
            .icon_name("swai")
            .default_width(640)
            .default_height(520)
            .build();

        let header_bar = build_header_bar(app);
        let main_vbox = GtkBox::new(Orientation::Vertical, 0);
        main_vbox.append(&header_bar);

        let card_box = Rc::new(RefCell::new(GtkBox::new(Orientation::Vertical, 12)));
        {
            let bx = card_box.borrow_mut();
            bx.set_margin_start(16);
            bx.set_margin_end(16);
            bx.set_margin_top(16);
            bx.set_margin_bottom(16);
        }

        let cards = Rc::new(RefCell::new(
            config
                .models
                .iter()
                .map(|m| {
                    let card = ModelCard::new(m);
                    card_box.borrow_mut().append(&card.widget);
                    card
                })
                .collect::<Vec<_>>(),
        ));

        reorder_card_container(&cards.borrow());
        let cards_scroll = build_cards_container(&card_box.borrow());
        main_vbox.append(&cards_scroll);

        let bottom_deck = super::bottom_deck::BottomDeck::new(config.proxy_port(), &config);
        main_vbox.append(&bottom_deck.container);

        let (footer_bar, footer_proxy_label, footer_model_label) =
            build_footer_bar(config.proxy_port());
        let footer_model_label_clone = footer_model_label.clone();
        main_vbox.append(&footer_bar);

        widget.set_content(Some(&main_vbox));

        let config_path = Config::resolve_path()
            .unwrap_or_else(|| std::path::PathBuf::from("/nonexistent/config.toml"));

        let pm = Arc::new(Mutex::new(ProcessManager::new(config.clone())));

        let reconciler = Reconciler::new(config.clone());
        let unmanaged_servers = reconciler.probe_unmanaged_servers();
        let (unmanaged_banner, adopt_port, adopt_model_name) = if !unmanaged_servers.is_empty() {
            let first = &unmanaged_servers[0];
            let banner = build_adoption_banner(
                &format!(
                    "Unmanaged local model detected on port {} ({})",
                    first.port, first.model_name
                ),
                first.port,
                first.model_name.clone(),
                &widget,
            );
            (
                Some(banner),
                Some(first.port),
                Some(first.model_name.clone()),
            )
        } else {
            (None, None, None)
        };

        if let Some(ref banner) = unmanaged_banner {
            main_vbox.insert_before(banner, Some(&cards_scroll));
        }

        let current_version = env!("CARGO_PKG_VERSION").to_string();
        check_for_update_background("verdioso/swai", &current_version);

        let current_keep_alive = Rc::new(RefCell::new(None::<Arc<AtomicBool>>));

        restore_running_models(&pm, &cards);

        let close_requested = Rc::new(RefCell::new(false));
        let tray_host_available = crate::tray::tray_host_available();
        tracing::info!(
            "tray host available: {}",
            if tray_host_available { "yes" } else { "no" }
        );

        let pm_for_struct = Arc::clone(&pm);
        let pm_close = Arc::clone(&pm);
        let app_close = app.clone();

        let close_requested_clone = Rc::clone(&close_requested);
        let widget_hide = widget.clone();
        let widget_close_req = widget.clone();
        widget_close_req.connect_close_request(move |win| {
            if *close_requested_clone.borrow() {
                return glib::Propagation::Stop;
            }

            if let Some(app) = win.application() {
                let win_ptr = win.upcast_ref::<gtk::Window>().as_ptr();
                for w in app.windows() {
                    if w.as_ptr() != win_ptr {
                        w.destroy();
                    }
                }
            }

            *close_requested_clone.borrow_mut() = true;

            let tray_available = tray_host_available;

            let (dialog_msg, has_minimize) = if tray_available {
                ("Quit SWAI entirely, or minimize to tray?", true)
            } else {
                (
                    "Quit SWAI entirely?\n\n\
                     Minimize to tray isn't available - no system tray was \
                     detected on this desktop.",
                    false,
                )
            };

            let dialog = MessageDialog::new(
                Some(win),
                gtk::DialogFlags::MODAL,
                MessageType::Question,
                gtk::ButtonsType::None,
                dialog_msg,
            );
            dialog.set_title(Some("SWAI"));

            dialog.add_button("Quit", ResponseType::Close);
            if has_minimize {
                dialog.add_button("Minimize to Tray", ResponseType::Apply);
            }

            let close_clone = Rc::clone(&close_requested_clone);
            let pm_quit = Arc::clone(&pm_close);
            let app_quit = app_close.clone();
            let widget_hide_ref = widget_hide.clone();
            dialog.connect_response(move |d, response| {
                match response {
                    ResponseType::Close => {
                        let _ = pm_quit.lock().unwrap_or_else(|e| {
                            tracing::error!("close dialog: process manager lock poisoned, continuing with shutdown");
                            e.into_inner()
                        }).stop_all(true);
                        tracing::info!("user chose to quit from close dialog");
                        for w in app_quit.windows() {
                            w.destroy();
                        }
                        app_quit.quit();
                    }
                    ResponseType::Apply => {
                        tracing::info!("user chose to minimize to tray");
                        widget_hide_ref.hide();
                    }
                    _ => {}
                }
                *close_clone.borrow_mut() = false;
                d.destroy();
            });

            dialog.present();
            glib::Propagation::Stop
        });

        // Cached Debate Arena window handle for single-instance window reuse.
        let debate_arena: std::rc::Rc<std::cell::RefCell<Option<crate::arena::ArenaWindow>>> =
            Rc::new(RefCell::new(None));

        {
            let pm_wa = Arc::clone(&pm);
            let app_wa = app.clone();
            let on_quit: Arc<dyn Fn()> = Arc::new(move || {
                let _ = pm_wa
                    .lock()
                    .unwrap_or_else(|e| {
                        tracing::error!(
                            "quit: process manager lock poisoned, continuing with shutdown"
                        );
                        e.into_inner()
                    })
                    .stop_all(true);
                for w in app_wa.windows() {
                    w.destroy();
                }
                app_wa.quit();
            });
            wire_actions(
                &widget,
                app,
                on_quit,
                Arc::clone(&pm),
                proxy_state.clone(),
                Rc::clone(&debate_arena),
            );

            crate::window::arena_wiring::attach_arena_shortcut(
                &widget,
                &debate_arena,
                proxy_state.clone(),
            );
        }

        let (sender, receiver) = std::sync::mpsc::channel::<ChannelMessage>();
        let sender_poll = sender.clone();
        let (slot_sender, slot_receiver) = std::sync::mpsc::channel::<SlotUpdate>();
        let (window_sender, window_receiver) = std::sync::mpsc::channel::<WindowAction>();
        let (tray_sender, tray_receiver) = std::sync::mpsc::channel::<TrayAction>();
        let (quit_sender, quit_receiver) = std::sync::mpsc::channel::<()>();
        let (import_sender, import_receiver) = std::sync::mpsc::channel::<ImportMessage>();

        {
            let import_sender_for_manage = import_sender.clone();
            let pm_for_manage = Arc::clone(&pm);
            let manage_models_action = gio::SimpleAction::new("manage_models", None);
            manage_models_action.connect_activate(glib::clone!(
                #[weak]
                widget,
                move |_, _| {
                    crate::window::dialogs::show_manage_models_dialog(
                        &widget,
                        &import_sender_for_manage,
                        &pm_for_manage,
                    );
                }
            ));
            widget.add_action(&manage_models_action);
        }

        {
            let import_sender_for_action = import_sender.clone();
            let add_model_action = gio::SimpleAction::new("add_model", None);
            add_model_action.connect_activate(glib::clone!(
                #[weak]
                widget,
                move |_, _| {
                    crate::window::dialogs::show_add_model_dialog(
                        &widget,
                        &import_sender_for_action,
                    );
                }
            ));
            widget.add_action(&add_model_action);
        }

        let tray_handle_timeout: Rc<RefCell<Option<Handle<crate::tray::SwaiTray>>>> =
            Rc::new(RefCell::new(None));
        let tray_handle_for_struct = Rc::clone(&tray_handle_timeout);
        let current_keep_alive_post_closure = Rc::clone(&current_keep_alive);
        let sender_for_post_closure = sender.clone();
        let log_viewer = Rc::new(RefCell::new(None::<LogViewerWindow>));

        attach_timeout_handler(TimeoutContext {
            cards: Rc::clone(&cards),
            card_box: Rc::clone(&card_box),
            pm: Arc::clone(&pm),
            widget: widget.clone(),
            tray_handle: tray_handle_timeout,
            current_keep_alive: Rc::clone(&current_keep_alive),
            proxy_state: proxy_state.clone(),
            log_viewer: Rc::clone(&log_viewer),
            config: config.clone(),
            footer_model_label: footer_model_label_clone,
            sender: sender.clone(),
            receiver,
            slot_receiver,
            window_receiver,
            tray_receiver,
            quit_receiver,
            import_receiver,
            bottom_deck: bottom_deck.clone(),
            debate_arena: Some(Rc::clone(&debate_arena)),
        });

        let pm_poll = Arc::clone(&pm);
        let auto_restart_enabled = config.auto_restart_on_context_full();
        let enable_notifications = config.enable_notifications();
        spawn_context_poller(
            pm_poll,
            sender_poll,
            slot_sender,
            auto_restart_enabled,
            proxy_state.clone(),
            enable_notifications,
        );

        {
            let pm_clone = Arc::clone(&pm);
            let keep_alive_ref = Rc::clone(&current_keep_alive_post_closure);
            let sender_ref = sender_for_post_closure.clone();
            let proxy_state_for_bg = Rc::new(proxy_state.clone());
            let cards_for_toggle = Rc::clone(&cards);
            let pm_for_toggle = Arc::clone(&pm_clone);

            let mut cards_borrow = cards.borrow_mut();
            for card in cards_borrow.iter_mut() {
                wire_card_handlers(
                    card,
                    &cards_for_toggle,
                    &pm_for_toggle,
                    &proxy_state_for_bg,
                    &sender_ref,
                    &keep_alive_ref,
                    &log_viewer,
                    &config,
                );

                // Option C: Click card to inspect telemetry in bottom deck
                let m_id = card.config().id.clone();
                let deck_click = bottom_deck.clone();
                let gesture = gtk::GestureClick::new();
                gesture.connect_released(move |_, _, _, _| {
                    deck_click.select_model(&m_id);
                });
                card.widget.add_controller(gesture);
            }
        }

        let tray_handle = crate::tray::create_tray(
            config.clone(),
            Arc::clone(&pm),
            window_sender,
            tray_sender,
            quit_sender,
        );
        if tray_handle.is_some() {
            tracing::info!("system tray icon created");
        }

        *tray_handle_for_struct.borrow_mut() = tray_handle;

        if let (Some(banner), Some(port), Some(name)) =
            (unmanaged_banner.as_ref(), adopt_port, adopt_model_name)
        {
            let parent = widget.clone();
            let sender = import_sender.clone();
            banner.connect_button_clicked(move |_| {
                show_adopt_model_dialog(&parent, &sender, port, name.clone());
            });
        }

        Self {
            widget,
            cards,
            current_keep_alive,
            config,
            config_path,
            proxy_state,
            log_viewer: Rc::clone(&log_viewer),
            close_requested,
            process_manager: pm_for_struct,
            tray_handle: None,
            tray_host_available,
            import_sender,
            footer_proxy_label,
            footer_model_label,
            unmanaged_banner,
            bottom_deck,
            debate_arena: Rc::clone(&debate_arena),
        }
    }

    pub fn show(&self) {
        self.widget.show();
    }

    #[allow(dead_code)]
    pub fn quit(&self) {
        tracing::info!("quitting SWAI");
        let _ = self
            .process_manager
            .lock()
            .unwrap_or_else(|e| {
                tracing::error!("quit: process manager lock poisoned, continuing with shutdown");
                e.into_inner()
            })
            .stop_all(true);
        self.widget.close();
    }
}
