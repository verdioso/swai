#![allow(dead_code, unused)]
//! SWAI — ArenaWindow: GTK4/Libadwaita debate arena desktop window.
//!
//! Provides a sidebar for browsing saved debates, a live-stream panel for
//! watching a council debate as it generates, and a transcript view for
//! rendering finished debates.
//!
//! ## Live streaming bridge
//!
//! The background council pipeline broadcasts `CouncilEvent`s on a tokio
//! `broadcast` channel. Those events live on a non-GTK thread, so they cannot
//! touch GTK widgets directly. `window.rs` bridges them onto the GTK main loop
//! without ever moving a GTK handle across a thread boundary:
//!
//! 1. A background thread drains the tokio receiver and translates each event
//!    into a plain-data `ArenaStreamAction`.
//! 2. Actions are forwarded over an `mpsc` channel (actions are `Send`; GTK
//!    widgets never leave the main thread).
//! 3. The main thread polls that channel from a `glib::timeout_add_local`
//!    source, applying each action to the appropriate `StageBubbleCard`.
//!
//! Because the polling closure runs on the GTK main thread, GTK is only ever
//! touched there, and the main loop never blocks on the debate.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use swai_core::council::{CouncilEvent, DebateTranscript};

use super::chime_in::ChimeIn;
use super::history;
use super::stream::StageBubbleCard;
use super::types::{event_to_action, stage_index_of, ArenaStreamAction};
use super::view;

/// ArenaWindow: Libadwaita window with history sidebar, live stream panel, and
/// transcript view.
pub struct ArenaWindow {
    /// The GTK application window.
    widget: gtk::ApplicationWindow,
    /// Sidebar listbox showing saved debates.
    debate_list: gtk::ListBox,
    /// Live stream panel container (holds one bubble card per stage).
    live_panel: gtk::Box,
    /// Bubble cards, one per stage index, created lazily.
    stage_cards: Rc<RefCell<Vec<Option<StageBubbleCard>>>>,
    /// The empty-state placeholder in the live panel.
    live_empty: gtk::Label,
    /// Right panel containing the transcript view.
    transcript_view: gtk::ScrolledWindow,
    /// The "Chime In" human-in-the-loop control, shown in the header while a
    /// live debate runs.
    chime_in: ChimeIn,
    /// Currently loaded transcript (if any).
    current_transcript: Rc<RefCell<Option<DebateTranscript>>>,
}

impl ArenaWindow {
    /// Create a new ArenaWindow.
    pub fn new() -> Self {
        let (widget, debate_list, live_panel, live_empty, transcript_view, chime_in) =
            build_window();

        let stage_cards = Rc::new(RefCell::new(Vec::new()));
        let current_transcript = Rc::new(RefCell::new(None::<DebateTranscript>));

        wire_sidebar(&debate_list, &current_transcript);

        Self {
            widget,
            debate_list,
            live_panel,
            stage_cards,
            live_empty,
            transcript_view,
            chime_in,
            current_transcript,
        }
    }

    /// Present the window (make it visible and raise it).
    pub fn present(&self) {
        self.widget.present();
    }

    /// Start observing a live debate.
    ///
    /// `events_rx` is the tokio broadcast receiver for this debate's
    /// `CouncilEvent`s (the same one the proxy registers on its state). This
    /// spawns a background thread that drains the channel and forwards each
    /// translated action onto the GTK main loop. The live panel replaces the
    /// empty-state placeholder on the first action.
    ///
    /// ## Thread-safety model
    ///
    /// GTK widgets are not `Send`, so they must never cross a thread boundary.
    /// The bridge therefore sends only plain-data `ArenaStreamAction`s over an
    /// `mpsc` channel; the GTK widgets are owned by the main thread, which polls
    /// the channel on a `timeout_add_local` source. This guarantees no GTK call
    /// ever runs on the bridge thread and the main loop never blocks on the
    /// debate.
    pub fn observe_debate(
        &self,
        events_rx: std::sync::Arc<std::sync::Mutex<tokio::sync::broadcast::Receiver<CouncilEvent>>>,
    ) {
        // Take ownership of the receiver from the mutex. We own it for the
        // lifetime of the bridge thread.
        let mut rx = match events_rx.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // `resubscribe` returns a fresh receiver positioned at the latest value,
        // so no early events are missed.
        let mut rx = rx.resubscribe();

        // Data-only channel: actions are `Send` (plain data), so this is safe to
        // cross threads. GTK widgets stay on the main thread.
        let (tx, rx_ui) = std::sync::mpsc::channel::<ArenaStreamAction>();

        // Background thread: drain the tokio receiver, translate, forward.
        std::thread::spawn(move || {
            loop {
                // `try_recv` is synchronous and non-blocking, which is exactly
                // what we want on this dedicated bridge thread.
                match rx.try_recv() {
                    Ok(event) => {
                        if let Some(action) = event_to_action(&event) {
                            // If the send fails the UI went away; stop.
                            if tx.send(action).is_err() {
                                break;
                            }
                        }
                        // Terminal events end the loop after forwarding.
                        if matches!(event, CouncilEvent::PipelineCompleted { .. })
                            || matches!(event, CouncilEvent::PipelineFailed { .. })
                        {
                            break;
                        }
                    }
                    // Channel closed: debate finished.
                    Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
                    // Fell behind: skip to the latest and keep going.
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                    // No new events yet: brief yield, then poll again.
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                        std::thread::yield_now()
                    }
                }
            }
            // Drop the sender so the UI receiver sees EOF when the debate ends.
            drop(tx);
        });

