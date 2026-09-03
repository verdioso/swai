//! SWAI — Main application window subsystem.

pub mod adoption;
pub mod arena_wiring;
pub mod bottom_deck;
pub mod card_wiring;
pub mod dialogs;
pub mod footer;
pub mod header;
pub mod health;
pub mod poller;
pub mod styles;
#[cfg(test)]
mod tests;
pub mod timeout;
pub mod types;
pub mod watchdog;
pub mod window;

pub use types::ImportMessage;
pub use window::MainWindow;
