use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{Box as GtkBox, Button, GestureClick, Label, Orientation};
use gtk4 as gtk;

use super::types::{ModelTelemetry, SlotUpdate, StageTelemetry};
use swai_core::config::Config;

/// Handles for the Fixed Bottom Deck widgets.
#[derive(Clone)]
pub struct BottomDeck {
    pub container: GtkBox,
    #[allow(dead_code)]
    pub web_chat_url_label: Label,
    pub model_pills_box: GtkBox,
    pub prompt_label: Label,
    pub decode_label: Label,
    pub telemetry_map: Rc<RefCell<HashMap<String, ModelTelemetry>>>,
    pub selected_model_id: Rc<RefCell<Option<String>>>,
    pub model_names: Rc<RefCell<HashMap<String, String>>>,
    current_pill_ids: Rc<RefCell<Vec<String>>>,
    #[allow(dead_code)]
    proxy_port: Rc<RefCell<u16>>,
}

impl BottomDeck {
    /// Create the Fixed Bottom Deck with Web AI Chat card and Telemetry card.
    pub fn new(proxy_port: u16, config: &Config) -> Self {
        let container = GtkBox::new(Orientation::Horizontal, 12);
        container.set_margin_start(16);
        container.set_margin_end(16);
        container.set_margin_top(4);
        container.set_margin_bottom(8);
        container.set_hexpand(true);

        let proxy_port_rc = Rc::new(RefCell::new(proxy_port));

        let mut names = HashMap::new();
        for m in &config.models {
            names.insert(m.id.clone(), m.name.clone());
        }
        let model_names = Rc::new(RefCell::new(names));
        let telemetry_map = Rc::new(RefCell::new(HashMap::new()));
        let selected_model_id = Rc::new(RefCell::new(None));
        let current_pill_ids = Rc::new(RefCell::new(Vec::new()));

        // ── 1. Web AI Chat Action Card (Left Column) ────────────
        let web_card = GtkBox::new(Orientation::Vertical, 4);
        web_card.set_css_classes(&["bottom-deck-card"]);
        web_card.set_hexpand(false);
        web_card.set_size_request(200, -1);

        let web_header = GtkBox::new(Orientation::Horizontal, 6);
        let web_icon = Label::new(Some("🌐"));
        let web_title = Label::new(Some("Web AI Chat"));
        web_title.set_css_classes(&["heading"]);
        web_header.append(&web_icon);
        web_header.append(&web_title);

        let port = *proxy_port_rc.borrow();
        let web_chat_url_label = Label::new(Some(&format!("http://127.0.0.1:{}", port)));
        web_chat_url_label.set_css_classes(&["dim-label", "caption"]);
        web_chat_url_label.set_halign(gtk::Align::Start);

        let open_btn = Button::with_label("Open in Browser ↗");
        open_btn.set_css_classes(&["bottom-deck-btn"]);
        open_btn.set_halign(gtk::Align::Start);
        open_btn.set_valign(gtk::Align::End);
        open_btn.set_margin_top(2);

        let port_for_click = Rc::clone(&proxy_port_rc);
        open_btn.connect_clicked(move |_| {
            let p = *port_for_click.borrow();
            let url = format!("http://127.0.0.1:{}", p);
            let _ = gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>);
        });

