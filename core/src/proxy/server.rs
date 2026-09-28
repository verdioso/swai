use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tiny_http::Server;
use tracing::info;

use super::router::handle_proxy_request;
use super::state::ProxyState;

/// A reverse proxy server that forwards requests to the active model.
///
/// Runs on a background std::thread (not the GTK main loop). The proxy reads
/// the shared `ProxyState` on every request to determine the forwarding target.
///
/// P2-1 FIX: The `reqwest::blocking::Client` is built once during construction
/// and reused for all proxied requests, instead of creating a new client per
/// request. This avoids the overhead of TCP/TLS handshake setup on every call.
pub struct ProxyServer {
    shutdown_flag: Arc<AtomicBool>,
    stop_tx: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    proxy_port: u16,
    /// Reusable HTTP client for forwarding requests to the model server.
    #[allow(dead_code)]
    client: reqwest::blocking::Client,
}

impl ProxyServer {
    /// Create and start the proxy server on the given port with the provided state.
    ///
    /// Returns `Ok(Self)` if the server started successfully, or an error string
    /// if binding failed (e.g., port already in use).
    pub fn new(proxy_port: u16, state: Arc<Mutex<ProxyState>>) -> Result<Self, String> {
        let addr = format!("127.0.0.1:{}", proxy_port);

        let server = Server::http(&addr)
            .map_err(|e| format!("failed to bind proxy server to {}: {}", addr, e))?;

        // P2-1 FIX: Build the reqwest client once and share it across all requests.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| format!("failed to build reqwest client for proxy: {}", e))?;

        // Graceful shutdown via oneshot channel
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = Arc::clone(&shutdown_flag);
        let state_for_proxy = Arc::clone(&state);
        let client_for_proxy = client.clone();

        std::thread::spawn(move || {
            info!("reverse proxy started on http://127.0.0.1:{}", proxy_port);

            for req in server.incoming_requests() {
                // Check shutdown signal first
                if stop_rx.try_recv().is_ok() || shutdown_flag_clone.load(Ordering::Relaxed) {
                    break;
                }

                let state = Arc::clone(&state_for_proxy);
                let client = client_for_proxy.clone();
                if let Err(e) = std::thread::Builder::new()
                    .name("proxy-worker".into())
                    .spawn(move || {
                        handle_proxy_request(req, state, client);
                    })
                {
                    tracing::error!("failed to spawn proxy worker thread: {}", e);
                }
            }

            info!("reverse proxy stopped");
        });

        Ok(Self {
            shutdown_flag,
            stop_tx: Mutex::new(Some(stop_tx)),
            proxy_port,
            client,
        })
    }

    /// Gracefully shut down the proxy server.
    pub fn stop(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        if let Some(tx) = self
            .stop_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = tx.send(());
        }
        // Ping the proxy port to instantly unblock tiny_http's accept() loop.
        let port = self.proxy_port;
        std::thread::spawn(move || {
            let _ = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_millis(200))
                .build()
                .and_then(|c| c.get(format!("http://127.0.0.1:{}/", port)).send());
        });
    }
}

impl Drop for ProxyServer {
    fn drop(&mut self) {
        self.stop();
    }
}
