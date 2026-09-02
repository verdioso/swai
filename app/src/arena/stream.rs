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
const CARD_WIDTH: i32 = 320;

/// Whether auto-scroll is currently pinned to the bottom of the view.
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
                if self.stage_index == Some(*stage_index) {
                    self.text.push_str(text);
                }
            }
            ArenaStreamAction::SetStageStatus {
                stage_index,
                status,
            } => {
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
        let (card, text_view, scrolled, pulse, status_label) = build_card();

        let state = Rc::new(RefCell::new(CardState {
            model: RefCell::new(StageState::default()),
            card: card.clone(),
            text_view: text_view.clone(),
            pulse: pulse.clone(),
            status_label: status_label.clone(),
            scroll_mode: ScrollMode::Sticky,
        }));

        // Track manual scrolls: if the user moves the viewport away from the
        // bottom, release auto-scroll; snapping back re-engages it.
        let sc_state = Rc::clone(&state);
        let adj = scrolled.vadjustment();
        adj.clone().connect_changed(move |adjustment| {
            let max = adj.upper() - adj.page_size();
            let at_bottom = adjustment.value() >= max - 1.0;
            let mut sc_state = sc_state.borrow_mut();
            sc_state.scroll_mode = if at_bottom {
                ScrollMode::Sticky
            } else {
                ScrollMode::Manual
            };
        });

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
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_halign(gtk::Align::Fill);

        let stage_tag = gtk::Label::new(Some(&format!(
            "#{}",
            model.stage_index.map(|i| i + 1).unwrap_or(0)
        )));
        stage_tag.set_css_classes(&["badge"]);

        let role = gtk::Label::new(model.role_label.as_deref());
        role.add_css_class("title-2");

        let model_label = gtk::Label::new(model.model_id.as_deref());
        model_label.set_css_classes(&["dim-label"]);
        model_label.set_hexpand(true);
        model_label.set_halign(gtk::Align::End);

        header.append(&stage_tag);
        header.append(&role);
        header.append(&model_label);
        self.state.borrow().card.prepend(&header);

        // Pulse badge.
        if model.is_generating() {
            self.state.borrow().pulse.add_css_class("pulse-active");
        } else {
            self.state.borrow().pulse.remove_css_class("pulse-active");
        }

        // Buffer + auto-scroll. Re-stick while generating and, when sticky,
        // pin the viewport to the newly appended text so the reader always sees
        // the latest tokens without having to scroll.
        {
            let state = self.state.borrow();
            state.text_view.buffer().set_text(&model.text);
        }
        if model.is_generating() {
            let mut state = self.state.borrow_mut();
            state.scroll_mode = ScrollMode::Sticky;
            // Scroll the text view so the end of the buffer is visible. This is
            // the auto-scroll: it runs on the main thread via the buffered text
            // update above, so there is no race with the bridge thread.
            let mut end = state.text_view.buffer().end_iter();
            state
                .text_view
                .scroll_to_iter(&mut end, 0.0, true, 0.0, 1.0);
        }

        // Footer.
        self.state.borrow().status_label.set_text(&model.footer);
    }
}

/// Build the card skeleton, returning all the widgets the card needs to hold
/// directly (no tree searching required).
fn build_card() -> (
    gtk::Box,
    gtk::TextView,
    gtk::ScrolledWindow,
    gtk::Label,
    gtk::Label,
) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.set_width_request(CARD_WIDTH);
    card.add_css_class("card");
    card.add_css_class("arena-bubble");

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

    // Streaming text view inside a scroll container.
    let text_view = gtk::TextView::new();
    text_view.set_editable(false);
    text_view.set_wrap_mode(gtk::WrapMode::WordChar);
    text_view.set_monospace(true);
    text_view.set_pixels_above_lines(2);
    text_view.set_pixels_below_lines(2);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scrolled.set_min_content_height(80);
    scrolled.set_max_content_height(320);
    scrolled.set_child(Some(&text_view));
    card.append(&scrolled);

    // Status footer.
    let status_label = gtk::Label::new(Some("idle"));
    status_label.set_halign(gtk::Align::Start);
    status_label.set_css_classes(&["dim-label"]);
    card.append(&status_label);

    (card, text_view, scrolled, pulse, status_label)
}