        // Main-thread polling: drain anything pending, then re-arm after a short
        // interval. `ControlFlow::Continue` keeps the source alive; `Break`
        // removes it once the debate channel is closed.
        let cards = Rc::clone(&self.stage_cards);
        let panel = self.live_panel.clone();
        let empty = self.live_empty.clone();
        let _ = glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            loop {
                match rx_ui.try_recv() {
                    Ok(action) => apply_action_to_panel(&panel, &cards, &empty, &action),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    /// Load and display a debate by session ID.
    pub fn load_debate(&self, id: &str) -> Result<(), String> {
        let transcript = history::load_transcript(id)?;

        *self.current_transcript.borrow_mut() = Some(transcript.clone());

        // Replace content with transcript view.
        self.transcript_view
            .set_child(Some(&view::create_transcript_view(&transcript)));
        self.transcript_view.set_visible(true);

        Ok(())
    }

    /// Save the currently loaded debate to disk.
    pub fn save_current(&self) -> Result<std::path::PathBuf, String> {
        let transcript = self
            .current_transcript
            .borrow()
            .as_ref()
            .ok_or_else(|| "No debate loaded".to_string())?
            .clone();

        history::save_transcript(&transcript)
    }
}

/// Apply a UI action to the live panel, creating cards lazily per stage index.
fn apply_action_to_panel(
    panel: &gtk::Box,
    cards: &Rc<RefCell<Vec<Option<StageBubbleCard>>>>,
    empty: &gtk::Label,
    action: &ArenaStreamAction,
) {
    // Ensure the empty-state placeholder is removed on first real action.
    if panel.first_child().is_some_and(|c| c.is::<gtk::Label>()) {
        panel.remove(empty);
    }

    let stage_index = stage_index_of(action);
    let mut cards_guard = cards.borrow_mut();
    // Grow the vec to hold this stage index.
    while cards_guard.len() <= stage_index {
        cards_guard.push(None);
    }

    // Get or create the card for this stage.
    let card = if let Some(existing) = &cards_guard[stage_index] {
        existing.clone()
    } else {
        let new_card = StageBubbleCard::new();
        panel.append(&new_card.widget());
        let cloned = new_card.clone();
        cards_guard[stage_index] = Some(new_card);
        cloned
    };
    drop(cards_guard);

    card.apply_action(action);
}

/// Build the full window UI, returning all widgets the struct needs to hold.
fn build_window() -> (
    gtk::ApplicationWindow,
    gtk::ListBox,
    gtk::Box,
    gtk::Label,
    gtk::ScrolledWindow,
    ChimeIn,
) {
    let widget = gtk::ApplicationWindow::builder()
        .title("Arena — Debate")
        .default_width(1024)
        .default_height(720)
        .build();

    let header = gtk::HeaderBar::new();
    header.set_show_title_buttons(true);

    let new_btn = gtk::Button::builder().label("New Debate").build();
    new_btn.add_css_class("suggested-action");
    new_btn.set_margin_end(6);
    let save_btn = gtk::Button::builder().label("Save Current").build();
    save_btn.set_margin_end(6);
    header.pack_start(&new_btn);
    header.pack_end(&save_btn);
    widget.set_titlebar(Some(&header));

    // Chime In control (human-in-the-loop). Shown in the header; clicking it
    // opens a drawer with a guidance editor and Inject / Skip decisions.
    let chime_in = ChimeIn::new();
    let chime_in_button = chime_in.button();
    chime_in_button.set_margin_end(6);
    header.pack_end(&chime_in_button);
    // Placeholder decision handler: the owning app wires this to the pause
    // controller. Until then, log the decision so the widget is exercised.
    let chime_in_for_handler = chime_in.clone();
    chime_in_on_decision(chime_in_for_handler);

    let main_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);

    // Sidebar.
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.set_width_request(240);
    sidebar.add_css_class("view");

    let sidebar_header = gtk::Label::new(Some("Saved Debates"));
    sidebar_header.set_css_classes(&["heading-2"]);
    sidebar_header.set_margin_start(12);
    sidebar_header.set_margin_top(12);
    sidebar.append(&sidebar_header);

    let debate_list = gtk::ListBox::new();
    debate_list.set_selection_mode(gtk::SelectionMode::Single);
    debate_list.add_css_class("navigation-sidebar");
    populate_debate_list(&debate_list);

    let sidebar_scroll = gtk::ScrolledWindow::new();
    sidebar_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sidebar_scroll.set_child(Some(&debate_list));
    sidebar.append(&sidebar_scroll);
    main_box.append(&sidebar);

    // Content area.
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_hexpand(true);
    content.set_vexpand(true);

    // Live stream panel (right, top): shows bubble cards while streaming.
    let live_panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    live_panel.set_margin_start(12);
    live_panel.set_margin_end(12);
    live_panel.set_margin_top(8);
    live_panel.set_margin_bottom(8);
    live_panel.set_halign(gtk::Align::Fill);

    let live_title = gtk::Label::new(Some("Live Debate"));
    live_title.set_css_classes(&["heading-3"]);
    live_panel.prepend(&live_title);

    let live_empty = gtk::Label::new(Some(
        "A live debate will stream here as models generate responses.\n\nStart one via the Council API.",
    ));
    live_empty.set_justify(gtk::Justification::Center);
    live_empty.set_valign(gtk::Align::Center);
    live_empty.set_margin_start(12);
    live_empty.set_margin_end(12);
    live_empty.set_margin_top(48);
    live_empty.set_margin_bottom(48);
    live_empty.add_css_class("dim-label");
    live_panel.append(&live_empty);

    let live_scroll = gtk::ScrolledWindow::new();
    live_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    live_scroll.set_vexpand(true);
    live_scroll.set_child(Some(&live_panel));
    content.append(&live_scroll);

    // Transcript view (right, bottom), initially hidden.
    let transcript_view = gtk::ScrolledWindow::new();
    transcript_view.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    transcript_view.set_visible(false);
    content.append(&transcript_view);

    widget.set_child(Some(&main_box));

    (
        widget,
        debate_list,
        live_panel,
        live_empty,
        transcript_view,
        chime_in,
    )
}

/// Wire the sidebar selection handler to load debates into the transcript view.
fn wire_sidebar(
    debate_list: &gtk::ListBox,
    current_transcript: &Rc<RefCell<Option<DebateTranscript>>>,
) {
    let ct_clone = Rc::clone(current_transcript);
    // `connect_row_activated` requires a `'static` closure, so clone the listbox
    // handle into the closure rather than borrowing it.
    let debate_list = debate_list.clone();
    debate_list.connect_row_activated(move |_listbox, row| {
        // The row's label holds the debate id.
        let label_text = row
            .first_child()
            .and_then(|w| w.downcast::<gtk::Label>().ok())
            .map(|label| label.text().to_string())
            .unwrap_or_default();

        if let Ok(transcript) = history::load_transcript(&label_text) {
            *ct_clone.borrow_mut() = Some(transcript.clone());
            tracing::info!("Loaded debate: {label_text}");
        } else {
            tracing::error!("Failed to load debate: {label_text}");
        }
    });
}

/// Populate the debate listbox with saved debates.
fn populate_debate_list(listbox: &gtk::ListBox) {
    let debates = match history::list_debates() {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("Failed to list debates: {e}");
            return;
        }
    };

    for id in debates {
        let row = gtk::ListBoxRow::new();
        let label = gtk::Label::new(Some(&id));
        label.set_xalign(0.0);
        label.set_margin_start(12);
        label.set_margin_end(12);
        label.set_margin_top(6);
        label.set_margin_bottom(6);

        row.add_css_class("selectable");
        row.set_activatable(true);
        row.set_child(Some(&label));
        listbox.append(&row);
    }
}

/// Wire the "Chime In" widget's decision handler.
///
/// The pipeline's pause controller lives in the core backend (see
/// `core/src/council/barrier.rs`); wiring the widget's decisions to it is the
/// owning app's responsibility. Until then, log the decision so the widget is
/// exercised and the human-in-the-loop path remains reachable.
fn chime_in_on_decision(chime: ChimeIn) {
    chime.on_decision(|decision| match decision {
        super::chime_in::ChimeInDecision::Inject { guidance } => {
            tracing::info!(
                "Chime In: injecting guidance ({}) at next stage gate",
                guidance.len()
            );
        }
        super::chime_in::ChimeInDecision::Skip => {
            tracing::info!("Chime In: resuming without changes at next stage gate");
        }
    });
}
