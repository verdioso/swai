use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, Label, Orientation, ProgressBar, Switch};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::types::{CardState, PollingState};

pub struct ModelCard {
    /// The underlying config for this model.
    config: swai_core::config::ModelConfig,
    /// Current UI-visible state (interior mutability).
    state: Rc<RefCell<CardState>>,
    /// Context polling state (interior mutability).
    pub(crate) polling_state: Rc<RefCell<PollingState>>,
    /// The card container (vertical box with all widgets).
    pub widget: GtkBox,
    /// Bold model name label.
    pub name_label: Label,
    /// Port subtitle label.
    pub port_label: Label,
    /// The ON/OFF switch.
    pub switch: Switch,
    /// Status text label.
    status_label: Label,
    /// Live speed label (⚡ 41.5 tok/s).
    pub(crate) speed_label: Label,
    /// Live prompt speed label (📥 450.2 p-tok/s).
    pub(crate) prompt_speed_label: Label,
    /// Live stopwatch label (⏱ 4.2s).
    pub(crate) stopwatch_label: Label,
    /// Context progress bar (4px thin bar).
    pub(crate) context_bar: ProgressBar,
    /// Context usage label below the progress bar.
    pub(crate) context_label: Label,
    /// Cumulative active session tokens label (TOTAL: xxxx tokens used).
    pub(crate) total_tokens_label: Label,
    /// Restart button (icon).
    pub restart_button: Button,
    /// Logs button (icon) — opens a log viewer window for this model's log file.
    pub logs_button: Button,
    /// Closure called when the Logs button is clicked.
    on_logs_clicked: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn()>>>>,
    /// Blocks the toggle handler during programmatic state changes.
    /// Prevents GTK4's re-entrant `notify::active` signal from spawning
    /// unwanted stop/switch threads when set_state/set_starting
    /// programmatically changes the switch's active state.
    signal_block: Rc<Cell<bool>>,
}

