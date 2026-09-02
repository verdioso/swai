#![allow(dead_code, unused)]
use gtk::prelude::*;
use gtk::{DropDown, Orientation, ResponseType, SpinButton, Window};
use gtk4 as gtk;

use adw::{EntryRow, SwitchRow};

use swai_core::config::Config;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::checkpoint_tab::{build_checkpoint_tab, CheckpointWidgets};
use super::council_tab::{build_council_tab, CouncilTabState};
use super::general_tab::{build_general_tab, GeneralWidgets};
use super::guides_tab::{build_guides_tab, GuidesWidgets};
use super::notifications_tab::{build_notifications_tab, NotificationsWidgets};
use super::proxy_tab::{build_proxy_tab, ProxyWidgets};
use super::types::PreferencesValues;

/// A modal dialog for editing global configuration with sidebar navigation.
#[derive(Clone)]
pub struct PreferencesDialog {
    pub widget: gtk::Dialog,
    log_dir_entry: EntryRow,
    proxy_port_entry: EntryRow,
    auto_restart_switch: SwitchRow,
    auto_follow_switch: SwitchRow,
    enable_notifications_switch: SwitchRow,
    notify_on_switch_switch: SwitchRow,
    autostart_switch: SwitchRow,
    max_concurrent_spin: SpinButton,
    summarizer_model_combo: DropDown,
    enable_checkpointing_switch: SwitchRow,
    enable_council_switch: SwitchRow,
    threshold_spin: SpinButton,
    council_tab_state: Arc<Mutex<CouncilTabState>>,
}

impl PreferencesDialog {
    /// Extract the selected summarizer model id from the dropdown.
    fn extract_summarizer_model(&self) -> Option<String> {
        let selected = self.summarizer_model_combo.selected();
        if selected == 0 {
            return None;
        }
        let config = swai_core::config::Config::load().ok()?;
        let idx = (selected as usize).saturating_sub(1);
        config
            .configured_models()
            .get(idx)
            .map(|(id, _)| id.to_string())
    }

    /// Extract the current form values as a serializable struct.
    pub fn values(&self) -> PreferencesValues {
        let log_dir_text = self.log_dir_entry.text();
        let log_dir = if log_dir_text.is_empty() {
            None
        } else {
            Some(PathBuf::from(log_dir_text.as_str()))
        };

        let proxy_port_text = self.proxy_port_entry.text();
        let proxy_port: Option<u16> = proxy_port_text.parse().ok();

        let auto_restart = self.auto_restart_switch.is_active();
        let auto_follow = self.auto_follow_switch.is_active();
        let enable_notifications = self.enable_notifications_switch.is_active();
        let notify_on_switch = self.notify_on_switch_switch.is_active();
        let autostart = self.autostart_switch.is_active();
        let max_concurrent = self.max_concurrent_spin.value() as usize;
        let summarizer_model = self.extract_summarizer_model();
        let enable_checkpointing = self.enable_checkpointing_switch.is_active();
        let enable_council = self.enable_council_switch.is_active();
        let compaction_threshold_pct = self.threshold_spin.value() as u8;

        PreferencesValues {
            log_dir,
            proxy_port,
            auto_restart_on_context_full: auto_restart,
            auto_follow_logs: auto_follow,
            enable_notifications,
            notify_on_switch,
            autostart_on_login: autostart,
            max_concurrent_models: max_concurrent,
            checkpoint_summarizer_model: summarizer_model,
            enable_checkpointing,
            enable_council,
            compaction_threshold_pct,
        }
    }

    /// Extract the council pipeline config from the tab.
    pub fn council_config(&self) -> swai_core::council::CouncilPipelineConfig {
        self.council_tab_state.lock().unwrap().to_config()
    }

    /// Create a new preferences dialog transient to the given parent window.
    pub fn new<T: IsA<Window>>(parent: &T, config: &Config, active_model_id: Option<&str>) -> Self {
        let widget = gtk::Dialog::builder()
            .title("Preferences")
            .transient_for(parent)
            .modal(true)
            .default_width(780)
            .default_height(560)
            .build();

        // Enforce a minimum window size so users cannot shrink it into an unusable state,
        // while allowing free resizing to larger dimensions.
        widget.set_size_request(740, 520);

        // Remove outer margins on content_area so the window fills edge-to-edge.
        let content_area = widget.content_area();
        content_area.set_margin_top(0);
        content_area.set_margin_bottom(0);
        content_area.set_margin_start(0);
        content_area.set_margin_end(0);

        // Hide the empty built-in action area of GtkDialog so it leaves no blank bottom bar
        if let Some(parent_box) = content_area.parent() {
            if let Some(action_area) = parent_box.last_child() {
                if action_area != content_area {
                    action_area.set_visible(false);
                }
            }
        }

        // Build sidebar with gtk::Stack and gtk::StackSidebar
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(150);
        stack.set_hexpand(true);
        stack.set_vexpand(true);

        // Build sidebar container box
        let sidebar_box = gtk::Box::new(Orientation::Vertical, 0);
        sidebar_box.set_size_request(210, -1);
        sidebar_box.set_vexpand(true);
        sidebar_box.set_valign(gtk::Align::Fill);
        sidebar_box.add_css_class("sidebar-container");

        let header_label = gtk::Label::builder()
            .label("<span weight=\"bold\" foreground=\"#2dd4f0\" size=\"9500\">SETTINGS</span>")
            .use_markup(true)
            .halign(gtk::Align::Start)
            .margin_start(16)
            .margin_top(16)
            .margin_bottom(10)
            .build();
        sidebar_box.append(&header_label);

        // Custom sidebar list with full-width rows and icons
        let list_box = gtk::ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::Single);
        list_box.add_css_class("sidebar-list");
        list_box.set_vexpand(true);
        list_box.set_valign(gtk::Align::Fill);

