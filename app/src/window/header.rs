use adw::ApplicationWindow;
use gtk::gio::{Menu, SimpleAction};
use gtk::prelude::*;
use gtk::{Application, Button, HeaderBar};
use gtk4 as gtk;
use std::sync::{Arc, Mutex};

use swai_core::process_manager::ProcessManager;
use swai_core::proxy::ProxyState;

use super::dialogs::{show_about_dialog, show_check_updates_dialog, show_preferences_dialog};

pub fn wire_actions(
    window: &ApplicationWindow,
    _app: &Application,
    on_quit: Arc<dyn Fn()>,
    process_manager: Arc<Mutex<ProcessManager>>,
    proxy_state: Option<Arc<Mutex<ProxyState>>>,
    debate_arena_cache: std::rc::Rc<std::cell::RefCell<Option<crate::arena::ArenaWindow>>>,
) {
    let quit_action = SimpleAction::new("quit", None);
    quit_action.connect_activate(move |_, _| {
        on_quit();
    });
    window.add_action(&quit_action);

    let refresh_action = SimpleAction::new("refresh", None);
    refresh_action.connect_activate(|_, _| {
        tracing::info!("Refresh requested (stub)");
    });
    window.add_action(&refresh_action);

    let about_action = SimpleAction::new("about", None);
    about_action.connect_activate(glib::clone!(
        #[weak]
        window,
        move |_, _| {
            show_about_dialog(&window);
        }
    ));
    window.add_action(&about_action);

    let check_updates_action = SimpleAction::new("check_updates", None);
    check_updates_action.connect_activate(glib::clone!(
        #[weak]
        window,
        move |_, _| {
            show_check_updates_dialog(&window);
        }
    ));
    window.add_action(&check_updates_action);

    let github_action = SimpleAction::new("github", None);
    github_action.connect_activate(|_, _| {
        let _ = gtk::gio::AppInfo::launch_default_for_uri(
            "https://github.com/verdioso/swai",
            None::<&gtk::gio::AppLaunchContext>,
        );
    });
    window.add_action(&github_action);

    let pm_prefs = Arc::clone(&process_manager);
    let ps_prefs = proxy_state.clone();
    let preferences_action = SimpleAction::new("preferences", None);
    preferences_action.connect_activate(glib::clone!(
        #[weak]
        window,
        move |_, _| {
            if let Some(ref ps) = ps_prefs {
                show_preferences_dialog(&window, &pm_prefs, ps);
            }
        }
    ));
    window.add_action(&preferences_action);

    let toggle_logs_action = SimpleAction::new("toggle_logs", None);
    window.add_action(&toggle_logs_action);

    let debate_arena_action = SimpleAction::new("debate_arena", None);
    debate_arena_action.connect_activate(move |_, _| {
        crate::window::arena_wiring::open_debate_arena(&debate_arena_cache, proxy_state.clone());
    });
    window.add_action(&debate_arena_action);
}

pub fn build_header_bar(_app: &Application) -> HeaderBar {
    let header_bar = HeaderBar::new();

    let menu_model = crate::menu::build_context_menu();
    let menubar = gtk::PopoverMenuBar::from_model(Some(&menu_model));
    header_bar.pack_start(&menubar);

    let add_btn = Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add Model")
        .action_name("win.add_model")
        .css_classes(vec!["suggested-action"])
        .build();
    header_bar.pack_end(&add_btn);

    let refresh_btn = Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh")
        .action_name("win.refresh")
        .css_classes(vec!["flat"])
        .build();
    header_bar.pack_end(&refresh_btn);

    let manage_btn = Button::builder()
        .icon_name("preferences-system-symbolic")
        .tooltip_text("Manage Models")
        .action_name("win.manage_models")
        .css_classes(vec!["flat"])
        .build();
    header_bar.pack_end(&manage_btn);

    header_bar
}

#[allow(dead_code)]
pub fn build_menu_model(_app: &Application) -> Menu {
    let menu = Menu::new();

    let file_section = Menu::new();
    file_section.append(Some("Add Model"), Some("win.add_model"));
    file_section.append(Some("Quit"), Some("win.quit"));
    menu.append_section(None, &file_section);

    let edit_section = Menu::new();
    edit_section.append(Some("Preferences"), Some("win.preferences"));
    menu.append_section(None, &edit_section);

    let view_section = Menu::new();
    view_section.append(Some("Toggle Logs Panel"), Some("win.toggle_logs"));
    view_section.append(Some("Refresh"), Some("win.refresh"));
    menu.append_section(None, &view_section);

    let help_section = Menu::new();
    help_section.append(Some("About"), Some("win.about"));
    help_section.append(Some("Open GitHub Repo"), Some("win.github"));
    menu.append_section(None, &help_section);

    menu
}
