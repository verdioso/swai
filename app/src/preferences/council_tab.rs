//! SWAI — Council Pipeline Preferences Tab.
//!
//! UI controls for configuring the council debate pipeline:
//! stage list (add/remove), role picker, model dropdown, prompt template,
//! and pipeline mode selector (Auto/Concurrent/Sequential).
//!
//! A master ON/OFF switch at the top of the tab enables or disables the
//! entire council pipeline. When disabled, the proxy bypasses synthetic
//! debate interception entirely, routing council-model requests directly
//! to the target model with zero overhead.

use std::sync::{Arc, Mutex};

use gtk::{DropDown, Orientation, StringList};
use gtk4 as gtk;

use adw::prelude::*;
use adw::{ActionRow, EntryRow, PreferencesGroup, PreferencesPage, SwitchRow};

use swai_core::config::Config;
use swai_core::council::{CouncilMode, CouncilPipelineConfig, CouncilRole, PipelineStage};

/// Mutable state shared between UI widgets and the dialog's save path.
#[derive(Clone)]
pub struct CouncilTabState {
    pub stages: Vec<Arc<Mutex<PipelineStage>>>,
    pub mode: CouncilMode,
    pub enable_council: bool,
}

impl CouncilTabState {
    pub fn new(config: &Config) -> Self {
        Self {
            stages: config
                .council
                .stages
                .iter()
                .map(|s| Arc::new(Mutex::new(s.clone())))
                .collect(),
            mode: config.council.mode.clone(),
            enable_council: config.enable_council(),
        }
    }

    /// Build a `CouncilPipelineConfig` from the current in-memory state.
    pub fn to_config(&self) -> CouncilPipelineConfig {
        CouncilPipelineConfig {
            stages: self
                .stages
                .iter()
                .map(|s| s.lock().unwrap().clone())
                .collect(),
            mode: self.mode.clone(),
            fallback: swai_core::council::FallbackAction::default(),
            role_overrides: std::collections::HashMap::new(),
        }
    }
}

/// Build the Council Pipeline preferences page.
///
/// The returned page shares `state` with the dialog via callbacks,
/// so `dialog.council_config()` reads live UI values.
pub fn build_council_tab(config: &Config, state: &Arc<Mutex<CouncilTabState>>) -> PreferencesPage {
    let page = PreferencesPage::new();
    page.set_title("Council Pipeline");

    // Master ON/OFF switch.
    let master_group = PreferencesGroup::new();
    master_group.set_title("Council Pipeline");

    let enable_switch = SwitchRow::builder()
        .title("Enable Council Pipeline")
        .subtitle(
            "Allow multi-model debate arbitration. When OFF, multiple models \
             run concurrently for independent tools with direct routing.",
        )
        .active(state.lock().unwrap().enable_council)
        .build();
    master_group.add(&enable_switch);
    page.add(&master_group);

    // Pipeline configuration group (dimmed when master switch is OFF).
    let group = PreferencesGroup::new();
    group.set_title("Pipeline Stages");
    group.set_description(Some("Configure the debate pipeline stages"));

    let mode_dropdown = add_mode_row(&group, state);
    let stage_list = gtk::Box::new(Orientation::Vertical, 6);
    stage_list.set_margin_start(12);
    stage_list.set_margin_end(12);
    stage_list.set_margin_top(6);
    stage_list.set_margin_bottom(6);

    let add_btn = gtk::Button::builder()
        .label("Add Stage")
        .css_classes(vec!["suggested-action"])
        .halign(gtk::Align::End)
        .build();

    for stage in &state.lock().unwrap().stages {
        add_stage_row(&stage_list, state, stage.clone(), config);
    }

    let sl = stage_list.clone();
    let st = state.clone();
    let cf = config.clone();
    add_btn.connect_clicked(move |_| {
        let first = cf
            .council
            .stages
            .first()
            .map(|s| s.model_id.clone())
            .or_else(|| cf.models.first().map(|m| m.id.clone()))
            .unwrap_or_default();
        let new_stage = Arc::new(Mutex::new(PipelineStage {
            model_id: first,
            role: CouncilRole::Auditor,
            prompt_template: String::new(),
            temperature: 0.7,
            top_p: 0.9,
            system_prompt: None,
        }));
        add_stage_row(&sl, &st, new_stage.clone(), &cf);
        st.lock().unwrap().stages.push(new_stage);
    });

    group.add(&stage_list);
    group.add(&add_btn);
    page.add(&group);

    // Wire master switch to dim/disable pipeline controls.
    let gw = group.clone();
    let abw = add_btn.clone();
    let mdw = mode_dropdown.clone();
    let stw = state.clone();
    enable_switch.connect_active_notify(move |sw| {
        let enabled = sw.is_active();
        // Update state.
        stw.lock().unwrap().enable_council = enabled;
        // Dim or restore the pipeline controls.
        set_pipeline_sensitive(&gw, &abw, &mdw, enabled);
    });

    // Initial dim state.
    {
        let enabled = state.lock().unwrap().enable_council;
        set_pipeline_sensitive(&group, &add_btn, &mode_dropdown, enabled);
    }

    unsafe {
        page.set_data::<Arc<Mutex<CouncilTabState>>>("council-tab-state", state.clone());
        page.set_data::<gtk::Box>("stage-list", stage_list);
        page.set_data::<DropDown>("mode-dropdown", mode_dropdown);
        page.set_data::<SwitchRow>("enable-switch", enable_switch);
    }

    page
}

