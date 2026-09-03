//! SWAI — Arena live-stream stage bubble cards.
//!
//! Renders one "bubble" card per pipeline stage and updates it live as tokens
//! stream in. Each card owns:
//!
//! * a header with the role label, model id, and a pulse badge that lights up
//!   while the stage is generating;
//! * a monospace text view whose buffer is appended to on every token, with
//!   auto-scroll that sticks to the bottom while streaming but releases when the
//!   user manually scrolls up;
//! * a status footer that reflects the stage `StageStatus`.
//!
//! The pure state machine (`StageState`) is deliberately separated from the GTK
//! widgets so it can be unit-tested without a display. The GTK card is a thin
//! rendering layer on top of that state. All GTK mutations happen on the main
//! thread; the bridge in `window.rs` is solely responsible for routing events
//! onto the GLib main context.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use super::types::{ArenaStreamAction, StageStatus};

/// Width of a single stage bubble card.
#[allow(dead_code)]
const CARD_WIDTH: i32 = 320;

/// Whether auto-scroll is currently pinned to the bottom of the view.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollMode {
    /// Follow new tokens to the bottom automatically.
    Sticky,
    /// User scrolled up; stop following until they scroll back down.
    Manual,
}

/// Pure, GTK-free state for one stage bubble card.
///
/// Tracks the accumulated token text, the terminal status, and whether the
/// pulse badge should be lit. This is the testable core of the widget: apply
/// an `ArenaStreamAction` and inspect the resulting state. No GTK handle is
/// referenced here, so it can be exercised in a headless test.
#[derive(Debug, Clone, Default)]
pub struct StageState {
    /// Whether a stage has been started on this card.
    pub active: bool,
    /// Zero-based index of the active stage, if any.
    pub stage_index: Option<usize>,
    /// Role label shown in the header.
    pub role_label: Option<String>,
    /// Model id shown in the header.
    pub model_id: Option<String>,
    /// Accumulated token text.
    pub text: String,
    /// Current terminal status.
    pub status: StageStatus,
    /// Whether the pulse badge is lit (stage is generating).
    pub generating: bool,
    /// Human-readable footer text.
    pub footer: String,
    /// Human guidance injected by a "Chime In" intervention, if any. When
    /// present the card header shows a distinct 👤 badge.
    pub human_intervention: Option<String>,
}

impl StageState {
    /// Apply a UI action, mutating this state machine.
    ///
    /// `AppendToken` only accumulates when the action's stage index matches the
    /// currently active stage, mirroring the guard the GTK layer applies before
    /// touching the buffer.
    pub fn apply(&mut self, action: &ArenaStreamAction) {
        match action {
            ArenaStreamAction::StageStarted {
                stage_index,
                role_label,
                model_id,
            } => {
                self.active = true;
                self.stage_index = Some(*stage_index);
                self.role_label = Some(role_label.clone());
                self.model_id = Some(model_id.clone());
                self.text.clear();
                self.generating = true;
                self.status = StageStatus::Running;
                self.footer = "generating…".to_string();
            }
            ArenaStreamAction::AppendToken { stage_index, text } => {
                if self.stage_index.is_none() {
                    self.active = true;
                    self.stage_index = Some(*stage_index);
                    self.generating = true;
                    self.status = StageStatus::Running;
                    self.role_label = Some(match *stage_index {
                        0 => "Generator Draft".to_string(),
                        1 => "Auditor Critique".to_string(),
                        _ => "Synthesizer Consensus".to_string(),
                    });
                }
                if self.stage_index == Some(*stage_index) {
                    self.text.push_str(text);
                }
            }
            ArenaStreamAction::SetStageStatus {
                stage_index,
                status,
            } => {
                if self.stage_index.is_none() {
                    self.active = true;
                    self.stage_index = Some(*stage_index);
                    self.role_label = Some(match *stage_index {
                        0 => "Generator Draft".to_string(),
                        1 => "Auditor Critique".to_string(),
                        _ => "Synthesizer Consensus".to_string(),
                    });
                }
                if self.stage_index == Some(*stage_index) {
                    self.generating = false;
                    self.status = status.clone();
                    match status {
                        StageStatus::Completed { .. } => self.footer = "complete".to_string(),
                        StageStatus::Failed { error } => self.footer = format!("failed: {error}"),
                        StageStatus::Running => self.footer = "generating…".to_string(),
                    }
                }
            }
            ArenaStreamAction::HumanIntervention {
                stage_index,
                feedback,
            } => {
                // Record the human guidance on the stage the pipeline is about
                // to resume into. The badge persists so it stays visible even
                // after the stage completes.
                if self.stage_index == Some(*stage_index) {
                    self.human_intervention = Some(feedback.clone());
                }
            }
        }
    }

