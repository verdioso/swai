//! SWAI — Arena "Chime In" human-in-the-loop interruption widget.
//!
//! A compact action-bar control that lets a user interrupt a running (or idle)
//! council debate between stages. Clicking **Chime In** pauses the pipeline and
//! reveals a lightweight collapsible drawer holding a multiline guidance editor
//! plus two decisions:
//!
//! * **Inject & Resume** — pause the pipeline, then resume the next stage with
//!   the typed guidance prepended as an authoritative `HumanAuditor` turn.
//! * **Skip / Resume** — pause the pipeline, then resume the next stage
//!   unchanged.
//!
//! The widget is deliberately GTK-only (no core coupling): it emits a
//! [`ChimeInDecision`] through a user-supplied closure when the user acts, so
//! the owning window decides how to translate it into a pause-controller call.
//! This keeps the widget trivially testable and transport-agnostic.
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

/// A decision produced by the "Chime In" drawer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChimeInDecision {
    /// Resume the next stage with the supplied guidance injected.
    Inject { guidance: String },
    /// Resume the next stage without applying any changes.
    Skip,
}

/// The kind of decision the user selects in the drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAction {
    /// Inject & Resume: apply the guidance.
    Inject,
    /// Skip / Resume: ignore the guidance.
    Skip,
}

/// Build a [`ChimeInDecision`] from the guidance text and the action the user
/// selected. Pure function so the decision logic is testable without GTK.
pub fn build_decision(guidance: &str, action: DecisionAction) -> ChimeInDecision {
    match action {
        DecisionAction::Inject => ChimeInDecision::Inject {
            guidance: guidance.to_string(),
        },
        DecisionAction::Skip => ChimeInDecision::Skip,
    }
}

/// "Chime In" action-bar control.
///
/// Owns a toggle button and a collapsible drawer. Construct with
/// [`ChimeIn::new`], attach a handler with [`ChimeIn::on_decision`], place the
/// returned widget into the layout, and call [`ChimeIn::show`] to present the
/// drawer when the pipeline reaches a stage gate.
#[derive(Clone)]
pub struct ChimeIn {
    inner: Rc<RefCell<Inner>>,
}

/// Interior-mutable state backing a [`ChimeIn`] widget.
struct Inner {
    /// The action-bar toggle button.
    button: gtk::Button,
    /// The collapsible drawer (hidden until the user chimes in).
    drawer: gtk::Box,
    /// The multiline guidance editor.
    guidance: gtk::Entry,
    /// The user-supplied decision handler, if any.
    on_decision: Option<Box<dyn Fn(ChimeInDecision)>>,
}

impl ChimeIn {
    /// Create a new "Chime In" control.
    ///
    /// The button starts in the "Chime In" (ready) state and the drawer is
    /// hidden. Attach a decision handler with [`ChimeIn::on_decision`] before
    /// showing the widget.
    pub fn new() -> Self {
        let button = gtk::Button::builder()
            .label("Chime In")
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("suggested-action");

        let (drawer, guidance) = build_drawer();

        // Toggle the drawer open/closed.
        let inner = Rc::new(RefCell::new(Inner {
            button: button.clone(),
            drawer: drawer.clone(),
            guidance,
            on_decision: None,
        }));

        let widget_inner = Rc::clone(&inner);
        button.connect_clicked(move |_btn| {
            let me = widget_inner.borrow();
            me.drawer.set_visible(!me.drawer.is_visible());
            let open = me.drawer.is_visible();
            me.button
                .set_label(if open { "Chime In…" } else { "Chime In" });
        });

        // Inject & Resume button.
        let inject_btn = gtk::Button::builder()
            .label("Inject & Resume")
            .valign(gtk::Align::Center)
            .build();
        inject_btn.add_css_class("suggested-action");
        let inject_inner = Rc::clone(&inner);
        inject_btn.connect_clicked(move |_| {
            let me = inject_inner.borrow();
            let guidance = me.guidance.text().to_string();
            if let Some(handler) = &me.on_decision {
                handler(ChimeInDecision::Inject { guidance });
            }
            me.drawer.set_visible(false);
            me.button.set_label("Chime In");
            me.guidance.set_text("");
        });

        // Skip / Resume button.
        let skip_btn = gtk::Button::builder()
            .label("Skip / Resume")
            .valign(gtk::Align::Center)
            .build();
        skip_btn.add_css_class("destructive-action");
        let skip_inner = Rc::clone(&inner);
        skip_btn.connect_clicked(move |_| {
            let me = skip_inner.borrow();
            if let Some(handler) = &me.on_decision {
                handler(ChimeInDecision::Skip);
            }
            me.drawer.set_visible(false);
            me.button.set_label("Chime In");
            me.guidance.set_text("");
        });

        drawer.append(&inject_btn);
        drawer.append(&skip_btn);

        Self { inner }
    }

