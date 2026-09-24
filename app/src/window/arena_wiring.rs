//! SWAI — Debate Arena window wiring.
//!
//! Thin module that owns the construction + live-subscription logic for the
//! Arena window, so `header.rs` stays a one-liner.
//!
//! Responsibilities:
//!  1. Construct `crate::arena::ArenaWindow::new()`.
//!  2. Bridge the live Council broadcast (`ProxyState.events_rx`) onto the GTK
//!     main loop via `ArenaWindow::observe_debate`.
//!  3. Present the window, reusing a cached handle so clicking the menu item
//!     raises/restores the existing window instead of spawning a new one.
//!
//! ## State management — single-instance window reuse
//!
//! The cached handle lives on `MainWindow` as `debate_arena:
//! Rc<RefCell<Option<ArenaWindow>>>`. On menu click: if `None`, construct +
//! subscribe + present and store the handle; if `Some`, call `.present()` to
//! raise/restore the existing window.
//! ## Re-subscribing on a new debate
//!
//! `ProxyState.events_rx` is replaced by the router when a new council debate
//! begins (see `core/src/proxy/router.rs`). We re-subscribe on every menu
//! click: if the cached window already has a live bridge, we tear it down and
//! start a new one from the current `events_rx`. This keeps the Arena window
//! in sync with whichever debate is most recently started without needing an
//! external poller or a hook inside `set_events_receiver`.
//!
//! The subscription logic itself lives in `ArenaWindow::observe_debate`, which
//! already implements the background→main-thread bridge (background thread
//! drains the tokio broadcast; the GTK main thread polls an mpsc channel via
//! `glib::timeout_add_local`). This module only *invokes* that bridge — it
//! never moves a GTK handle across a thread. `ProxyState.events_rx` is behind
//! `Arc<Mutex<...>>`; we lock it only long enough to read the `Option`, then
//! drop the lock before spawning the bridge.
//!
//! All GTK construction/presentation happens on the GTK main thread. This
//! module is always called from the main thread (the `debate_arena` action
//! closure), so no GTK handle ever crosses a thread boundary here.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use gtk4 as gtk;
use gtk::prelude::*;

use swai_core::proxy::ProxyState;

use crate::arena::ArenaWindow;

/// Type alias for the cached Arena-window handle kept on `MainWindow`.
pub type DebateArenaCache = Rc<RefCell<Option<ArenaWindow>>>;

/// Open (or raise) the Debate Arena window.
///
/// Called from the GTK main thread via the `debate_arena` action. If the
/// cached handle is `None`, construct the window, subscribe it to the live
/// Council broadcast (if a debate is in flight), and present it. If the handle
/// already exists, just present it to raise/restore the existing window.
pub fn open_debate_arena(cache: &DebateArenaCache, proxy_state: Option<Arc<Mutex<ProxyState>>>) {
    let mut guard = cache.borrow_mut();

    if let Some(existing) = guard.as_ref() {
        existing.set_proxy_state(proxy_state.clone());
        existing.present();
        subscribe_to_debate(existing, proxy_state.clone());
        return;
    }

    // Construct a fresh window on the GTK main thread.
    let arena = ArenaWindow::new();
    arena.set_proxy_state(proxy_state.clone());
    arena.present();
    subscribe_to_debate(&arena, proxy_state);

    *guard = Some(arena);
}

/// Subscribe the Arena window to the live Council broadcast.
///
/// Reads `ProxyState.events_rx` behind a short-lived lock. If a receiver is
/// present (a debate is in flight), hand it to `ArenaWindow::observe_debate`,
/// which spawns the background→main-thread bridge. If `None`, the window is
/// simply left showing its empty-state placeholder so the user can browse
/// saved debates.
pub fn subscribe_to_debate(arena: &ArenaWindow, proxy_state: Option<Arc<Mutex<ProxyState>>>) {
    let rx = match proxy_state {
        Some(ref ps) => {
            // Lock only long enough to read the `Option`, then drop the lock
            // before spawning the bridge (which runs on a background thread).
            ps.lock()
                .map(|s| s.events_rx.clone())
                .unwrap_or_else(|poisoned| poisoned.into_inner().events_rx.clone())
        }
        None => None,
    };

    if let Some(events_rx) = rx {
        arena.observe_debate(events_rx);
    }
}

/// Automatically sync active debate broadcast if the Arena window is open/visible.
pub fn sync_active_debate(arena: &ArenaWindow, proxy_state: Option<Arc<Mutex<ProxyState>>>) {
    if arena.is_visible() {
        subscribe_to_debate(arena, proxy_state);
    }
}

/// Intercept global shortcut keys in the capture phase to open Arena and prevent GTK Inspector.
pub fn attach_arena_shortcut(
    widget: &impl IsA<gtk::Widget>,
    debate_arena: &DebateArenaCache,
    proxy_state: Option<Arc<Mutex<ProxyState>>>,
) {
    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    let da_key = debate_arena.clone();
    let ps_key = proxy_state;
    key_ctrl.connect_key_pressed(move |_ctrl, keyval, _code, state| {
        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
            let is_shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let is_d = keyval == gtk::gdk::Key::d || keyval == gtk::gdk::Key::D;
            let is_i = keyval == gtk::gdk::Key::i || keyval == gtk::gdk::Key::I;
            let is_a = keyval == gtk::gdk::Key::a || keyval == gtk::gdk::Key::A;
            if (is_shift && (is_d || is_i || is_a)) || is_d {
                open_debate_arena(&da_key, ps_key.clone());
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    });
    widget.add_controller(key_ctrl);
}
