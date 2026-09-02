//! SWAI — Preferences dialog subsystem.

pub mod checkpoint_tab;
pub mod client_expanders;
pub mod council_tab;
pub mod dialog;
pub mod gateway_tab;
pub mod general_tab;
pub mod guides_tab;
pub mod notifications_tab;
pub mod proxy_tab;
pub mod types;

pub use dialog::PreferencesDialog;
pub use types::PreferencesValues;
