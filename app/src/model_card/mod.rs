//! SWAI — Model card widget subsystem.

pub mod telemetry;
#[cfg(test)]
mod tests;
pub mod types;
pub mod view;

pub use types::CardState;
pub use view::ModelCard;
