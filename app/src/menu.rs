//! Application main menu bar and hamburger popover menu.
//! All action names are preserved — only the UI container changed.

use gio::Menu;

/// Build the application context menu for the hamburger button.
///
/// Contains File / Edit / View / Help sections with the same action names
/// as the old PopoverMenuBar: `win.add_model`, `win.quit`, `win.preferences`,
/// `win.refresh`, `win.toggle_logs`, `win.about`, `win.github`.
pub fn build_context_menu() -> Menu {
    let menu = Menu::new();

    // ── File menu ────────────────────────────────────────────────
    let file_menu = build_file_section();
    menu.append_submenu(Some("File"), &file_menu);

    // ── Edit menu ────────────────────────────────────────────────
    let edit_menu = build_edit_section();
    menu.append_submenu(Some("Edit"), &edit_menu);

    // ── View menu ────────────────────────────────────────────────
    let view_menu = build_view_section();
    menu.append_submenu(Some("View"), &view_menu);

    // ── Help menu ────────────────────────────────────────────────
    let help_menu = build_help_section();
    menu.append_submenu(Some("Help"), &help_menu);

    menu
}

/// Build the File section: Add Model, Quit.
fn build_file_section() -> Menu {
    let menu = Menu::new();
    menu.append(Some("Add Model"), Some("win.add_model"));
    menu.append(Some("Quit"), Some("win.quit"));
    menu
}

/// Build the Edit section: Preferences, Manage Models.
fn build_edit_section() -> Menu {
    let menu = Menu::new();
    menu.append(Some("Preferences"), Some("win.preferences"));
    menu.append(Some("Manage Models"), Some("win.manage_models"));
    menu
}

/// Build the View section: Refresh, Toggle Logs Panel, Debate Arena.
fn build_view_section() -> Menu {
    let menu = Menu::new();
    menu.append(Some("Refresh"), Some("win.refresh"));
    menu.append(Some("Toggle Logs Panel"), Some("win.toggle_logs"));
    menu.append(Some("Debate Arena"), Some("win.debate_arena"));
    menu
}

/// Build the Help section: Check for Updates, Open GitHub Repo, About.
fn build_help_section() -> Menu {
    let menu = Menu::new();
    menu.append(Some("Check for Updates…"), Some("win.check_updates"));
    menu.append(Some("Open GitHub Repo"), Some("win.github"));
    menu.append(Some("About"), Some("win.about"));
    menu
}
