//! SWAI — Council execution pause barrier ("Chime In" interruption system).
//!
//! `CouncilPauseController` is the shared coordination primitive that lets a
//! human interrupt a running (or idle) council pipeline between stages. The
//! pipeline stages call [`CouncilPauseController::await_stage_gate`] before
//! executing stage `N+1`; the call blocks until the human either resumes the
//! debate (optionally with injected guidance) or aborts it.
//!
//! ## Design
//!
//! The controller is deliberately transport-agnostic and GTK-free. It exposes
//! a plain-data API that the UI layer wraps:
//!
//! * [`CouncilPauseController::request_pause`] flags that a pause is requested
//!   and notifies anyone currently waiting at a gate.
//! * [`CouncilPauseController::await_stage_gate`] blocks the pipeline until a
//!   decision is made. It returns a [`PauseDecision`].
//! * [`CouncilPauseController::resume_with_feedback`] /
//!   [`CouncilPauseController::resume_without_changes`] /
//!   [`CouncilPauseController::abort`] are the three UI-facing decisions.
//!
//! Internally it uses a `tokio::sync::Notify` to wake waiters and a
//! `std::sync::Mutex<Option<...>>` to hold the most recent decision. The mutex
//! guards only plain data (`Option<String>`), so there is no risk of holding a
//! lock across an `.await` point — the gate waits on the notify, not on the
//! mutex. This keeps the controller fully `Send + Sync` and free of deadlocks.
//!
//! ## Thread-safety
//!
//! The controller is `Arc<Mutex<...>>`-wrapped by callers, matching the rest
//! of the crate's concurrency conventions. All state transitions are atomic
//! with respect to the inner mutex, and no blocking call is ever made while the
//! mutex is held.

use std::sync::{Arc, Condvar, Mutex};

/// A decision produced when a paused pipeline resumes at a stage gate.
#[derive(Debug, Clone, PartialEq)]
pub enum PauseDecision {
    /// Resume the pipeline, optionally with injected human guidance that is
    /// prepended to the next stage's prompt. `None` means "resume as-is".
    ResumeWithFeedback { feedback: Option<String> },
    /// Resume the pipeline without applying any changes.
    ResumeWithoutChanges,
    /// Abort the pipeline entirely.
    Aborted,
}

/// Shared, interior-mutable pause controller for a single council debate.
///
/// Construct one per debate and share it via `Arc`. It is cheap to clone
/// (cloning just bumps the `Arc` reference count).
#[derive(Clone)]
pub struct CouncilPauseController {
    inner: Arc<(Mutex<ControllerState>, Condvar)>,
}

/// The plain data guarded by the controller's mutex.
#[derive(Debug, Default)]
struct ControllerState {
    /// Set to `true` once a pause has been requested.
    pause_requested: bool,
    /// Set to `true` once the pipeline has reached a stage gate and is about
    /// to block (or has just returned). Acts as a one-shot handshake so the UI
    /// knows exactly when to present the "Chime In" drawer. Reset when a
    /// decision is recorded.
    gate_reached: bool,
    /// The most recent decision made by the human, if any. Consumed by the
    /// next waiter so a decision is not applied twice.
    pending_decision: Option<PauseDecision>,
}

