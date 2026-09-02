//! SWAI — Arena module: GTK4 debate arena desktop window.
//!
//! Provides a visual interface for viewing council debate transcripts,
//! browsing saved debates, and persisting transcript data to disk.
//!
//! The live-stream sublayer (`stream`, `types`) subscribes to real-time
//! `CouncilEvent`s and streams tokens into stage bubble cards; the historical
//! sublayer (`history`, `view`, `window`) renders saved debates.

pub mod chime_in;
pub mod history;
pub mod stream;
pub mod types;
pub mod view;
pub mod window;

#[cfg(test)]
mod stream_tests;