impl ModelCard {
    /// Create a new model card from a model config.
    pub fn new(config: &swai_core::config::ModelConfig) -> Self {
        let state = Rc::new(RefCell::new(CardState::Stopped));
        let polling_state = Rc::new(RefCell::new(PollingState::Inactive));

        // ── Card layout (3 rows) ───────────────────────────────────
        let card = GtkBox::new(Orientation::Vertical, 4);
        card.set_hexpand(true);

        // ── Row 1: Model name + port (left) | Status + controls (right) ─
        let top_row = GtkBox::new(Orientation::Horizontal, 8);
        top_row.set_hexpand(true);

        // Left side: bold model name + port subtitle in vertical stack
        let left_vbox = GtkBox::new(Orientation::Vertical, 2);
        let name_label = Label::new(Some(&config.name));
        name_label.set_css_classes(&["heading"]);
        name_label.set_halign(gtk::Align::Start);

        let port_label = Label::new(None);
        port_label.set_markup(&format!(
            "<span font_weight='bold'>PORT:</span> {}",
            config.port
        ));
        port_label.set_css_classes(&["dim-label", "caption"]);
        port_label.set_halign(gtk::Align::Start);

        left_vbox.append(&name_label);
        left_vbox.append(&port_label);

        // Right side: status + controls
        let right_side = GtkBox::new(Orientation::Horizontal, 8);
        right_side.set_hexpand(true);
        right_side.set_halign(gtk::Align::End);
        right_side.set_valign(gtk::Align::Center);

        // Status text
        let status_label = Label::new(Some("Stopped"));
        if !config.script_path.exists() {
            status_label.set_css_classes(&["caption"]);
            status_label.set_markup("<span foreground='#f66151'>⚠️ Script not found</span>");
        } else {
            status_label.set_css_classes(&["dim-label", "caption"]);
        }
        status_label.set_halign(gtk::Align::Start);
        status_label.set_valign(gtk::Align::Center);

        // Speed label (⚡ 41.5 tok/s) — hidden until model is Ready.
        let speed_label = Label::new(Some(""));
        speed_label.set_css_classes(&["dim-label", "caption"]);
        speed_label.set_halign(gtk::Align::Start);
        speed_label.set_valign(gtk::Align::Center);
        speed_label.set_visible(false); // hidden by default

        // Prompt speed label (📥 450.2 p-tok/s) — hidden until model is Ready.
        let prompt_speed_label = Label::new(Some(""));
        prompt_speed_label.set_css_classes(&["dim-label", "caption"]);
        prompt_speed_label.set_halign(gtk::Align::Start);
        prompt_speed_label.set_valign(gtk::Align::Center);
        prompt_speed_label.set_visible(false); // hidden by default

        // Stopwatch label (⏱ 4.2s) — hidden until model is Ready.
        let stopwatch_label = Label::new(Some(""));
        stopwatch_label.set_css_classes(&["dim-label", "caption"]);
        stopwatch_label.set_halign(gtk::Align::Start);
        stopwatch_label.set_valign(gtk::Align::Center);
        stopwatch_label.set_visible(false); // hidden by default

        // Controls (switch + restart + logs)
        let controls = GtkBox::new(Orientation::Horizontal, 4);
        controls.set_valign(gtk::Align::Center);

        // ON/OFF switch
        let switch = Switch::new();
        switch.set_active(false);
        switch.set_halign(gtk::Align::End);
        switch.set_valign(gtk::Align::Center);

        // Restart icon button
        let restart_button = Button::from_icon_name("view-refresh-symbolic");
        restart_button.set_css_classes(&["flat"]);
        restart_button.set_sensitive(false); // disabled until model is Ready/Error
        restart_button.set_tooltip_text(Some("Restart"));

        // Logs icon button
        let logs_button = Button::from_icon_name("text-x-generic-symbolic");
        logs_button.set_css_classes(&["flat"]);
        logs_button.set_sensitive(false); // disabled until model is Ready/Error
        logs_button.set_tooltip_text(Some("View logs"));

        controls.append(&switch);
        controls.append(&restart_button);
        controls.append(&logs_button);

        right_side.append(&status_label);
        right_side.append(&speed_label);
        right_side.append(&prompt_speed_label);
        right_side.append(&stopwatch_label);
        right_side.append(&controls);

        top_row.append(&left_vbox);
        top_row.append(&right_side);

        // ── Row 2: Context progress bar (4px thin) ─────────────────
        let context_bar = ProgressBar::new();
        context_bar.set_fraction(0.0);
        context_bar.set_show_text(false);
        context_bar.set_hexpand(true);
        context_bar.set_css_classes(&["progressbar"]);

        // ── Row 3: Context usage text (left) & Total session tokens (right) ──
        let bottom_row = GtkBox::new(Orientation::Horizontal, 8);
        bottom_row.set_hexpand(true);

        let context_label = Label::new(Some(""));
        context_label.set_css_classes(&["caption", "dim-label"]);
        context_label.set_halign(gtk::Align::Start);
        context_label.set_hexpand(true);

        let total_tokens_label = Label::new(Some(""));
        total_tokens_label.set_css_classes(&["caption"]);
        total_tokens_label.set_halign(gtk::Align::End);
        total_tokens_label.set_visible(false);

        bottom_row.append(&context_label);
        bottom_row.append(&total_tokens_label);

        card.append(&top_row);
        card.append(&context_bar);
        card.append(&bottom_row);

        Self {
            config: config.clone(),
            state,
            polling_state,
            widget: card,
            name_label,
            port_label,
            switch,
            status_label,
            speed_label,
            prompt_speed_label,
            stopwatch_label,
            context_bar,
            context_label,
            total_tokens_label,
            restart_button,
            logs_button,
            signal_block: Rc::new(Cell::new(false)),
            on_logs_clicked: Rc::new(RefCell::new(None::<Box<dyn Fn()>>)),
        }
    }

    /// Set the toggle callback. Called by MainWindow after construction.
    ///
    /// GTK stores the closure for the lifetime of the switch, so the
    /// handler stays alive automatically — no need to store it separately.
    pub fn set_toggle_handler(&mut self, handler: impl Fn(bool) + 'static) {
        let handler = Rc::new(handler);
        let guard = Rc::clone(&self.signal_block);
        let switch_clone = self.switch.clone();
        let handler_ref = Rc::clone(&handler);
        self.switch.connect_active_notify(move |_| {
            // Block re-entrant calls triggered by programmatic set_active()
            if guard.get() {
                return;
            }
            handler_ref(switch_clone.is_active());
        });
    }

    /// Set the callback invoked when the Logs button is clicked.
    ///
    /// Called by MainWindow after construction to open a log viewer window
    /// scoped to this model's log file.
    pub fn set_logs_handler(&mut self, handler: impl Fn() + 'static) {
        *self.on_logs_clicked.borrow_mut() = Some(Box::new(handler));

        // Wire the click handler. The closure captures `handler_ref` (an Rc)
        // which keeps the handler alive for the lifetime of the button.
        let handler_ref = Rc::clone(&self.on_logs_clicked);
        self.logs_button.connect_clicked(move |_| {
            if let Some(ref cb) = *handler_ref.borrow() {
                cb();
            }
        });
    }