impl CouncilPauseController {
    /// Create a fresh controller in the "not paused" state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(ControllerState::default()), Condvar::new())),
        }
    }

    /// Request a pause. Sets the pause flag and notifies any waiter currently
    /// blocked at a stage gate. Idempotent: calling it repeatedly is harmless.
    pub fn request_pause(&self) {
        let (lock, cond) = &*self.inner;
        let mut state = lock.lock().expect("pause controller lock poisoned");
        state.pause_requested = true;
        cond.notify_all();
    }

    /// Whether a pause has been requested for this debate.
    pub fn is_paused_requested(&self) -> bool {
        let (lock, _) = &*self.inner;
        let state = lock.lock().expect("pause controller lock poisoned");
        state.pause_requested
    }

    /// Block until the pipeline has reached a stage gate and is waiting for a
    /// decision. This is the synchronization primitive the UI layer uses to
    /// know when to present the "Chime In" drawer: it returns as soon as a
    /// waiter is parked at `await_stage_gate`, before any resume decision is
    /// made.
    ///
    /// # Thread-safety
    ///
    /// Waits on the `Condvar`, never on the mutex directly. A poisoned lock is
    /// treated as "no longer waiting" so a broken UI cannot deadlock the
    /// pipeline.
    pub fn wait_until_paused(&self) {
        let (lock, cond) = &*self.inner;
        let mut state = lock.lock().expect("pause controller lock poisoned");
        while !state.gate_reached {
            state = cond.wait(state).expect("condvar wait failed");
        }
    }

    /// Block the current (pipeline) thread until a decision is made.
    ///
    /// Returns the [`PauseDecision`] consumed from the controller. If the
    /// controller was never paused, this returns immediately with
    /// `ResumeWithoutChanges`, so callers can invoke it unconditionally.
    ///
    /// # Thread-safety
    ///
    /// This waits on the `Condvar`, never on the mutex directly, so the lock is
    /// never held across a blocking point. A poisoned lock is treated as a
    /// no-op resume to keep a broken UI from deadlocking the pipeline.
    pub fn await_stage_gate(&self) -> PauseDecision {
        let (lock, cond) = &*self.inner;
        let mut state = lock.lock().expect("pause controller lock poisoned");

        // Signal that a gate has been reached so the UI can present the
        // "Chime In" drawer. We set it before waiting so a responder that is
        // already parked on `wait_until_paused` is guaranteed to wake. Notify
        // so a waiter in `wait_until_paused` does not block forever if it
        // acquired the lock before we set the flag.
        state.gate_reached = true;
        cond.notify_all();

        // Fast path: nothing paused, nothing pending. Return immediately.
        if !state.pause_requested && state.pending_decision.is_none() {
            state.gate_reached = false;
            return PauseDecision::ResumeWithoutChanges;
        }

        // Wait until a decision lands or someone notifies us.
        while state.pending_decision.is_none() {
            state = cond.wait(state).expect("condvar wait failed");
        }

        // Consume the decision so it is not applied twice.
        let decision = state
            .pending_decision
            .take()
            .unwrap_or(PauseDecision::ResumeWithoutChanges);
        state.gate_reached = false;
        decision
    }

    /// Record a resume-with-feedback decision and wake any waiter.
    pub fn resume_with_feedback(&self, feedback: Option<String>) {
        self.record(PauseDecision::ResumeWithFeedback { feedback });
    }

    /// Record a resume-without-changes decision and wake any waiter.
    pub fn resume_without_changes(&self) {
        self.record(PauseDecision::ResumeWithoutChanges);
    }

    /// Record an abort decision and wake any waiter.
    pub fn abort(&self) {
        self.record(PauseDecision::Aborted);
    }

    /// Record a decision and notify all waiters.
    fn record(&self, decision: PauseDecision) {
        let (lock, cond) = &*self.inner;
        let mut state = lock.lock().expect("pause controller lock poisoned");
        state.pending_decision = Some(decision);
        // Clear the pause flag so subsequent gates do not block again.
        state.pause_requested = false;
        cond.notify_all();
    }
}

impl Default for CouncilPauseController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_unpaused_gate_returns_immediately() {
        let ctrl = CouncilPauseController::new();
        assert_eq!(ctrl.await_stage_gate(), PauseDecision::ResumeWithoutChanges);
        assert!(!ctrl.is_paused_requested());
    }

    #[test]
    fn test_pause_then_resume_without_changes() {
        let ctrl = Arc::new(CouncilPauseController::new());
        ctrl.request_pause();
        assert!(ctrl.is_paused_requested());

        ctrl.resume_without_changes();
        // A waiter woken by the resume consumes the decision.
        assert_eq!(ctrl.await_stage_gate(), PauseDecision::ResumeWithoutChanges);
    }

    #[test]
    fn test_pause_then_resume_with_feedback() {
        let ctrl = CouncilPauseController::new();
        ctrl.request_pause();
        ctrl.resume_with_feedback(Some("Handle edge cases".into()));

        match ctrl.await_stage_gate() {
            PauseDecision::ResumeWithFeedback { feedback } => {
                assert_eq!(feedback.as_deref(), Some("Handle edge cases"));
            }
            other => panic!("expected feedback resume, got {other:?}"),
        }
    }

    #[test]
    fn test_abort_decision() {
        let ctrl = CouncilPauseController::new();
        ctrl.request_pause();
        ctrl.abort();

        assert_eq!(ctrl.await_stage_gate(), PauseDecision::Aborted);
    }

    #[test]
    fn test_decision_is_consumed_once() {
        let ctrl = CouncilPauseController::new();
        ctrl.request_pause();
        ctrl.resume_without_changes();

        // First waiter consumes the decision.
        assert_eq!(ctrl.await_stage_gate(), PauseDecision::ResumeWithoutChanges);
        // Second waiter sees no pending decision and resumes as a no-op.
        assert_eq!(ctrl.await_stage_gate(), PauseDecision::ResumeWithoutChanges);
    }

    #[test]
    fn test_shared_across_arc() {
        let ctrl = Arc::new(CouncilPauseController::new());
        let other = Arc::clone(&ctrl);
        other.request_pause();
        other.resume_with_feedback(None);

        match ctrl.await_stage_gate() {
            PauseDecision::ResumeWithFeedback { feedback } => assert!(feedback.is_none()),
            other => panic!("expected feedback resume, got {other:?}"),
        }
    }
}
