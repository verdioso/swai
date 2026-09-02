//! SWAI — General Preferences Tab.
//!
//! Core application settings: log directory, autostart, max concurrent models, and storage maintenance.

use gtk::{FileChooserAction, ResponseType, SpinButton};
use gtk4 as gtk;

use adw::prelude::*;
use adw::{ActionRow, EntryRow, PreferencesGroup, PreferencesPage, SwitchRow};

use std::path::PathBuf;
use swai_core::config::Config;

/// Widget handles for the General tab.
pub struct GeneralWidgets {
    pub log_dir_entry: EntryRow,
    pub autostart_switch: SwitchRow,
    pub max_concurrent_spin: SpinButton,
}

/// Build the General preferences page.
pub fn build_general_tab(config: &Config) -> (PreferencesPage, GeneralWidgets) {
    let page = PreferencesPage::new();
    page.set_title("General");

    let group = PreferencesGroup::new();
    group.set_title("Application Settings");

    let log_dir_entry = add_log_dir_row(&group, config);
    let autostart_switch = add_autostart_on_login_row(&group, config);
    let max_concurrent_spin = add_max_concurrent_models_row(&group, config);

    page.add(&group);

    // Storage maintenance section.
    let storage_group = PreferencesGroup::new();
    storage_group.set_title("Storage Maintenance");
    storage_group.set_description(Some("One-click storage cleanup for application data"));

    let _clear_logs_btn = add_clear_logs_button(&storage_group);
    let _clear_checkpoints_btn = add_clear_checkpoints_button(&storage_group);

    page.add(&storage_group);

    let widgets = GeneralWidgets {
        log_dir_entry,
        autostart_switch,
        max_concurrent_spin,
    };

    (page, widgets)
}

/// Add a log directory entry row with Browse button.
pub fn add_log_dir_row(parent: &PreferencesGroup, config: &Config) -> EntryRow {
    let row = EntryRow::builder().title("Log directory").build();

    let current_path = config.log_dir();
    row.set_text(current_path.to_string_lossy().as_ref());

    let entry_clone = row.clone();
    let browse_btn = gtk::Button::builder()
        .label("Browse…")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    browse_btn.connect_clicked(move |_| {
        show_folder_chooser(&entry_clone);
    });
    row.add_suffix(&browse_btn);

    parent.add(&row);
    row
}

/// Show a folder chooser dialog using the async run_async pattern.
fn show_folder_chooser(entry: &EntryRow) {
    let chooser = gtk::FileChooserDialog::new(
        Some("Select Log Directory"),
        None::<&gtk::Window>,
        FileChooserAction::SelectFolder,
        &[
            ("_Cancel", ResponseType::Cancel),
            ("_Select", ResponseType::Ok),
        ],
    );

    if let Ok(path) = std::env::var("HOME") {
        let _ = chooser.set_current_folder(Some(&gio::File::for_path(PathBuf::from(path))));
    }

    let entry_clone = entry.clone();
    chooser.run_async(move |chooser, response| {
        if response == ResponseType::Ok {
            if let Some(folder) = chooser.current_folder() {
                if let Some(path) = folder.path() {
                    entry_clone.set_text(&path.to_string_lossy());
                }
            }
        }
        chooser.destroy();
    });
}

/// Add a switch row for autostart on login.
pub fn add_autostart_on_login_row(parent: &PreferencesGroup, config: &Config) -> SwitchRow {
    let row = SwitchRow::builder()
        .title("Start SWAI automatically on login")
        .subtitle("Launch the SWAI tray and proxy daemon on system boot")
        .build();

    let autostart = config.autostart_on_login();
    row.set_active(autostart);

    parent.add(&row);
    row
}

/// Add a spin button for configuring the maximum number of concurrent model servers (1–4).
pub fn add_max_concurrent_models_row(parent: &PreferencesGroup, config: &Config) -> SpinButton {
    let current = config.max_concurrent_models().clamp(1, 4) as f64;

    let adj = gtk::Adjustment::new(current, 1.0, 4.0, 1.0, 1.0, 0.0);

    let spin = SpinButton::new(Some(&adj), 0.0, 0);
    spin.set_snap_to_ticks(true);
    spin.set_valign(gtk::Align::Center);

    let row = ActionRow::builder()
        .title("Max concurrent models")
        .subtitle("Number of model servers allowed to run simultaneously (1–4)")
        .build();
    row.add_suffix(&spin);

    parent.add(&row);
    spin
}