    /// Whether the pulse badge should currently be lit.
    pub fn is_generating(&self) -> bool {
        self.generating
    }
}

/// Shared, interior-mutable state for one stage bubble card.
///
/// Holds both the pure `StageState` and the GTK widget handles. The card is
/// always manipulated on the main thread, so `RefCell` (not `Mutex`) is
/// sufficient and avoids any cross-thread locking on the UI path.
struct CardState {
    /// Pure state machine backing the rendering.
    model: RefCell<StageState>,
    /// Card container (vertical box).
    card: gtk::Box,
    /// The text view backing the streaming buffer.
    text_view: gtk::TextView,
    /// The pulse badge widget.
    pulse: gtk::Label,
    /// The status footer label.
    status_label: gtk::Label,
    /// Current auto-scroll mode.
    #[allow(dead_code)]
    scroll_mode: ScrollMode,
}

/// One live stage bubble card.
///
/// Created empty (no stage selected) and populated via `apply_action`. The card
/// is safe to construct off-screen; all GTK mutations must happen on the GTK
/// main thread.
#[derive(Clone)]
pub struct StageBubbleCard {
    state: Rc<RefCell<CardState>>,
}

impl StageBubbleCard {
    /// Create an empty bubble card with no stage assigned yet.
    pub fn new() -> Self {
        let (card, text_view, pulse, status_label) = build_card();

        let state = Rc::new(RefCell::new(CardState {
            model: RefCell::new(StageState::default()),
            card: card.clone(),
            text_view,
            pulse,
            status_label,
            scroll_mode: ScrollMode::Sticky,
        }));

        Self { state }
    }

    /// The underlying GTK widget, for embedding into a layout.
    pub fn widget(&self) -> gtk::Box {
        self.state.borrow().card.clone()
    }

    /// Apply a single UI action to this card.
    ///
    /// This is the only method the bridge calls. It updates the pure state,
    /// then mirrors the result onto the GTK widgets (header, buffer, pulse
    /// badge, footer) and re-sticks auto-scroll as needed. It must always run
    /// on the GTK main thread.
    pub fn apply_action(&self, action: &ArenaStreamAction) {
        // Snapshot the action's effect on the pure model, then release the
        // borrow before rendering (render needs &mut self). We clone the
        // `StageState` value itself (not the `Ref`) so the snapshot outlives
        // the borrow of `self.state`.
        let model_snapshot = {
            let s = self.state.borrow_mut();
            s.model.borrow_mut().apply(action);
            let snapshot = (*s.model.borrow()).clone();
            drop(s);
            snapshot
        };
        self.render(&model_snapshot);
    }

