//! SWAI — Reverse proxy server module.
//!
//! A transparent local HTTP reverse proxy listening on `127.0.0.1:proxy_port`
//! (default 9080). It inspects the active model state dynamically and forwards
//! all incoming API requests to whichever model is currently Ready in the
//! ProcessManager.

pub mod anthropic;
pub mod council;
pub mod council_route;
pub mod council_sse;
pub mod ollama;
pub mod ollama_chat;
pub mod ollama_generate;
pub mod ollama_streaming;
pub mod ollama_types;
pub mod openai;
pub mod router;
pub mod server;
pub mod session_tracker;
pub mod state;
pub mod streaming;
pub mod tool_calling;
#[cfg(test)]
mod tests_council;
#[cfg(test)]
mod tests_protocol;
#[cfg(test)]
mod tests_state;

pub use server::ProxyServer;
pub use state::ProxyState;