/// Add a "Clear All Logs" button with confirmation dialog.
pub fn add_clear_logs_button(parent: &PreferencesGroup) -> ActionRow {
    let row = ActionRow::builder()
        .title("Clear Application Logs")
        .subtitle("Delete all log files in the configured log directory")
        .build();

    let btn = gtk::Button::builder()
        .label("Clear Logs")
        .css_classes(vec!["destructive-action"])
        .valign(gtk::Align::Center)
        .build();

    btn.connect_clicked(move |_| {
        show_confirmation_dialog(
            "Clear All Logs",
            "This will delete all files in the log directory. This action cannot be undone.",
            "Clear Logs",
            || {
                if let Ok(config) = Config::load() {
                    let log_dir = config.log_dir();
                    if log_dir.exists() {
                        // Find the most recent active log file for each configured model.
                        let active_log_paths: Vec<PathBuf> = config
                            .models
                            .iter()
                            .filter_map(|m| {
                                let stem = m
                                    .script_path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or(&m.id);
                                let mut matching = Vec::new();
                                if let Ok(entries) = std::fs::read_dir(&log_dir) {
                                    for entry in entries.flatten() {
                                        let name = entry.file_name().to_string_lossy().to_string();
                                        if name.starts_with(&format!("{}_", stem))
                                            && name.ends_with(".log")
                                        {
                                            matching.push(entry.path());
                                        }
                                    }
                                }
                                matching.sort();
                                matching.pop()
                            })
                            .collect();

                        if let Ok(entries) = std::fs::read_dir(&log_dir) {
                            for entry in entries.flatten() {
                                let path = entry.path();
                                if path.is_file() {
                                    if active_log_paths.contains(&path) {
                                        // Truncate active log file in-place so running process
                                        // preserves its open file descriptor and writes new logs.
                                        let _ = std::fs::OpenOptions::new()
                                            .write(true)
                                            .truncate(true)
                                            .open(&path);
                                    } else {
                                        // Delete older/rotated log files completely.
                                        let _ = std::fs::remove_file(&path);
                                    }
                                }
                            }
                            tracing::info!("Cleared all log files in {:?}", log_dir);
                        }
                    }
                }
            },
        );
    });

    row.add_suffix(&btn);
    parent.add(&row);
    row
}

/// Add a "Clear All Checkpoints" button with confirmation dialog.
pub fn add_clear_checkpoints_button(parent: &PreferencesGroup) -> ActionRow {
    let row = ActionRow::builder()
        .title("Clear Context Checkpoints")
        .subtitle("Delete all saved conversation checkpoint ledgers")
        .build();

    let btn = gtk::Button::builder()
        .label("Clear Checkpoints")
        .css_classes(vec!["destructive-action"])
        .valign(gtk::Align::Center)
        .build();

    btn.connect_clicked(move |_| {
        show_confirmation_dialog(
            "Clear All Checkpoints",
            "This will delete all checkpoint session files in ~/.local/share/swai/checkpoints/. This action cannot be undone.",
            "Clear Checkpoints",
            || {
                if let Ok(home) = std::env::var("HOME") {
                    let checkpoint_dir = PathBuf::from(home)
                        .join(".local")
                        .join("share")
                        .join("swai")
                        .join("checkpoints");
                    if checkpoint_dir.exists() {
                        if let Ok(entries) = std::fs::read_dir(&checkpoint_dir) {
                            for entry in entries.flatten() {
                                let _ = std::fs::remove_file(entry.path());
                            }
                            tracing::info!("Cleared all checkpoint files in {:?}", checkpoint_dir);
                        }
                    }
                }
            },
        );
    });

    row.add_suffix(&btn);
    parent.add(&row);
    row
}

/// Show a confirmation dialog.
fn show_confirmation_dialog(
    title: &str,
    message: &str,
    confirm_label: &str,
    on_confirm: impl FnOnce() + 'static,
) {
    let dialog = adw::MessageDialog::builder()
        .heading(title)
        .body(message)
        .build();

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("confirm", confirm_label);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);

    let on_confirm = std::sync::Arc::new(std::sync::Mutex::new(Some(on_confirm)));
    dialog.connect_response(None, move |_dialog, response| {
        if response == "confirm" {
            if let Some(callback) = on_confirm.lock().unwrap().take() {
                callback();
            }
        }
    });

    dialog.present();
}
