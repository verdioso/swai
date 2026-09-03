//! SWAI (SWitch AI) — GTK4 + Libadwaita native desktop shell.

use std::sync::Arc;

use gtk::prelude::*;
use gtk4 as gtk;

mod arena;
mod import_wizard;
mod logs_panel;
mod manage_dialog;
mod menu;
mod model_card;
mod preferences;
mod tray;
mod update_checker;
mod update_installer;
mod window;

fn try_handle_cli_args() -> Option<glib::ExitCode> {
    let args: Vec<String> = std::env::args().collect(); // nosemgrep: rust.lang.security.args.args
    if args.len() < 2 {
        return None; // Launch standard GUI
    }

    let command = args[1].as_str();
    let action_req = match command {
        "status" => swai_core::ipc::ActionRequest {
            action: "status".to_string(),
            data: None,
        },
        "list" => swai_core::ipc::ActionRequest {
            action: "list".to_string(),
            data: None,
        },
        "stop" => swai_core::ipc::ActionRequest {
            action: "stop".to_string(),
            data: None,
        },
        "start" => {
            if args.len() < 3 {
                eprintln!(
                    "Error: 'swai start' requires a model ID (e.g. swai start ornith-1.0-35b)"
                );
                return Some(glib::ExitCode::FAILURE);
            }
            swai_core::ipc::ActionRequest {
                action: "start".to_string(),
                data: Some(serde_json::json!({ "model_id": args[2] })),
            }
        }
        "switch" => {
            if args.len() < 3 {
                eprintln!("Error: 'swai switch' requires a model ID (e.g. swai switch qwen-3.6)");
                return Some(glib::ExitCode::FAILURE);
            }
            swai_core::ipc::ActionRequest {
                action: "switch".to_string(),
                data: Some(serde_json::json!({ "model_id": args[2] })),
            }
        }
        "--help" | "-h" | "help" => {
            println!("SWAI (SWitch AI) CLI Interface");
            println!("Usage:");
            println!("  swai                Launch SWAI GTK Desktop UI");
            println!("  swai status         Show active model status over IPC");
            println!("  swai list           List configured models over IPC");
            println!("  swai start <id>     Start a local model over IPC");
            println!("  swai stop           Stop active model over IPC");
            println!("  swai switch <id>    Switch active model over IPC");
            println!("                        (use 'next' or 'prev' to cycle)");
            return Some(glib::ExitCode::SUCCESS);
        }
        _ => return None, // Unknown argument, let GTK handle or launch GUI
    };

    match swai_core::ipc::ipc_send(&action_req) {
        Ok(resp) => {
            println!("{}", resp.message);
            if let Some(data) = resp.data {
                if let Ok(pretty) = serde_json::to_string_pretty(&data) {
                    println!("{}", pretty);
                }
            }
            Some(glib::ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("SWAI IPC Error: {}", e);
            Some(glib::ExitCode::FAILURE)
        }
    }
}

fn main() -> glib::ExitCode {
    if let Some(exit_code) = try_handle_cli_args() {
        return exit_code;
    }
    glib::log_set_handler(
        Some("Adwaita"),
        glib::LogLevels::LEVEL_WARNING,
        false,
        false,
        |_, _, _| {},
    );

    adw::init().expect("Failed to initialize Libadwaita");

    let style_manager = adw::StyleManager::default();
    style_manager.set_color_scheme(adw::ColorScheme::PreferDark);

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let _single_instance_guard = match swai_core::single_instance::SingleInstanceGuard::try_acquire(
    ) {
        Ok(guard) => guard,
        Err(_) => {
            let dialog = gtk::MessageDialog::new(
                None::<&gtk::Window>,
                gtk::DialogFlags::MODAL,
                gtk::MessageType::Warning,
                gtk::ButtonsType::Close,
                "Another instance of SWAI is already running.\n\nPlease check your system tray or desktop taskbar.",
            );
            dialog.set_title(Some("SWAI — Already Running"));

            dialog.connect_response(|d, _| {
                d.destroy();
            });
            dialog.present();

            let main_context = glib::MainContext::default();
            while dialog.is_visible() {
                main_context.iteration(true);
            }
            return glib::ExitCode::FAILURE;
        }
    };

    let app = gtk::Application::builder()
        .application_id("com.swai.app")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    // Ctrl+Shift+A / Ctrl+D accelerator for the View → Debate Arena menu action.
    app.set_accels_for_action("win.debate_arena", &["<Ctrl><Shift>a", "<Ctrl>d"]);

    app.connect_activate(|app| {
        gtk::Window::set_interactive_debugging(false);

        let config = match swai_core::config::Config::load() {
            Ok(cfg) => cfg,
            Err(_) => {
                let default_dir = if let Ok(home) = std::env::var("HOME") {
                    std::path::PathBuf::from(home).join(".config").join("swai")
                } else {
                    std::path::PathBuf::from(".config/swai")
                };
                let default_path = default_dir.join("config.toml");
                if !default_path.exists() {
                    let _ = std::fs::create_dir_all(&default_dir);
                    let initial_toml = r#"# SWAI configuration file
schema_version = 1

[global]
proxy_port = 9080
auto_restart_on_context_full = true
"#;
                    let _ = std::fs::write(&default_path, initial_toml);
                }

                match swai_core::config::Config::load() {
                    Ok(cfg) => cfg,
                    Err(err) => {
                        let dialog = gtk::MessageDialog::new(
                            None::<&gtk::Window>,
                            gtk::DialogFlags::MODAL,
                            gtk::MessageType::Error,
                            gtk::ButtonsType::Close,
                            format!("Failed to load config:\n\n{}", err),
                        );
                        dialog.set_title(Some("SWAI — Config Error"));

                        let app_clone = app.clone();
                        dialog.connect_response(move |d, _| {
                            d.destroy();
                            app_clone.quit();
                        });
                        dialog.present();
                        return;
                    }
                }
            }
        };

        let mut proxy_state = swai_core::proxy::ProxyState::new();
        // Sync the enable-checkpointing preference from config into proxy state
        // so the proxy thread can read it on every request without touching the
        // config file directly.
        proxy_state.enable_checkpointing = config.enable_checkpointing();
        proxy_state.enable_council = config.enable_council();
        proxy_state.compaction_threshold_pct = config.compaction_threshold_pct();
        let proxy_state = Arc::new(std::sync::Mutex::new(proxy_state));

        let proxy_server =
            match swai_core::proxy::ProxyServer::new(config.proxy_port(), Arc::clone(&proxy_state))
            {
                Ok(server) => Some(server),
                Err(e) => {
                    tracing::warn!("failed to start reverse proxy: {}", e);
                    None
                }
            };

        let main_window = window::MainWindow::new(app, config.clone(), Some(proxy_state.clone()));
        main_window.show();

        use std::rc::Rc;
        let main_window_rc = Rc::new(main_window);
        let proxy_server_rc = Rc::new(proxy_server);

        // Hook SIGINT (Ctrl+C) from terminal
        let main_window_for_sigint = Rc::clone(&main_window_rc);
        let app_for_sigint = app.clone();
        glib::unix_signal_add_local(2, move || {
            tracing::info!("Received SIGINT (Ctrl+C) — stopping all active models");
            main_window_for_sigint.quit();
            app_for_sigint.quit();
            glib::ControlFlow::Break
        });

        // Hook SIGTERM
        let main_window_for_sigterm = Rc::clone(&main_window_rc);
        let app_for_sigterm = app.clone();
        glib::unix_signal_add_local(15, move || {
            tracing::info!("Received SIGTERM — stopping all active models");
            main_window_for_sigterm.quit();
            app_for_sigterm.quit();
            glib::ControlFlow::Break
        });

        let main_window_for_shutdown = Rc::clone(&main_window_rc);
        app.connect_shutdown(move |_| {
            main_window_for_shutdown.quit();
            if let Some(ref _server) = *proxy_server_rc {
                let _server_rc = Rc::clone(&proxy_server_rc);
                drop(_server_rc);
            }
        });
    });

    app.run()
}
