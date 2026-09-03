#![allow(dead_code, unused)]
//! SWAI — ArenaWindow: GTK4/Libadwaita Gemini-style debate arena window.
//!
//! Provides a saved debates sidebar, a unified auto-scrolling chat stream,
//! a persistent bottom Chime In bar, and rich color-coded role cards.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use gtk4 as gtk;
use crate::arena::history::populate_debate_list;
use swai_core::council::{CouncilEvent, DebateTranscript};
use swai_core::proxy::ProxyState;

use super::chime_in::ChimeIn;
use super::history;
use super::stream::StageBubbleCard;
use super::types::{event_to_action, ArenaStreamAction};
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
    /// Stack switcher between live stream and saved transcript view.
    stack: gtk::Stack,
    /// Right panel containing the transcript view.
    transcript_view: gtk::ScrolledWindow,
    /// The "Chime In" human-in-the-loop control, shown in the header while a
    /// live debate runs.
    chime_in: ChimeIn,
    /// Currently loaded transcript (if any).
    current_transcript: Rc<RefCell<Option<DebateTranscript>>>,
    /// Pointer id of currently observed receiver to prevent duplicate bridges.
    observed_rx: Rc<RefCell<Option<usize>>>,
    /// Optional ProxyState handle for triggering instant model generation aborts.
    proxy_state: Rc<RefCell<Option<Arc<Mutex<ProxyState>>>>>,
}

impl ArenaWindow {
    /// Create a new ArenaWindow.
    pub fn new() -> Self {
        let proxy_state = Rc::new(RefCell::new(None));
        let (widget, debate_list, live_panel, live_empty, stack, transcript_view, chime_in) =
            build_window(&proxy_state);

        let stage_cards = Rc::new(RefCell::new(Vec::new()));
        let current_transcript = Rc::new(RefCell::new(None::<DebateTranscript>));
        let observed_rx = Rc::new(RefCell::new(None));

        wire_sidebar(&debate_list, &current_transcript, &stack, &transcript_view);

        Self {
            widget,
            debate_list,
            live_panel,
            stage_cards,
            live_empty,
            stack,
            transcript_view,
            chime_in,
            current_transcript,
            observed_rx,
            proxy_state,
        }
    }

    /// Attach proxy state handle to enable immediate interruption on chime-in.
    pub fn set_proxy_state(&self, proxy_state: Option<Arc<Mutex<ProxyState>>>) {
        *self.proxy_state.borrow_mut() = proxy_state;
    }

    /// Present the window (make it visible and raise it).
    pub fn present(&self) {
        self.widget.present();
    }

    /// Check if the window is currently visible.
    pub fn is_visible(&self) -> bool {
        self.widget.is_visible()
    }