/// Set sensitivity of all pipeline configuration widgets.
///
/// When `enabled` is false, widgets are dimmed (not destroyed) so users can
/// still see the configuration but understand it is inactive.
fn set_pipeline_sensitive(
    group: &PreferencesGroup,
    add_btn: &gtk::Button,
    mode_dropdown: &DropDown,
    enabled: bool,
) {
    let opacity = if enabled { 1.0 } else { 0.4 };
    add_btn.set_opacity(opacity);

    // Dim the mode dropdown row.
    if let Some(row) = mode_dropdown.parent() {
        row.set_opacity(opacity);
    }

    // Toggle sensitivity on the group itself.
    group.set_sensitive(enabled);
}

/// Mode selector row (Auto / Concurrent / Sequential).
fn add_mode_row(parent: &PreferencesGroup, state: &Arc<Mutex<CouncilTabState>>) -> DropDown {
    let modes = ["Auto", "Concurrent", "Sequential"];
    let list = StringList::new(&modes);
    let dd = DropDown::new(Some(list), None::<gtk::Expression>);
    let idx = match &state.lock().unwrap().mode {
        CouncilMode::Auto => 0,
        CouncilMode::Concurrent => 1,
        CouncilMode::Sequential => 2,
    };
    dd.set_selected(idx as u32);
    let row = ActionRow::builder().title("Pipeline Mode").build();
    row.add_prefix(&dd);
    parent.add(&row);
    let sc = state.clone();
    dd.connect_notify(Some("selected"), move |dd, _| {
        let m = match dd.selected() {
            0 => CouncilMode::Auto,
            1 => CouncilMode::Concurrent,
            _ => CouncilMode::Sequential,
        };
        sc.lock().unwrap().mode = m;
    });
    dd
}

