//! SWAI — Checkpointing Preferences Tab.
//!
//! Context checkpointing / compaction controls (Phase 24_8 feature).
//! Phase 31 adds a configurable compaction trigger threshold slider.

use gtk::{DropDown, SpinButton, StringList};
use gtk4 as gtk;

use adw::prelude::*;
use adw::{ActionRow, PreferencesGroup, PreferencesPage, SwitchRow};

use swai_core::config::Config;

/// Widget handles for the Checkpointing tab.
pub struct CheckpointWidgets {
    pub enable_checkpointing_switch: SwitchRow,
    pub summarizer_model_combo: DropDown,
    pub threshold_spin: SpinButton,
}

/// Build the Checkpoint preferences page.
pub fn build_checkpoint_tab(
    config: &Config,
    active_model_id: Option<&str>,
) -> (PreferencesPage, CheckpointWidgets) {
    let page = PreferencesPage::new();
    page.set_title("Checkpoint");

    let group = PreferencesGroup::new();
    group.set_title("Context Checkpoint");
    group.set_description(Some(
        "Summarizes earlier turns at ~70% context capacity to prevent context exhaustion and retain conversation memory across long sessions.",
    ));

    let enable_switch = add_enable_checkpointing_row(&group, config);
    let summarizer_dropdown = add_summarizer_model_row(&group, config);

    let (threshold_spin, readout_group) =
        add_compaction_threshold_controls(&group, config, &summarizer_dropdown, active_model_id);

    page.add(&group);
    page.add(&readout_group);

    let widgets = CheckpointWidgets {
        enable_checkpointing_switch: enable_switch,
        summarizer_model_combo: summarizer_dropdown,
        threshold_spin,
    };

    (page, widgets)
}

/// Add a switch row for enabling/disabling context checkpointing.
pub fn add_enable_checkpointing_row(parent: &PreferencesGroup, config: &Config) -> SwitchRow {
    let row = SwitchRow::builder()
        .title("Enable Context Checkpoint")
        .subtitle("Switch Context Checkpoint ON/OFF")
        .build();

    let enabled = config.enable_checkpointing();
    row.set_active(enabled);

    parent.add(&row);
    row
}

/// Add a dropdown row for selecting the checkpoint summarizer model.
pub fn add_summarizer_model_row(parent: &PreferencesGroup, config: &Config) -> DropDown {
    let mut display_names: Vec<&str> = vec!["Same as active model (Default)"];
    let mut model_ids: Vec<Option<String>> = vec![None];

    for (id, name) in config.configured_models() {
        display_names.push(name);
        model_ids.push(Some(id.to_string()));
    }

    let string_list = StringList::new(&display_names);

    let dropdown = DropDown::new(Some(string_list), None::<gtk::Expression>);
    dropdown.set_valign(gtk::Align::Center);

    if let Some(ref preferred) = config.preferences.checkpoint_summarizer_model {
        for (i, opt_id) in model_ids.iter().enumerate() {
            if let Some(ref id) = opt_id {
                if id == preferred {
                    dropdown.set_selected(i as u32);
                    break;
                }
            }
        }
    }

    let row = ActionRow::builder()
        .title("Checkpoint Summarizer Model")
        .subtitle("Select a secondary model to offload background summarization")
        .build();
    row.add_suffix(&dropdown);

    parent.add(&row);
    dropdown
}

/// Add a slim spinbutton row for the threshold inside the card,
/// and return an informative readout group placed outside below the card.
fn add_compaction_threshold_controls(
    parent: &PreferencesGroup,
    config: &Config,
    dropdown: &DropDown,
    active_model_id: Option<&str>,
) -> (SpinButton, PreferencesGroup) {
    let current_pct = config.compaction_threshold_pct();

    let min = swai_core::compaction::MIN_THRESHOLD_PCT as f64;
    let max = swai_core::compaction::MAX_THRESHOLD_PCT as f64;

    let adjustment = gtk::Adjustment::builder()
        .value(current_pct as f64)
        .lower(min)
        .upper(max)
        .step_increment(5.0)
        .page_increment(10.0)
        .build();

    let spin = SpinButton::new(Some(&adjustment), 0.0, 0);
    spin.set_digits(0);
    spin.set_value(current_pct as f64);
    spin.set_width_chars(6);
    spin.set_valign(gtk::Align::Center);

    // Build a map of dropdown index -> ctx_size
    let active_ctx = active_model_id
        .and_then(|id| {
            config
                .models
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.ctx_size)
        })
        .unwrap_or_else(|| config.models.first().map(|m| m.ctx_size).unwrap_or(65_536));

    let mut model_ctxs: Vec<usize> = vec![active_ctx];
    for m in &config.models {
        model_ctxs.push(m.ctx_size);
    }

    let selected_idx = dropdown.selected() as usize;
    let initial_ctx = model_ctxs.get(selected_idx).copied().unwrap_or(active_ctx);

    let budget =
        swai_core::compaction::ContextBudget::from_ctx_size_and_threshold(initial_ctx, current_pct);

    // Info group placed outside below the main card
    let readout_group = PreferencesGroup::new();
    readout_group.set_title("Active Budget Estimation");

    let readout_label = gtk::Label::builder()
        .label(&format!(
            "<span foreground=\"#2dd4f0\">ℹ️</span>  <span alpha=\"80%\">{}</span>",
            budget.summary_display()
        ))
        .use_markup(true)
        .halign(gtk::Align::Start)
        .wrap(true)
        .xalign(0.0)
        .margin_start(6)
        .margin_top(2)
        .margin_bottom(6)
        .build();

    readout_group.add(&readout_label);

    let spin_for_dd = spin.clone();
    let readout_clone = readout_label.clone();
    let model_ctxs_clone = model_ctxs.clone();
    dropdown.connect_selected_notify(move |dd| {
        let idx = dd.selected() as usize;
        let ctx = model_ctxs_clone.get(idx).copied().unwrap_or(65_536);
        let pct = spin_for_dd.value() as u8;
        let b = swai_core::compaction::ContextBudget::from_ctx_size_and_threshold(ctx, pct);
        readout_clone.set_label(&format!(
            "<span foreground=\"#2dd4f0\">ℹ️</span>  <span alpha=\"80%\">{}</span>",
            b.summary_display()
        ));
    });

    let dd_clone = dropdown.clone();
    let readout_clone2 = readout_label.clone();
    let model_ctxs_clone2 = model_ctxs;
    spin.connect_value_changed(move |s| {
        let idx = dd_clone.selected() as usize;
        let ctx = model_ctxs_clone2.get(idx).copied().unwrap_or(65_536);
        let pct = s.value() as u8;
        let b = swai_core::compaction::ContextBudget::from_ctx_size_and_threshold(ctx, pct);
        readout_clone2.set_label(&format!(
            "<span foreground=\"#2dd4f0\">ℹ️</span>  <span alpha=\"80%\">{}</span>",
            b.summary_display()
        ));
    });

    let row = ActionRow::builder()
        .title("Compaction Trigger Threshold")
        .subtitle("Context usage percentage to initiate compaction (50%–85%)")
        .build();
    row.add_suffix(&spin);

    parent.add(&row);
    (spin, readout_group)
}