        // Make the entire card clickable too
        let card_gesture = GestureClick::new();
        let port_for_card = Rc::clone(&proxy_port_rc);
        card_gesture.connect_released(move |_, _, _, _| {
            let p = *port_for_card.borrow();
            let url = format!("http://127.0.0.1:{}", p);
            let _ = gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>);
        });
        web_card.add_controller(card_gesture);

        web_card.append(&web_header);
        web_card.append(&web_chat_url_label);
        web_card.append(&open_btn);

        // ── 2. Telemetry Inspector Card (Right Column) ─────────
        let telem_card = GtkBox::new(Orientation::Vertical, 4);
        telem_card.set_css_classes(&["bottom-deck-card"]);
        telem_card.set_hexpand(true);

        let telem_top_row = GtkBox::new(Orientation::Horizontal, 8);
        let telem_title = Label::new(Some("📊 Telemetry"));
        telem_title.set_css_classes(&["heading"]);
        telem_top_row.append(&telem_title);

        let model_pills_box = GtkBox::new(Orientation::Horizontal, 4);
        model_pills_box.set_hexpand(true);
        model_pills_box.set_halign(gtk::Align::End);
        telem_top_row.append(&model_pills_box);

        let prompt_label = Label::new(Some("Awaiting prompt request..."));
        prompt_label.set_css_classes(&["dim-label", "caption"]);
        prompt_label.set_halign(gtk::Align::Start);

        let decode_label = Label::new(Some(""));
        decode_label.set_css_classes(&["caption"]);
        decode_label.set_halign(gtk::Align::Start);

        telem_card.append(&telem_top_row);
        telem_card.append(&prompt_label);
        telem_card.append(&decode_label);

        container.append(&web_card);
        container.append(&telem_card);

        Self {
            container,
            web_chat_url_label,
            model_pills_box,
            prompt_label,
            decode_label,
            telemetry_map,
            selected_model_id,
            model_names,
            current_pill_ids,
            proxy_port: proxy_port_rc,
        }
    }

    /// Update the proxy port display label and stored port.
    #[allow(dead_code)]
    pub fn update_proxy_port(&self, port: u16) {
        *self.proxy_port.borrow_mut() = port;
        self.web_chat_url_label
            .set_text(&format!("http://127.0.0.1:{}", port));
    }

    /// Update telemetry data when a slot update arrives from poller.
    pub fn handle_slot_update(&self, update: &SlotUpdate) {
        let mut map = self.telemetry_map.borrow_mut();
        let telem = map
            .entry(update.model_id.clone())
            .or_insert_with(|| ModelTelemetry {
                model_id: update.model_id.clone(),
                ..Default::default()
            });

        if update.prompt_tokens > 0 {
            telem.prompt_tokens = update.prompt_tokens;
        }
        if update.prompt_per_second > 0.0 {
            telem.prompt_speed = update.prompt_per_second;
            if telem.prompt_speed > 0.0 && telem.prompt_tokens > 0 {
                telem.prompt_duration_sec = telem.prompt_tokens as f64 / telem.prompt_speed;
            }
        }

        if update.decoded_tokens > 0 {
            telem.decode_tokens = update.decoded_tokens;
        }
        if update.predicted_per_second > 0.0 {
            telem.decode_speed = update.predicted_per_second;
        }
        if let Some(dur) = update.elapsed_duration_sec {
            telem.elapsed_duration_sec = Some(dur);
        }
        telem.is_processing = update.is_processing;

        // Auto-select latest active model
        if update.is_processing || update.elapsed_duration_sec.is_some() {
            *self.selected_model_id.borrow_mut() = Some(update.model_id.clone());
        }

        drop(map);
        self.refresh_view();
    }

    /// Update telemetry data for Council pipeline.
    pub fn handle_council_telemetry(
        &self,
        council_data: &swai_core::proxy::state::CouncilTelemetryData,
        enable_council: bool,
    ) {
        if !enable_council {
            self.telemetry_map.borrow_mut().remove("council-pipeline");
            self.refresh_view();
            return;
        }

        let mut map = self.telemetry_map.borrow_mut();
        let telem = map
            .entry("council-pipeline".to_string())
            .or_insert_with(|| ModelTelemetry {
                model_id: "council-pipeline".to_string(),
                ..Default::default()
            });

        telem.decode_tokens = council_data.total_tokens;
        telem.elapsed_duration_sec = Some(council_data.total_duration_sec);
        telem.is_processing = council_data.is_processing;
        telem.current_stage = council_data.current_stage.clone();
        if council_data.total_duration_sec > 0.0 && council_data.total_tokens > 0 {
            telem.decode_speed = council_data.total_tokens as f64 / council_data.total_duration_sec;
        }

        telem.council_stages = council_data
            .stages
            .iter()
            .map(|s| StageTelemetry {
                stage_name: s.stage_name.clone(),
                model_id: s.model_id.clone(),
                output_tokens: s.output_tokens,
                duration_sec: s.duration_sec,
                speed: s.speed_tokens_sec,
            })
            .collect();

        if council_data.is_processing || !council_data.stages.is_empty() {
            *self.selected_model_id.borrow_mut() = Some("council-pipeline".to_string());
        }

        drop(map);
        self.refresh_view();
    }

    /// Ensure Council pill is present when enable_council is true.
    pub fn ensure_council_pill(&self, enable_council: bool) {
        let mut changed = false;
        if enable_council {
            let mut map = self.telemetry_map.borrow_mut();
            if !map.contains_key("council-pipeline") {
                map.insert(
                    "council-pipeline".to_string(),
                    ModelTelemetry {
                        model_id: "council-pipeline".to_string(),
                        ..Default::default()
                    },
                );
                changed = true;
            }
        } else {
            let mut map = self.telemetry_map.borrow_mut();
            if map.remove("council-pipeline").is_some() {
                changed = true;
            }
        }
        if changed {
            self.refresh_view();
        }
    }

    /// Remove telemetry data for a model when it is stopped.
    pub fn remove_model(&self, model_id: &str) {
        self.telemetry_map.borrow_mut().remove(model_id);
        let mut sel = self.selected_model_id.borrow_mut();
        if sel.as_deref() == Some(model_id) {
            *sel = None;
        }
        drop(sel);
        self.refresh_view();
    }

    /// Set selected model for telemetry inspection (e.g. from pill click or card click).
    pub fn select_model(&self, model_id: &str) {
        *self.selected_model_id.borrow_mut() = Some(model_id.to_string());
        self.refresh_view();
    }

    /// Update active model pills and current telemetry readout.
    pub fn refresh_view(&self) {
        let map = self.telemetry_map.borrow();
        let names = self.model_names.borrow();
        let current_sel = self.selected_model_id.borrow().clone();

        // If no selection yet, pick first available model in telemetry map
        let active_id = current_sel.or_else(|| map.keys().next().cloned());

        let mut model_keys: Vec<String> = map.keys().cloned().collect();
        model_keys.sort();

        let mut pill_ids = self.current_pill_ids.borrow_mut();

        if *pill_ids != model_keys {
            *pill_ids = model_keys.clone();
            while let Some(child) = self.model_pills_box.first_child() {
                self.model_pills_box.remove(&child);
            }

            for model_id in &model_keys {
                let display_name = if model_id == "council-pipeline" {
                    "⚖️ Council".to_string()
                } else {
                    names
                        .get(model_id)
                        .cloned()
                        .unwrap_or_else(|| model_id.clone())
                };
                let is_active = active_id.as_deref() == Some(model_id.as_str());

                let btn = Button::with_label(&display_name);
                if is_active {
                    btn.set_css_classes(&["model-pill", "model-pill-active"]);
                } else {
                    btn.set_css_classes(&["model-pill"]);
                }

                let this = self.clone();
                let m_id = model_id.clone();
                btn.connect_clicked(move |_| {
                    this.select_model(&m_id);
                });

                self.model_pills_box.append(&btn);
            }
        } else {
            let mut child_opt = self.model_pills_box.first_child();
            for model_id in &model_keys {
                if let Some(child) = child_opt {
                    let is_active = active_id.as_deref() == Some(model_id.as_str());
                    if is_active {
                        child.set_css_classes(&["model-pill", "model-pill-active"]);
                    } else {
                        child.set_css_classes(&["model-pill"]);
                    }
                    child_opt = child.next_sibling();
                }
            }
        }

        // 2. Render telemetry readout for selected model
        if let Some(ref m_id) = active_id {
            if let Some(telem) = map.get(m_id) {
                if m_id == "council-pipeline" {
                    if telem.is_processing {
                        self.prompt_label.set_markup(
                            "<span foreground='#e5a50a'>● <b>⚖️ Council Pipeline</b></span> <span alpha='70%'>running debate...</span>",
                        );
                        self.decode_label.set_markup(
                            "<span foreground='#2dd4f0'>Stage:</span> <span alpha='80%'>Generating initial draft...</span>",
                        );
                    } else if !telem.council_stages.is_empty() {
                        let total_dur = telem.elapsed_duration_sec.unwrap_or(0.0);
                        let p_str = format!(
                            "⚖️ Council Debate: <b>{} stages</b> · <b>{} tok</b> · <span foreground='#2dd4f0'><b>⏱ {:.1}s total</b></span>",
                            telem.council_stages.len(), telem.decode_tokens, total_dur
                        );
                        let stage_summaries: Vec<String> = telem
                            .council_stages
                            .iter()
                            .map(|s| {
                                format!(
                                    "{}: {} tok ({:.1}s)",
                                    s.stage_name, s.output_tokens, s.duration_sec
                                )
                            })
                            .collect();
                        let d_str = stage_summaries.join(" · ");
                        self.prompt_label.set_markup(&p_str);
                        self.decode_label
                            .set_markup(&format!("<span alpha='85%'>{}</span>", d_str));
                    } else {
                        self.prompt_label
                            .set_text("Awaiting Council pipeline debate request...");
                        self.decode_label.set_text("");
                    }
                    return;
                }

                let model_name = names.get(m_id).cloned().unwrap_or_else(|| m_id.clone());

                if telem.is_processing {
                    let dur = telem.elapsed_duration_sec.unwrap_or(0.0);
                    self.prompt_label.set_markup(&format!(
                        "<span foreground='#2dd4f0'>● <b>{}</b></span> <span alpha='70%'>is generating...</span>",
                        model_name
                    ));
                    self.decode_label.set_markup(&format!(
                        "<span foreground='#2dd4f0'>⚡ {:.1} tok/s</span> · <span alpha='80%'>⏱ {:.1}s elapsed</span>",
                        telem.decode_speed, dur
                    ));
                } else if telem.decode_tokens > 0 || telem.prompt_tokens > 0 {
                    let p_str = if telem.prompt_tokens > 0 && telem.prompt_speed > 0.0 {
                        format!(
                            "📥 Prompt: <b>{} tok</b> · {:.1}s · {:.1} t/s",
                            telem.prompt_tokens, telem.prompt_duration_sec, telem.prompt_speed
                        )
                    } else if telem.prompt_speed > 0.0 {
                        format!("📥 Prompt: {:.1} p-tok/s", telem.prompt_speed)
                    } else {
                        "📥 Prompt: completed".to_string()
                    };

                    let d_str = if telem.decode_tokens > 0 && telem.decode_speed > 0.0 {
                        let total_dur = telem.elapsed_duration_sec.unwrap_or(0.0);
                        format!(
                            "⚡ Decode: <span foreground='#2dd4f0'><b>{} tok</b> · {:.1} t/s</span> <span alpha='70%'>(⏱ {:.1}s total)</span>",
                            telem.decode_tokens, telem.decode_speed, total_dur
                        )
                    } else if telem.decode_speed > 0.0 {
                        format!(
                            "⚡ Decode: <span foreground='#2dd4f0'>{:.1} tok/s</span>",
                            telem.decode_speed
                        )
                    } else {
                        "".to_string()
                    };

                    self.prompt_label.set_markup(&p_str);
                    self.decode_label.set_markup(&d_str);
                } else {
                    self.prompt_label
                        .set_text(&format!("Awaiting prompt for {}...", model_name));
                    self.decode_label.set_text("");
                }
                return;
            }
        }

        self.prompt_label.set_text("Awaiting prompt request...");
        self.decode_label.set_text("");
    }
}