fn add_stage_row(
    parent: &gtk::Box,
    state: &Arc<Mutex<CouncilTabState>>,
    stage: Arc<Mutex<PipelineStage>>,
    config: &Config,
) {
    let wrapper = gtk::Box::new(Orientation::Vertical, 0);

    let header = gtk::Box::new(Orientation::Horizontal, 6);
    header.set_margin_top(6);
    header.set_margin_bottom(6);

    // Label.
    let lbl = gtk::Label::builder()
        .label("Stage:")
        .halign(gtk::Align::Start)
        .css_classes(vec!["dim-label", "title-3"])
        .build();
    header.append(&lbl);

    let stage_lock = stage.lock().unwrap();

    // Role picker.
    let roles = ["Generator", "Auditor", "Synthesizer"];
    let rl = StringList::new(&roles);
    let rdd = DropDown::new(Some(rl), None::<gtk::Expression>);
    let ri = match &stage_lock.role {
        CouncilRole::Generator => 0,
        CouncilRole::Auditor => 1,
        CouncilRole::Synthesizer => 2,
        CouncilRole::Custom(_) => 1,
    };
    rdd.set_selected(ri as u32);
    let rr = ActionRow::builder().title("Role").build();
    rr.add_prefix(&rdd);
    header.append(&rr);

    // Model dropdown.
    let mut mnames: Vec<&str> = vec!["— Select model —"];
    let mut mids: Vec<String> = vec![String::new()];
    for (id, _name) in config.configured_models() {
        mnames.push(_name);
        mids.push(id.to_string());
    }
    let ml = StringList::new(&mnames);
    let mdd = DropDown::new(Some(ml), None::<gtk::Expression>);
    for (i, id) in mids.iter().enumerate() {
        if id == &stage_lock.model_id {
            mdd.set_selected(i as u32);
            break;
        }
    }
    let mr = ActionRow::builder().title("Model").build();
    mr.add_prefix(&mdd);
    header.append(&mr);

    // Prompt template.
    let pe = EntryRow::builder().title("Prompt Template").build();
    pe.set_text(&stage_lock.prompt_template);

    drop(stage_lock);

    // Remove button.
    let rb = gtk::Button::from_icon_name("list-remove-symbolic");
    rb.set_tooltip_text(Some("Remove stage"));
    rb.set_css_classes(&["destructive-action"]);
    header.append(&rb);

    wrapper.append(&header);
    wrapper.append(&pe);
    parent.append(&wrapper);

    // Wire callbacks.
    let stg = stage.clone();
    rdd.connect_notify(Some("selected"), move |dd, _| {
        let role = match dd.selected() as usize {
            0 => CouncilRole::Generator,
            1 => CouncilRole::Auditor,
            2 => CouncilRole::Synthesizer,
            _ => CouncilRole::Auditor,
        };
        stg.lock().unwrap().role = role;
    });

    let stg = stage.clone();
    let mids_c = mids.clone();
    mdd.connect_notify(Some("selected"), move |dd, _| {
        let mid = mids_c[dd.selected() as usize].clone();
        stg.lock().unwrap().model_id = mid;
    });

    let stg = stage.clone();
    pe.connect_changed(move |entry| {
        stg.lock().unwrap().prompt_template = entry.text().to_string();
    });

    let st = state.clone();
    let stg = stage.clone();
    let wc = wrapper.clone();
    rb.connect_clicked(move |_| {
        st.lock().unwrap().stages.retain(|s| !Arc::ptr_eq(s, &stg));
        if let Some(parent) = wc.parent() {
            if let Ok(box_parent) = parent.downcast::<gtk::Box>() {
                box_parent.remove(&wc);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use swai_core::council::CouncilPipelineConfig;

    #[test]
    fn test_council_config_round_trip() {
        let orig = CouncilPipelineConfig {
            stages: vec![
                PipelineStage {
                    model_id: "llama3-8b".into(),
                    role: CouncilRole::Generator,
                    prompt_template: "Answer: {input}".into(),
                    temperature: 0.7,
                    top_p: 0.9,
                    system_prompt: None,
                },
                PipelineStage {
                    model_id: "llama3-8b".into(),
                    role: CouncilRole::Auditor,
                    prompt_template: "Critique: {input}".into(),
                    temperature: 0.5,
                    top_p: 0.85,
                    system_prompt: Some("Be thorough".into()),
                },
            ],
            mode: CouncilMode::Concurrent,
            fallback: swai_core::council::FallbackAction::Skip,
            role_overrides: std::collections::HashMap::new(),
        };
        let toml_str = toml::to_string_pretty(&orig).unwrap();
        let des: CouncilPipelineConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(des.stages.len(), 2);
        assert_eq!(des.stages[0].model_id, "llama3-8b");
        assert_eq!(des.stages[0].role, CouncilRole::Generator);
        assert_eq!(des.stages[0].prompt_template, "Answer: {input}");
        assert_eq!(des.stages[1].role, CouncilRole::Auditor);
        assert_eq!(des.stages[1].system_prompt.as_deref(), Some("Be thorough"));
        assert_eq!(des.mode, CouncilMode::Concurrent);
    }

    #[test]
    fn test_tab_state_to_config() {
        let state = CouncilTabState {
            stages: vec![Arc::new(Mutex::new(PipelineStage {
                model_id: "test-model".into(),
                role: CouncilRole::Synthesizer,
                prompt_template: "Summarize: {input}".into(),
                temperature: 0.3,
                top_p: 0.95,
                system_prompt: None,
            }))],
            mode: CouncilMode::Sequential,
            enable_council: true,
        };
        let cfg = state.to_config();
        assert_eq!(cfg.stages.len(), 1);
        assert_eq!(cfg.stages[0].model_id, "test-model");
        assert_eq!(cfg.stages[0].role, CouncilRole::Synthesizer);
        assert_eq!(cfg.mode, CouncilMode::Sequential);
    }
}