    /// Mirror the pure state onto the GTK widgets.
    ///
    /// Takes `&self` only; the auto-scroll mode is updated through the
    /// interior-mutable `RefCell`, so no `&mut self` is required and this can
    /// be called from `apply_action`. The `model` borrow stays read-only
    /// throughout so we never hold two `RefMut`s on the same cell.
    fn render(&self, model: &StageState) {
        // Header: stage tag, role, model id.
        if let Some(old) = self.state.borrow().card.first_child() {
            self.state.borrow().card.remove(&old);
        }
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_halign(gtk::Align::Fill);

        let role_name = model.role_label.as_deref().unwrap_or("STAGE");
        let (role_caps, color, css_class) = if role_name.contains("Generator") {
            ("GENERATOR", "#38bdf8", "generator")
        } else if role_name.contains("Auditor") {
            ("AUDITOR", "#fbbf24", "auditor")
        } else if role_name.contains("Synthesizer") || role_name.contains("Consensus") {
            ("SYNTHESIZER", "#34d399", "synthesizer")
        } else {
            ("AI", "#38bdf8", "generator")
        };

        let title_label = gtk::Label::new(None);
        title_label.set_markup(&format!(
            "<span foreground='{}'><b>{}</b></span> <span foreground='{}'>says:</span>",
            color, role_caps, color
        ));
        title_label.set_halign(gtk::Align::Start);

        let stage_num = model.stage_index.map(|i| i + 1).unwrap_or(0);
        let stage_badge = gtk::Label::new(Some(&format!("#{stage_num}")));
        stage_badge.set_css_classes(&["badge"]);

        header.append(&stage_badge);
        header.append(&title_label);

        // Human intervention badge
        if let Some(feedback) = &model.human_intervention {
            let badge = gtk::Label::new(Some("👤 Chime-In Added"));
            badge.add_css_class("badge");
            badge.add_css_class("human-intervention-badge");
            let tooltip: String = if feedback.trim().is_empty() {
                "Human resumed without changes".to_string()
            } else {
                format!("Human guidance:\n{feedback}")
            };
            badge.set_tooltip_text(Some(&tooltip));
            header.append(&badge);
        }

        let model_label = gtk::Label::new(None);
        if let Some(m_id) = &model.model_id {
            model_label.set_markup(&format!(
                "<span alpha='65%' size='small'>({})</span>",
                glib::markup_escape_text(m_id)
            ));
        }
        model_label.set_hexpand(true);
        model_label.set_halign(gtk::Align::End);

        header.append(&model_label);
        self.state.borrow().card.prepend(&header);

        // Apply role accent border
        let s = self.state.borrow();
        s.card.remove_css_class("generator");
        s.card.remove_css_class("auditor");
        s.card.remove_css_class("synthesizer");
        s.card.add_css_class(css_class);

        // Pulse badge.
        if model.is_generating() {
            self.state.borrow().pulse.add_css_class("pulse-active");
        } else {
            self.state.borrow().pulse.remove_css_class("pulse-active");
        }

        // Buffer update.
        {
            let state = self.state.borrow();
            state.text_view.buffer().set_text(&model.text);
        }

        // Footer.
        self.state.borrow().status_label.set_text(&model.footer);
    }
}

/// Build the card skeleton as a full-width chat bubble.
fn build_card() -> (
    gtk::Box,
    gtk::TextView,
    gtk::Label,
    gtk::Label,
) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("arena-bubble-ai");
    card.set_margin_start(8);
    card.set_margin_end(8);
    card.set_margin_top(6);
    card.set_margin_bottom(16);

    // Header row (placeholder; replaced on stage start).
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let stage_tag = gtk::Label::new(Some("#0"));
    stage_tag.set_css_classes(&["badge"]);
    let role = gtk::Label::new(Some("—"));
    role.add_css_class("title-2");
    header.append(&stage_tag);
    header.append(&role);
    card.prepend(&header);

    // Pulse badge (top-right of the header).
    let pulse = gtk::Label::new(Some("●"));
    pulse.set_halign(gtk::Align::End);
    pulse.set_css_classes(&["dim-label"]);
    header.append(&pulse);

    // Streaming text view expanding naturally without inner scrollbox.
    let text_view = gtk::TextView::new();
    text_view.set_editable(false);
    text_view.set_wrap_mode(gtk::WrapMode::WordChar);
    text_view.set_monospace(false);
    text_view.set_pixels_above_lines(2);
    text_view.set_pixels_below_lines(2);
    card.append(&text_view);

    // Status footer.
    let status_label = gtk::Label::new(Some("idle"));
    status_label.set_halign(gtk::Align::Start);
    status_label.set_css_classes(&["dim-label"]);
    status_label.set_margin_start(4);
    card.append(&status_label);

    (card, text_view, pulse, status_label)
}
