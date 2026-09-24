#![allow(dead_code, unused)]
//! SWAI — Gemini-style visual chat components for debate turn display.
//!
//! Color-coded chat message bubbles for:
//! - Human Prompt / Chime In (Radiant Lavender / Purple)
//! - Generator Draft (Soft Sky Cyan)
//! - Auditor Critiques (Warm Amber / Peach)
//! - Synthesizer Consensus (Mint Emerald)

use gtk::prelude::*;
use gtk4 as gtk;

use swai_core::council::{CouncilRole, DebateTranscript, TurnResult};

/// Accent colors for roles (eye-friendly in dark and light modes).
pub const CYAN_COLOR: &str = "#38bdf8";
pub const AMBER_COLOR: &str = "#fbbf24";
pub const GREEN_COLOR: &str = "#34d399";
pub const PURPLE_COLOR: &str = "#c084fc";

/// Get role metadata: (CAPS name, color hex, css class).
pub fn role_meta(role: &CouncilRole) -> (&'static str, &'static str, &'static str) {
    match role {
        CouncilRole::Planner => ("PLANNER", PURPLE_COLOR, "planner"),
        CouncilRole::Generator => ("GENERATOR", CYAN_COLOR, "generator"),
        CouncilRole::Auditor => ("AUDITOR", AMBER_COLOR, "auditor"),
        CouncilRole::Synthesizer => ("SYNTHESIZER", GREEN_COLOR, "synthesizer"),
        CouncilRole::Custom(_) => ("CUSTOM", AMBER_COLOR, "auditor"),
    }
}

/// Create a single turn chat bubble with color-coded uppercase speaker header.
pub fn create_turn_card(turn: &TurnResult) -> gtk::Box {
    let (role_caps, color, css_class) = role_meta(&turn.role);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.set_css_classes(&["arena-bubble-ai", css_class]);

    // Header bar: "<B>ROLE</B> says: (model_id · Xs)"
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title_label = gtk::Label::new(None);
    title_label.set_markup(&format!(
        "<span foreground='{}'><b>{}</b></span> <span foreground='{}'>says:</span>",
        color, role_caps, color
    ));
    title_label.set_halign(gtk::Align::Start);

    let meta_label = gtk::Label::new(None);
    meta_label.set_markup(&format!(
        "<span alpha='65%' size='small'>({} · {:.1}s)</span>",
        glib::markup_escape_text(&turn.model_id),
        turn.duration.as_secs_f64()
    ));
    meta_label.set_halign(gtk::Align::End);
    meta_label.set_hexpand(true);

    header.append(&title_label);
    header.append(&meta_label);
    card.append(&header);

    // Error indicator.
    if let Some(ref error) = turn.error {
        let error_label = gtk::Label::new(Some(&format!("⚠ Error: {}", error)));
        error_label.add_css_class("error-label");
        card.append(&error_label);
    }

    // Output text view (read-only, expands naturally without inner scrollbox).
    let text_view = gtk::TextView::new();
    text_view.set_editable(false);
    text_view.set_wrap_mode(gtk::WrapMode::WordChar);
    text_view.set_monospace(false);
    text_view.set_pixels_above_lines(2);
    text_view.set_pixels_below_lines(2);

    let buffer = text_view.buffer();
    buffer.set_text(&turn.output);
    card.append(&text_view);

    card
}

/// Create human prompt or chime-in bubble (right-aligned).
pub fn create_human_bubble(text: &str, is_chime_in: bool) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.set_css_classes(&["arena-bubble-user"]);
    card.set_halign(gtk::Align::End);

    let header = gtk::Label::new(None);
    let label = if is_chime_in { "HUMAN (Chimed In)" } else { "HUMAN" };
    header.set_markup(&format!(
        "<span foreground='{}'><b>{}</b></span> <span foreground='{}'>says:</span>",
        PURPLE_COLOR, label, PURPLE_COLOR
    ));
    header.set_halign(gtk::Align::Start);
    card.append(&header);

    let text_view = gtk::TextView::new();
    text_view.set_editable(false);
    text_view.set_wrap_mode(gtk::WrapMode::WordChar);
    text_view.set_monospace(false);
    text_view.buffer().set_text(text);
    card.append(&text_view);

    card
}

/// Create a full transcript view with all turns stacked vertically.
pub fn create_transcript_view(transcript: &DebateTranscript) -> gtk::ScrolledWindow {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 12);
    container.set_css_classes(&["arena-stream-container"]);

    // 1. Human Prompt bubble (right-aligned).
    let prompt_bubble = create_human_bubble(&transcript.input_prompt, false);
    container.append(&prompt_bubble);

    // 2. Turn cards in sequence.
    for turn in &transcript.turns {
        let card = create_turn_card(turn);
        container.append(&card);
    }

    // 3. Summary footer.
    let summary = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    summary.set_margin_top(16);
    summary.set_margin_bottom(24);

    let turn_count = gtk::Label::new(Some(&format!("Total turns: {}", transcript.turn_count())));
    turn_count.set_css_classes(&["dim-label"]);

    let status_label = if transcript.all_succeeded() {
        gtk::Label::new(Some("✓ Debate completed successfully"))
    } else {
        gtk::Label::new(Some("⚠ Some stages had errors"))
    };
    status_label.set_hexpand(true);
    status_label.set_halign(gtk::Align::End);

    summary.append(&turn_count);
    summary.append(&status_label);
    container.append(&summary);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled.set_child(Some(&container));
    scrolled
}