        let items = [
            ("general", "General", "preferences-system-symbolic"),
            ("proxy", "Proxy", "network-server-symbolic"),
            ("checkpoint", "Checkpoint", "document-save-symbolic"),
            (
                "notifications",
                "Notifications",
                "preferences-system-notifications-symbolic",
            ),
            ("council", "Council Pipeline", "system-users-symbolic"),
            ("guides", "Guides", "help-browser-symbolic"),
        ];

        for (_id, title, icon_name) in items.iter() {
            let row_box = gtk::Box::new(Orientation::Horizontal, 12);
            row_box.set_margin_start(16);
            row_box.set_margin_end(16);
            row_box.set_margin_top(10);
            row_box.set_margin_bottom(10);

            let icon = gtk::Image::from_icon_name(*icon_name);
            let label = gtk::Label::builder().label(*title).xalign(0.0).build();

            row_box.append(&icon);
            row_box.append(&label);

            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&row_box));
            list_box.append(&row);
        }

        // Select first row by default
        if let Some(first_row) = list_box.row_at_index(0) {
            list_box.select_row(Some(&first_row));
        }

        let stack_clone = stack.clone();
        list_box.connect_row_selected(move |_, row| {
            if let Some(r) = row {
                let name = match r.index() {
                    0 => "general",
                    1 => "proxy",
                    2 => "checkpoint",
                    3 => "notifications",
                    4 => "council",
                    5 => "guides",
                    _ => "general",
                };
                stack_clone.set_visible_child_name(name);
            }
        });

        sidebar_box.append(&list_box);

        // Build all preference pages and add to stack
        let (general_page, general_widgets) = build_general_tab(config);
        let (proxy_page, proxy_widgets) = build_proxy_tab(config);
        let (checkpoint_page, checkpoint_widgets) = build_checkpoint_tab(config, active_model_id);
        let (notifications_page, notifications_widgets) = build_notifications_tab(config);

        let council_tab_state = Arc::new(Mutex::new(CouncilTabState::new(config)));
        let council_page = build_council_tab(config, &council_tab_state);

        // Retrieve the enable_council switch from the council tab.
        let enable_council_switch = unsafe {
            let ptr = council_page
                .data::<SwitchRow>("enable-switch")
                .expect("council tab should have enable-switch");
            // NonNull<SwitchRow> -> &SwitchRow -> clone
            ptr.as_ref().clone()
        };

        let (guides_page, _) = build_guides_tab(config);

        // Add pages to stack with names
        stack.add_named(&general_page, Some("general"));
        stack.add_named(&proxy_page, Some("proxy"));
        stack.add_named(&checkpoint_page, Some("checkpoint"));
        stack.add_named(&notifications_page, Some("notifications"));
        stack.add_named(&council_page, Some("council"));
        stack.add_named(&guides_page, Some("guides"));

        // Right side container: scrollable stack on top, action buttons on bottom-right
        let right_box = gtk::Box::new(Orientation::Vertical, 0);
        right_box.set_hexpand(true);
        right_box.set_vexpand(true);

        let scrolled_stack = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&stack)
            .hexpand(true)
            .vexpand(true)
            .margin_top(12)
            .margin_bottom(6)
            .margin_start(16)
            .margin_end(16)
            .build();
        right_box.append(&scrolled_stack);

        // Action buttons inside right pane (Cancel & Save)
        let action_bar = gtk::Box::new(Orientation::Horizontal, 8);
        action_bar.set_halign(gtk::Align::End);
        action_bar.set_margin_end(16);
        action_bar.set_margin_bottom(12);
        action_bar.set_margin_top(6);

        let cancel_btn = gtk::Button::builder().label("Cancel").build();
        let widget_cancel = widget.clone();
        cancel_btn.connect_clicked(move |_| {
            widget_cancel.response(ResponseType::Cancel);
        });
        action_bar.append(&cancel_btn);

        let save_btn = gtk::Button::builder()
            .label("Save")
            .css_classes(vec!["suggested-action"])
            .build();
        let widget_save = widget.clone();
        save_btn.connect_clicked(move |_| {
            widget_save.response(ResponseType::Ok);
        });
        action_bar.append(&save_btn);

        right_box.append(&action_bar);

        // Layout: sidebar panel on left, right content on right
        let main_box = gtk::Box::new(Orientation::Horizontal, 0);
        main_box.set_hexpand(true);
        main_box.set_vexpand(true);
        main_box.append(&sidebar_box);
        main_box.append(&right_box);

        content_area.append(&main_box);

        let widget_clone = widget.clone();
        widget.connect_close_request(move |_| {
            widget_clone.response(ResponseType::Cancel);
            glib::Propagation::Proceed
        });

        // Extract real widget handles from the tab builders
        Self {
            widget,
            log_dir_entry: general_widgets.log_dir_entry,
            proxy_port_entry: proxy_widgets.proxy_port_entry,
            auto_restart_switch: proxy_widgets.auto_restart_switch,
            auto_follow_switch: notifications_widgets.auto_follow_switch,
            enable_notifications_switch: notifications_widgets.enable_notifications_switch,
            notify_on_switch_switch: notifications_widgets.notify_on_switch_switch,
            autostart_switch: general_widgets.autostart_switch,
            max_concurrent_spin: general_widgets.max_concurrent_spin,
            summarizer_model_combo: checkpoint_widgets.summarizer_model_combo,
            enable_checkpointing_switch: checkpoint_widgets.enable_checkpointing_switch,
            enable_council_switch,
            threshold_spin: checkpoint_widgets.threshold_spin,
            council_tab_state,
        }
    }

    /// Destroy the dialog window.
    #[allow(dead_code)]
    pub fn destroy(&self) {
        self.widget.destroy();
    }

    /// Read current values and save back to the config file.
    #[allow(dead_code)]
    pub fn save(&self, config_path: &std::path::Path) -> Result<(), String> {
        let log_dir_text = self.log_dir_entry.text();
        let log_dir = if log_dir_text.is_empty() {
            None
        } else {
            Some(PathBuf::from(log_dir_text.as_str()))
        };

        let proxy_port_text = self.proxy_port_entry.text();
        let proxy_port: Option<u16> = proxy_port_text.parse().ok();

        let auto_restart = self.auto_restart_switch.is_active();
        let auto_follow = self.auto_follow_switch.is_active();
        let enable_notifications = self.enable_notifications_switch.is_active();
        let notify_on_switch = self.notify_on_switch_switch.is_active();
        let autostart = self.autostart_switch.is_active();
        let max_concurrent = self.max_concurrent_spin.value() as usize;
        let summarizer_model = self.extract_summarizer_model();
        let enable_checkpointing = self.enable_checkpointing_switch.is_active();
        let enable_council = self.enable_council_switch.is_active();
        let compaction_threshold_pct = self.threshold_spin.value() as u8;

        let mut config = Config::load().map_err(|e| format!("Failed to load config: {}", e))?;
        config.global.log_dir = log_dir;
        config.global.proxy_port = proxy_port;
        config.global.auto_restart_on_context_full = Some(auto_restart);
        config.global.auto_follow_logs = Some(auto_follow);
        config.preferences.enable_notifications = enable_notifications;
        config.preferences.notify_on_switch = notify_on_switch;
        config.preferences.autostart_on_login = autostart;
        config.preferences.max_concurrent_models = max_concurrent;
        config.preferences.checkpoint_summarizer_model = summarizer_model;
        config.preferences.enable_checkpointing = enable_checkpointing;
        config.preferences.enable_council = enable_council;
        config.preferences.compaction_threshold_pct = compaction_threshold_pct;

        // Save council pipeline config.
        config.council = self.council_config();

        Config::validate(&config, config_path)
            .map_err(|e| format!("Config validation error: {}", e))?;

        let content = toml::to_string_pretty(&config)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        std::fs::write(config_path, &content)
            .map_err(|e| format!("Failed to write config file: {}", e))?;

        // Sync autostart state with the filesystem.
        if autostart {
            swai_core::autostart::enable_autostart()
                .map_err(|e| format!("Failed to enable autostart: {}", e))?;
        } else {
            swai_core::autostart::disable_autostart()
                .map_err(|e| format!("Failed to disable autostart: {}", e))?;
        }

        // Hot-reload: sync compaction threshold into ProxyState so the background
        // proxy picks up the new value on the very next request.
        // Note: This requires access to the ProxyState, which would need to be
        // passed as a parameter or stored as a field. For now, we'll add a note
        // about this requirement.

        Ok(())
    }
}