    /// Return a reference to the model's config.
    pub fn config(&self) -> &swai_core::config::ModelConfig {
        &self.config
    }

    /// Get the current UI-visible state.
    pub fn state(&self) -> CardState {
        self.state.borrow().clone()
    }

    /// Returns the current polling state.
    #[allow(dead_code)]
    pub fn polling_state(&self) -> PollingState {
        self.polling_state.borrow().clone()
    }

    /// Set the current UI-visible state and update all widgets.
    pub fn set_state(&self, new_state: CardState) {
        // Block re-entrant notify::active signals from GTK4 before any programmatic
        // set_active() calls — the handler checks this guard and returns early.
        self.block_signals();
        let is_on = new_state.is_on();
        let transitioning = new_state.is_transitioning();

        self.switch.set_active(is_on);
        self.switch.set_sensitive(!transitioning);
        self.status_label.set_text(new_state.status_text());

        // Color the status label: cyan for Ready, dim for everything else.
        if matches!(&new_state, CardState::Ready) {
            self.status_label
                .set_css_classes(&["caption", "accent-label"]);
        } else {
            self.status_label.set_css_classes(&["dim-label", "caption"]);
        }

        // Clear speed labels when not Ready (transitional states don't have live metrics).
        if !matches!(&new_state, CardState::Ready) {
            self.clear_speed();
            self.clear_prompt_speed();
            self.clear_stopwatch();
        }
        if matches!(&new_state, CardState::Stopped) {
            self.clear_context();
        }

        // Update restart button and logs button sensitivity: enabled only when
        // Ready or Error (logs available once the model has produced output).
        let interactive = matches!(&new_state, CardState::Ready | CardState::Error(_));
        self.restart_button.set_sensitive(interactive);
        self.logs_button.set_sensitive(interactive);

        *self.state.borrow_mut() = new_state;
        self.unblock_signals();
    }

    /// Set the card to "Starting..." and disable the switch.
    pub fn set_starting(&self) {
        self.block_signals();
        self.switch.set_active(true);
        self.switch.set_sensitive(false);
        self.status_label.set_text("Starting...");
        self.restart_button.set_sensitive(false);
        self.logs_button.set_sensitive(false);
        *self.state.borrow_mut() = CardState::Starting;
        self.unblock_signals();
    }

    /// Disable the switch button.
    pub fn disable_toggle(&self) {
        self.switch.set_sensitive(false);
    }

    /// Re-enable the switch button if not in a transitioning state.
    pub fn enable_toggle(&self) {
        let current = self.state.borrow().clone();
        if !current.is_transitioning() {
            self.switch.set_sensitive(true);
        }
    }

    /// Block the toggle handler to prevent re-entrant `notify::active` signals.
    /// Call before any programmatic state changes that will call
    /// `set_active()` on the switch widget.
    pub fn block_signals(&self) {
        self.signal_block.set(true);
    }

    /// Unblock the toggle handler after a programmatic state change.
    pub fn unblock_signals(&self) {
        self.signal_block.set(false);
    }


    /// Mark the restart button as "Restarting…" and disable it.
    pub fn disable_restart(&self) {
        self.block_signals();
        self.restart_button.set_tooltip_text(Some("Restarting…"));
        self.restart_button.set_sensitive(false);
        self.unblock_signals();
    }

    /// Restore the restart button to its normal state.
    pub fn enable_restart(&self) {
        self.block_signals();
        self.restart_button.set_tooltip_text(Some("Restart"));
        let current = self.state.borrow().clone();
        self.restart_button
            .set_sensitive(matches!(&current, CardState::Ready | CardState::Error(_)));
        self.unblock_signals();
    }

    /// Check if a restart is currently in progress (button shows "Restarting…").
    pub fn restart_requested(&self) -> bool {
        self.restart_button
            .tooltip_text()
            .map(|text| text.contains("Restarting"))
            .unwrap_or(false)
    }

    /// Update the card's display name, port label, and stored config.
    ///
    /// Called from the main thread when a model's settings change
    /// via the Edit dialog (broadcast through the import channel).
    pub fn update_model(&mut self, new_name: &str, new_port: u16) {
        self.name_label.set_text(new_name);
        self.config.name = new_name.to_string();
        self.port_label.set_markup(&format!(
            "<span font_weight='bold'>PORT:</span> {}",
            new_port
        ));
        self.config.port = new_port;
    }

    /// Legacy helper for updating display name only.
    #[allow(dead_code)]
    pub fn update_display_name(&mut self, new_name: &str) {
        self.update_model(new_name, self.config.port);
    }
}