    /// Start observing a live debate. Spawns a background bridge thread
    /// draining the broadcast channel and forwarding plain actions onto GTK main loop.
    pub fn observe_debate(
        &self,
        events_rx: std::sync::Arc<std::sync::Mutex<tokio::sync::broadcast::Receiver<CouncilEvent>>>,
    ) {
        let rx_id = std::sync::Arc::as_ptr(&events_rx) as usize;
        if *self.observed_rx.borrow() == Some(rx_id) {
            return;
        }
        *self.observed_rx.borrow_mut() = Some(rx_id);

        let mut rx = match events_rx.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut rx = rx.resubscribe();

        let (tx, rx_ui) = std::sync::mpsc::channel::<ArenaStreamAction>();

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
        let stack = self.stack.clone();
        let debate_list = self.debate_list.clone();
        let _ = glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            loop {
                match rx_ui.try_recv() {
                    Ok(action) => {
                        stack.set_visible_child_name("live");
                        apply_action_to_panel(&panel, &cards, &empty, &action);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        populate_debate_list(&debate_list);
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

        // Replace content with transcript view and show saved stack.
        self.transcript_view
            .set_child(Some(&view::create_transcript_view(&transcript)));
        self.stack.set_visible_child_name("saved");

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

/// Apply a UI action to the live panel, creating cards sequentially for a continuous chat flow.
fn apply_action_to_panel(
    panel: &gtk::Box,
    cards: &Rc<RefCell<Vec<Option<StageBubbleCard>>>>,
    empty: &gtk::Label,
    action: &ArenaStreamAction,
) {
    if empty.parent().is_some() {
        panel.remove(empty);
    }

    let mut cards_guard = cards.borrow_mut();
    let card = match action {
        ArenaStreamAction::StageStarted { .. } => {
            let new_card = StageBubbleCard::new();
            panel.append(&new_card.widget());
            cards_guard.push(Some(new_card.clone()));
            new_card
        }
        _ => {
            if let Some(Some(last)) = cards_guard.last() {
                last.clone()
            } else {
                let new_card = StageBubbleCard::new();
                panel.append(&new_card.widget());
                cards_guard.push(Some(new_card.clone()));
                new_card
            }
        }
    };
    drop(cards_guard);

    card.apply_action(action);
}

/// Build the full window UI, returning all widgets the struct needs to hold.
fn build_window(
    proxy_state: &Rc<RefCell<Option<Arc<Mutex<ProxyState>>>>>,
) -> (
    gtk::ApplicationWindow,
    gtk::ListBox,
    gtk::Box,
    gtk::Label,
    gtk::Stack,
    gtk::ScrolledWindow,
    ChimeIn,
) {
    let widget = gtk::ApplicationWindow::builder()
        .title("Arena — Debate")
        .default_width(1024)
        .default_height(720)
        .hide_on_close(true)
        .build();

    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_ctrl.connect_key_pressed(move |_, keyval, _, state| {
        let mask = gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK;
        if state.contains(mask) && matches!(keyval, gtk::gdk::Key::d | gtk::gdk::Key::D | gtk::gdk::Key::i | gtk::gdk::Key::I) {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    widget.add_controller(key_ctrl);

    let header = gtk::HeaderBar::new();
    header.set_show_title_buttons(true);

    let new_btn = gtk::Button::builder().label("Live Stream").css_classes(["suggested-action"]).margin_end(6).build();
    let save_btn = gtk::Button::builder().label("Save Current").margin_end(6).build();
    header.pack_start(&new_btn);
    header.pack_end(&save_btn);
    widget.set_titlebar(Some(&header));

    let chime_in = ChimeIn::new();
    chime_in_on_decision(chime_in.clone());

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

    // Content area with Stack.
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    // Live stream panel (Page 1)
    let live_panel = gtk::Box::new(gtk::Orientation::Vertical, 12);
    live_panel.set_css_classes(&["arena-stream-container"]);
    live_panel.set_halign(gtk::Align::Fill);

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
    stack.add_named(&live_scroll, Some("live"));

    // Transcript view (Page 2)
    let transcript_view = gtk::ScrolledWindow::new();
    transcript_view.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    stack.add_named(&transcript_view, Some("saved"));

    stack.set_visible_child_name("live");

    // Persistent Gemini-style Chime In Input Bar
    let bottom_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bottom_bar.add_css_class("arena-bottom-bar");

    let entry = gtk::Entry::builder()
        .placeholder_text("Type guidance to Chime In on the debate and press Enter...")
        .css_classes(["arena-bottom-entry"])
        .hexpand(true)
        .build();

    let send_btn = gtk::Button::builder().label("Chime In ➤").css_classes(["suggested-action"]).build();
    bottom_bar.append(&entry);
    bottom_bar.append(&send_btn);

    // Wire submit action
    let entry_clone = entry.clone();
    let chime_clone = chime_in.clone();
    let panel_clone = live_panel.clone();
    let proxy_for_chime = Rc::clone(proxy_state);
    let submit_guidance = std::rc::Rc::new(move || {
        let text = entry_clone.text().trim().to_string();
        if !text.is_empty() {
            if let Some(ref ps) = *proxy_for_chime.borrow() {
                if let Ok(state) = ps.lock() {
                    state.abort_active_stage(Some(text.clone()));
                }
            }
            chime_clone.inject(&text);
            let bubble = view::create_human_bubble(&text, true);
            panel_clone.append(&bubble);
            entry_clone.set_text("");
        }
    });

    let submit_for_btn = std::rc::Rc::clone(&submit_guidance);
    send_btn.connect_clicked(move |_| {
        submit_for_btn();
    });

    let submit_for_entry = std::rc::Rc::clone(&submit_guidance);
    entry.connect_activate(move |_| {
        submit_for_entry();
    });

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.append(&stack);
    content.append(&bottom_bar);
    main_box.append(&content);

    let stack_for_new = stack.clone();
    new_btn.connect_clicked(move |_| {
        stack_for_new.set_visible_child_name("live");
    });

    widget.set_child(Some(&main_box));

    (
        widget,
        debate_list,
        live_panel,
        live_empty,
        stack,
        transcript_view,
        chime_in,
    )
}

/// Wire the sidebar selection handler to load debates into the transcript view.
fn wire_sidebar(
    debate_list: &gtk::ListBox,
    current_transcript: &Rc<RefCell<Option<DebateTranscript>>>,
    stack: &gtk::Stack,
    transcript_view: &gtk::ScrolledWindow,
) {
    let ct_clone = Rc::clone(current_transcript);
    let debate_list = debate_list.clone();
    let stack_clone = stack.clone();
    let tv_clone = transcript_view.clone();
    debate_list.connect_row_activated(move |_listbox, row| {
        let label_text = row
            .first_child()
            .and_then(|w| w.downcast::<gtk::Label>().ok())
            .map(|label| label.text().to_string())
            .unwrap_or_default();

        if let Ok(transcript) = history::load_transcript(&label_text) {
            *ct_clone.borrow_mut() = Some(transcript.clone());
            tv_clone.set_child(Some(&view::create_transcript_view(&transcript)));
            stack_clone.set_visible_child_name("saved");
            tracing::info!("Loaded debate: {label_text}");
        } else {
            tracing::error!("Failed to load debate: {label_text}");
        }
    });
}

/// Wire the "Chime In" widget's decision handler.
fn chime_in_on_decision(chime: ChimeIn) {
    chime.on_decision(|decision| match decision {
        super::chime_in::ChimeInDecision::Inject { guidance } => {
            tracing::info!("Chime In: injecting guidance ({}) at next stage gate", guidance.len());
        }
        super::chime_in::ChimeInDecision::Skip => {
            tracing::info!("Chime In: resuming without changes at next stage gate");
        }
    });
}