    /// The action-bar toggle button, for embedding into a layout.
    pub fn button(&self) -> gtk::Button {
        self.inner.borrow().button.clone()
    }

    /// Attach a handler invoked when the user makes a decision.
    ///
    /// The closure receives the [`ChimeInDecision`]. It is the owner's
    /// responsibility to translate that into a pause-controller call.
    pub fn on_decision<F>(&self, handler: F)
    where
        F: Fn(ChimeInDecision) + 'static,
    {
        self.inner.borrow_mut().on_decision = Some(Box::new(handler));
    }

    /// Present the drawer and focus the guidance editor.
    ///
    /// Call this when the pipeline reaches a stage gate.
    pub fn show(&self) {
        let me = self.inner.borrow();
        me.drawer.set_visible(true);
        me.button.set_label("Chime In…");
        me.guidance.grab_focus();
    }

    /// Hide the drawer and restore the ready button label.
    pub fn dismiss(&self) {
        let me = self.inner.borrow();
        me.drawer.set_visible(false);
        me.button.set_label("Chime In");
    }

    /// Whether the drawer is currently open.
    pub fn is_open(&self) -> bool {
        self.inner.borrow().drawer.is_visible()
    }
}

/// Build the collapsible drawer: a guidance editor row plus a hint row.
fn build_drawer() -> (gtk::Box, gtk::Entry) {
    let drawer = gtk::Box::new(gtk::Orientation::Vertical, 6);
    drawer.add_css_class("card");
    drawer.add_css_class("chime-in-drawer");
    drawer.set_margin_start(0);
    drawer.set_margin_end(0);
    drawer.set_margin_top(6);
    drawer.set_margin_bottom(6);

    let guidance = gtk::Entry::new();
    guidance.set_editable(true);
    guidance.set_placeholder_text(Some(
        "Type guidance for the Council (e.g., 'Make sure to handle edge cases and use serde')...",
    ));
    guidance.set_valign(gtk::Align::Start);
    guidance.set_width_chars(40);
    drawer.append(&guidance);

    // A small hint row describing the two decisions.
    let hint = gtk::Label::new(Some(
        "Inject & Resume prepends your guidance as authoritative feedback; Skip / Resume resumes unchanged.",
    ));
    hint.set_css_classes(&["dim-label"]);
    hint.set_xalign(0.0);
    hint.set_justify(gtk::Justification::Fill);
    drawer.append(&hint);

    (drawer, guidance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_decision_inject_carries_guidance() {
        let decision = build_decision("Handle edge cases", DecisionAction::Inject);
        assert_eq!(
            decision,
            ChimeInDecision::Inject {
                guidance: "Handle edge cases".into()
            }
        );
    }

    #[test]
    fn test_build_decision_skip_ignores_guidance() {
        let decision = build_decision("ignored", DecisionAction::Skip);
        assert_eq!(decision, ChimeInDecision::Skip);
    }

    #[test]
    fn test_build_decision_inject_with_empty_guidance() {
        // An empty guidance with Inject still carries the (empty) string.
        let decision = build_decision("", DecisionAction::Inject);
        assert_eq!(
            decision,
            ChimeInDecision::Inject {
                guidance: String::new()
            }
        );
    }

    #[test]
    fn test_decision_variant_equality() {
        assert_ne!(
            build_decision("x", DecisionAction::Inject),
            build_decision("x", DecisionAction::Skip)
        );
    }
}
