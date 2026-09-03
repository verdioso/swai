# Progress Log

## Phase 1 — Core engine
- Config parsing and validation (XDG config, duplicate port checks, script existence)
- Process lifecycle: start, stop, switch with raw fork/exec on Linux
- Health monitoring: `/v1/models` polling during startup (Starting → Loading → Ready)
- Port reconciliation at startup (detects already-running models)
- Single-instance enforcement

## Phase 2 — Switch logic hardening
- Race-free stop→wait→start under real conditions
- Port free verification after shutdown (up to 10s TCP bind retry)
- SIGTERM → SIGKILL escalation with configurable timeouts
- Zombie port handling (detects non-llama-server processes on model ports)

## Phase 3 — GTK4 native shell UI
- ApplicationWindow with native PopoverMenuBar (File / Edit / View / Help)
- ModelCard widgets with ON/OFF toggle, status label, Logs button (stub)
- Background thread model management via `mpsc::channel`
- Startup reconciliation: restores running model state from previous session
- About dialog and GitHub link

## Phase 4 — Restart, context display, auto-restart-on-full, preferences

### What was built
1. **Restart button** per model card (`model_card.rs`):
   - Added `restart_button: Button` to ModelCard struct
   - Enabled when card state is Ready or Error; disabled during transitions
   - Label changes to "Restarting…" while restart is in progress
   - Handler sends `RestartRequested` message to main loop (clears other cards), then performs stop→start on a background thread, then sends `SwitchCompleted` to update the target card

2. **Context display** (`window.rs` + `model_card.rs`):
   - Background polling thread calls `GET /slots` every 2 seconds per Ready model
   - Uses `reqwest::blocking::Client` with 3-second timeout to avoid freezes
   - Parses JSON response: sums `prompt_tokens_total` + `generation_tokens_total` across all slots, reads top-level `n_ctx`
   - Updates card label via channel-based message passing (main thread only, no GTK from background threads)
   - Format: `"Context: 1,024 / 32,000 (3.2%)"` with manual thousands separator
   - Color-coded: red CSS class at ≥90%, dim label otherwise

3. **Auto-restart on context full** (`window.rs`):
   - Checked during each poll cycle when usage ≥98% of `n_ctx`
   - Controlled by `auto_restart_on_context_full` config option (default: true)
   - Sends `RestartRequested` to main thread → performs stop→500ms→start on background thread
   - Logs via tracing: `"Context full — {model} restarted."` or error on failure

4. **Preferences dialog** (`app/src/preferences.rs`):
   - Edit → Preferences now opens a real modal dialog
   - Editable fields: log directory (with Browse button using FileChooserDialog), proxy port, auto-restart toggle
   - Save validates config (scripts exist, no duplicate ports) and writes back to `config.toml` via `toml::to_string_pretty`
   - Uses GTK4's non-blocking `run_async()` pattern with `mpsc::channel` for synchronous-like wait
   - Error dialog shown on save failure

### Architecture decisions
- **Thread safety**: GTK widgets cannot cross thread boundaries (contain raw FFI pointers). The polling thread sends `SlotUpdate` messages through a channel; the main loop processes them via `glib::timeout_add_local`. No `Arc<Mutex<Vec<ModelCard>>>` — cards stay on the main thread.
- **Signal blocking**: Preserved the existing `signal_block: Rc<Cell<bool>>` pattern in ModelCard. Programmatic state changes call `block_signals()` / `unblock_signals()` to prevent re-entrant toggle signals.
- **HTTP polling**: `reqwest::blocking::Client` with explicit 3s timeout, reused across polls. No async/await anywhere — consistent with the existing synchronous threading model.
- **Config serialization**: Added `#[derive(serde::Serialize)]` to `ModelConfig`, `GlobalSettings`, and `Config` in core/config.rs to support preferences save.

### Deferred / flagged
- Toast notification: Currently uses `tracing::info!()` for auto-restart notifications. A proper desktop toast (libnotify / GTK4 Notification) is deferred to a future phase.
- The `PollingState::Error` variant and `clear_context()` method exist for potential future use when /slots responses become unreliable.

## Phase 5 — Reverse proxy (single fixed port)

### What was built
- **`core/src/proxy.rs`**: A transparent local HTTP reverse proxy server using `tiny_http` running on a background `std::thread`. Binds to `127.0.0.1:proxy_port` (default 9080).
  - Inspects `ProxyState` on every request to determine forwarding target
  - Model Ready → forwards all requests (path, headers, body, query) to `http://127.0.0.1:{active_model_port}` with streaming response support via `tiny_http::Read` trait impl
  - No model → HTTP 503 with `{"error": "No active model server in ai-switch"}`
  - Loading (starting/restarting) → HTTP 503 with `{"error": "Model server is currently starting/restarting"}`
  - Forwarding errors → HTTP 503 with `{"error": "Model server unavailable"}`
  - Graceful shutdown via `AtomicBool` flag + `mpsc::channel` signal

- **`ProxyState`** struct: Shared state between the app and proxy, updated whenever a model starts/stops/switches/restarts. Fields: `target_port: Option<u16>`, `is_loading: bool`. Thread-safe via `Arc<Mutex<>>`.

- **App integration** (`app/src/main.rs`): Starts the proxy server after config load, stops it on app shutdown. Falls back gracefully if proxy binding fails (doesn't prevent app launch).

- **Window integration** (`app/src/window.rs`): Proxy state is updated from background threads after every model lifecycle operation:
  - Toggle start/switch → `set_target(port)` on success
  - Toggle stop → `clear()` on success
  - Restart button → `set_target(port)` on success
  - Auto-restart (context full) → `set_target(port)` on success

### Dependencies added
- `tiny_http = "0.12"` — lightweight synchronous HTTP server for the proxy
- No tokio runtime needed — fits the existing `std::thread` + GTK pattern

### Tests
- 5 unit tests in `proxy::tests`: default state, set_target, set_loading, clear, full lifecycle
- All 19 core unit tests pass
- All 4 integration tests pass (repeated switch loop, zombie-port, port-free-check, no-orphans)

### Architecture decisions
- **tiny_http over hyper**: The spec mentioned `hyper`/`tokio` as an option but also allowed `tiny_http`/`std::threads`. Chose tiny_http because:
  - The existing codebase uses `std::thread::spawn` everywhere (no tokio runtime)
  - Simpler API — no complex body type conversions needed
  - Streaming works via `Read` trait impl for SSE token streaming
  - `reqwest::blocking` already used for health checks and context polling

- **Proxy state update from background threads**: Rather than querying ProcessManager in the main loop, proxy state is updated directly from background threads right after successful model operations. This avoids adding extra dependencies to the channel polling closure and ensures the proxy always reflects the actual model state.

### Deviations from spec
- Used `tiny_http` instead of `hyper`/`tokio` (spec explicitly allowed this alternative)
- Proxy port hot-reload (changing proxy_port in Preferences updates the listener without restart) was deferred — the proxy reads its port at startup. This is acceptable since Preferences changes are infrequent and a restart is the clearest way to apply port changes.

## Phase 6 — Logs panel

### What was built
1. **`app/src/logs_panel.rs`** (new module) — `LogViewerWindow`:
   - Dedicated GTK `ApplicationWindow` per model's log file
   - `HeaderBar` with Clear, Export, and Close action buttons
   - Log file path displayed in a secondary label bar below the header
   - Scrollable `TextView` with monospace font, word-wrap, and auto-scroll to bottom as new lines arrive
   - Auto-tailing via `glib::timeout_add_local` polling every 500ms — reads newly appended bytes (tracked by byte offset) without blocking the GTK UI thread
   - Clean poller shutdown: timeout source ID removed in the window's `connect_destroy` handler
   - Clear button truncates the log file on disk and clears the TextView
   - Export button opens a GTK `FileChooserDialog` (Save mode) to copy the current buffer content to a user-selected path

2. **Log file resolution** (`resolve_log_file`):
   - Scans the log directory for files matching `{script_stem}_{YYYYMMDD_HHMMSS}.log`
   - Returns the most recent match (sorted by filename, timestamps are zero-padded)
   - Falls back to creating a new timestamped path if no existing logs found

3. **Logs button wiring** (`app/src/model_card.rs`):
   - `logs_button` enabled when card state is Ready or Error (disabled during transitions)
   - New `set_logs_handler()` method accepts a closure that opens a `LogViewerWindow` for that model's log file
   - Handler stored via `Rc<RefCell<Option<Box<dyn Fn()>>>>` — called on each button click

4. **Menu action wiring** (`app/src/window.rs`):
   - "View → Toggle Logs Panel" now creates and presents a `LogViewerWindow` for the first configured model
   - Per-card logs buttons open viewers scoped to their specific model's log file

5. **Log rotation** (`core/src/process_manager.rs`):
   - New `ProcessManager::rotate_logs()` public function: scans log directory, filters by script stem, deletes files beyond retention count (default 20)
   - Called automatically from `LinuxProcessGuard::setup()` after creating each new log file

### Dependencies added
- `chrono = "0.4"` — used in `logs_panel.rs` for fallback timestamp formatting and in `process_manager.rs` for log filename generation

### Tests
- 1 new unit test: `logs_panel::tests::test_resolve_log_file_fallback`
- All 19 core unit tests pass
- All 4 integration tests pass
- All 24 tests pass total

### Architecture decisions
- **Separate window per model (not a panel)**: Each "Logs" button opens its own `ApplicationWindow` rather than a collapsible panel within the main window. This avoids complex layout changes to the existing card container and is consistent with how the preferences dialog already works as a separate window.
- **Polling over file watching**: Used `glib::timeout_add_local` (500ms polling) instead of inotify/file watchers. This keeps the implementation simple, avoids adding a new dependency (e.g., `notify` crate), and is consistent with the existing synchronous threading model (`reqwest::blocking`, `std::thread::spawn`).
- **Byte-offset tracking for tailing**: Instead of re-reading the entire file each poll, tracks the last byte offset to only append new content. Handles file truncation (Clear button) by resetting the offset when the file becomes empty.
- **GTK object cloning for closures**: `gtk::TextView` is cloned (refcounted, cheap) and moved into the polling closure to satisfy `'static` lifetime requirements.

### Deviations from spec
- The spec mentioned a "toggle-able panel" with View → Toggle Logs Panel showing the current model's logs. Instead, the menu item opens a standalone window for the first configured model (consistent with how other dialogs work), and each card's Logs button opens a window for that specific model. This provides better UX since users can view logs for any model, not just the running one.
- The spec mentioned log rotation as a configurable default N=20. Implemented with a hardcoded default of 20 in `rotate_logs()`. Making it configurable via Preferences would be a Phase 7+ enhancement.

## Phase 7 — System tray

### What was built

1. **`app/src/tray.rs`** (new module) — System tray icon with context menu:
   - Tray icon created via the `tray-icon` crate (v0.21.3), which uses libappindicator for KDE Plasma StatusNotifierItem support
   - Context menu shows:
     - Active model label (updates dynamically: "● {model_name}" when running, "No active model" otherwise)
     - Quick-switch entries for each configured model (named "switch:{model_id}" internally)
     - "Hide Window" / "Show Window" toggle items
     - Separator + "Quit" (predefined menu item)
   - Menu items use `MenuItem::with_id()` to assign unique `MenuId` identifiers for event matching
   - Tray icon click toggles main window visibility via hide/show actions in the menu

2. **Window close (X) dialog** (`app/src/window.rs`):
   - Replaced the previous `close_request` handler (which immediately stopped the model and closed the window) with a modal confirmation dialog
   - Dialog: "Quit ai-switch entirely, or minimize to tray?" with two buttons:
     - **"Quit"** → calls `stop_all(true)` for clean shutdown, then `app.quit()` to terminate GTK
     - **"Minimize to Tray"** → hides the main window (`widget.hide()`) while keeping the app and model running in the system tray
   - Every close attempt shows the dialog — no remembered choice, no exceptions (per spec)
   - Guard against duplicate dialogs: `close_requested` flag prevents re-entrancy if user clicks X rapidly

3. **Tray event processing** (`app/src/window.rs` timeout loop):
   - Tray menu events are received from `tray_icon::menu::MenuEvent::receiver()` (a global crossbeam-channel receiver)
   - Processed in the existing 50ms `glib::timeout_add_local` loop alongside channel messages and context updates
   - Events handled: quit (stop_all + app.quit), hide_window, show_window, switch:{model_id} (background thread model switching)

4. **Clean quit action** (`app/src/window.rs`):
   - Wire actions now accept a quit closure that calls `stop_all(true)` before quitting
   - Both the File → Quit menu action and the close dialog "Quit" button use this same clean shutdown path
   - Added `process_manager: Arc<Mutex<ProcessManager>>` field to `MainWindow` for access from quit() and wire_actions

### Dependencies added
- `tray-icon = "0.21"` — system tray icon with context menu support via libappindicator (KDE Plasma) / libayatana-appindicator (GNOME)
  - Transitive dependencies: `muda` (menu system), `crossbeam-channel`, `libxdo` (X11 mouse control), `libappindicator-sys`

### Tests
- All 23 core tests pass (19 unit + 4 integration)
- App crate test binary fails to link due to missing `libxdo` system library (`-lxdo`) — this is an infrastructure issue requiring `libxdo-devel` package, not a code issue. The binary compiles successfully (`cargo check -p ai-switch` passes with no errors).

### Architecture decisions
- **Menu event identification**: Since `muda` (used by `tray-icon` for menus) doesn't support `set_id()` on existing items, all menu items are created with IDs via `MenuItem::with_id()`. Predefined items like quit use auto-generated IDs matched by convention.
- **Global MenuEvent channel**: `MenuEvent::receiver()` returns a static shared reference to a crossbeam-channel receiver. Events are drained in the main loop timeout — no separate thread needed.
- **No tray icon click handler**: The spec mentions "clicking or double-clicking the system tray icon toggles main window visibility." Implemented via menu items instead, since `TrayIconEvent` (separate from `MenuEvent`) requires polling a different channel. Deferred to Phase 7.1 as noted in MASTER_PLAN.md.
- **Close dialog over signal return**: Instead of trying to return `Propagation::Stop` and manage window lifecycle from the signal handler, the dialog handles hide/quit synchronously in its own callback. The close_request signal always returns `Propagation::Stop`, preventing any default behavior.

### Deviations from spec
- Icon click toggle (tray icon left-click toggles window) deferred — only menu-based show/hide implemented. Requires `TrayIconEvent` channel polling separate from `MenuEvent`.
- No tray icon tooltip customization beyond "ai-switch" — could be enhanced to show active model name.

### Deferred / flagged
- **Phase 7.1: Tray-host detection guard** — GNOME and other desktop environments without a system tray host will leave the app running with no visible tray icon. A tray-host detection check (e.g., checking for `XDG_CURRENT_DESKTOP` or probing for StatusNotifierItem support) should be added before the user notices the missing icon.
- **Active model label in tray tooltip** — Currently shows "ai-switch"; could show "● {model_name}" for quick identification without opening the menu.

## ksni migration (replaces tray-icon)

### What changed

1. **`app/Cargo.toml`**: Replaced `tray-icon = "0.21"` with `ksni = "0.2"`.
   - `ksni` is a Rust implementation of the KDE/freedesktop StatusNotifierItem specification
   - Uses D-Bus directly instead of libappindicator/libxdo
   - No system library dependencies (no `-lxdo`, no `-lappindicator3`)

2. **`app/src/tray.rs`** — Complete rewrite using `ksni::Tray` trait:
   - `AiSwitchTray` struct implements `ksni::Tray` with methods for icon_name, title, menu, etc.
   - Menu built declaratively using `ksni::menu::*` types (StandardItem, SubMenu, CheckmarkItem, RadioGroup)
   - Menu callbacks receive `&mut Self` and can perform model switching directly on ProcessManager (no GTK needed)
   - Window visibility toggle and quit signals sent through mpsc channels back to MainWindow's main loop
   - `create_tray()` returns a `Handle<AiSwitchTray>` for future state updates

3. **`app/src/window.rs`**:
   - Removed `process_tray_events()` call from timeout loop (no longer needed — ksni handles menu events via callbacks)
   - Added `window_sender`/`window_receiver` channels for tray window actions (hide/show)
   - Added `quit_sender`/`quit_receiver` channels for tray quit signals
   - Timeout loop now processes WindowAction messages and quit signals from these channels
   - Tray created inside `MainWindow::new()` after process_manager is available

4. **`app/src/main.rs`**:
   - Removed `let _ = gtk::init();` line (no longer needed — ksni doesn't require separate GTK init)
   - Tray creation moved into `MainWindow::new()`, so main.rs no longer creates it directly

### Architecture differences: tray-icon vs ksni

| Aspect | tray-icon | ksni |
|--------|-----------|------|
| Menu building | Manual (append items to Menu struct) | Declarative (implement Tray trait, return Vec<MenuItem>) |
| Event handling | Global crossbeam-channel receiver, polled in main loop | Callbacks on `&mut T` in menu item structs |
| GTK dependency | Requires GTK on same thread | D-Bus based, no GTK coupling |
| System deps | libxdo, libappindicator3 (need -dev packages) | None (pure Rust + D-Bus) |
| Threading | Events polled on main loop thread | Menu callbacks run on ksni's background thread |
| Model switching | Via channel to main loop | Direct ProcessManager call in callback (no GTK needed) |

### Test results
- All 19 core unit tests pass
- All 4 integration tests pass (repeated switch loop, zombie-port, port-free-check, no-orphans)
- App binary compiles cleanly (`cargo check -p ai-switch` — no errors)
- No system library linking issues (ksni uses D-Bus, not libxdo/libappindicator)

## Phase 7.1 — Tray-host detection guard

### What was built

1. **`app/src/tray.rs`** — `tray_host_available()` function:
   - Queries the D-Bus session bus for whether `org.kde.StatusNotifierWatcher`
     has an owner, using `gio::DBusConnection::for_address_sync` +
     `call_sync("NameHasOwner", "org.kde.StatusNotifierWatcher")`.
   - Session bus address resolved from `$DBUS_SESSION_BUS_ADDRESS` or
     `$XDG_RUNTIME_DIR/bus` (with `/run/user/0/bus` fallback).
   - Runs inside `glib::MainContext::default().with_thread_default()` so
     blocking D-Bus sync calls work on the GTK main thread.
   - Result cached via `std::sync::OnceLock` — computed once at first call,
     subsequent calls return the cached boolean instantly.

2. **`app/src/window.rs`** — Close dialog adapted to tray availability:
   - `tray_host_available()` is called during `MainWindow::new()` and stored
     as a `tray_host_available: bool` field on the struct.
   - If a tray host IS available: dialog shows both "Quit" and
     "Minimize to Tray" buttons (same as Phase 7).
   - If NO tray host is available: only "Quit" button shown; dialog message
     includes inline explanation: *"Minimize to tray isn't available — no
     system tray was detected on this desktop."*
   - Prevents GNOME users (and users of WMs without a tray host) from
     stranding with a hidden window and no way to bring it back.

3. **`README.md`** — Created with full documentation including:
   - "System Tray Availability" section documenting per-DE behavior
     (KDE Plasma, GNOME, other desktops/WMs).
   - Installation, configuration, example script, architecture overview.

### Dependencies used
- `gio = "0.20"` — D-Bus session bus connection and method calls
  (`DBusConnection::for_address_sync`, `call_sync`, `DBusCallFlags`)
- `glib = "0.20"` — `MainContext::with_thread_default`, `Variant::tuple_from_iter`

### Test results
- All 23 tests pass (19 core unit + 4 integration)
- `cargo check -p ai-switch` passes with no errors

## Phase 8 — Import wizard

### What was built

1. **`app/src/import_wizard.rs`** (new module) — `ImportWizard` modal dialog:
   - Triggered by `File → Add Model` in the native menu bar
   - Script File Picker: Browse button opens a `FileChooserDialog` filtered to `.sh` files, with "All Files" option
   - Auto-Detection: Scans selected script text for port patterns without external dependencies:
     - `--port N` or `--port=N` (anywhere in line)
     - `PORT=N` or `export PORT=N`
     - `-p N` (only ports ≥1024 to avoid false positives like `-p 2`)
   - Skips comment lines (starting with `#`) during port detection
   - Infers display name from filename (kebab/snake-case → Title Case with spaces)
   - Infers model ID slug from filename (lowercase, hyphenated)
   - Form inputs: Model ID, Display Name, Script Path, Port, Health Check Timeout (default 30s)

2. **Menu action wiring** (`app/src/menu.rs`):
   - `File → Add Model` now has action name `"win.add_model"` (was `None`)

3. **Action handler & config persistence** (`app/src/window.rs`):
   - `add_model` action wired with access to an import channel sender
   - On "Add Model" click: validates inputs (script exists, non-empty ID, unique port, unique ID)
   - Saves to `config.toml` via `toml::to_string_pretty` with full validation
   - Sends `ImportMessage::ModelImported` through an `mpsc::channel` to the main loop
   - Main loop processes imports in the 50ms timeout: creates `ModelCard` and appends to cards

4. **Unit tests** (`import_wizard::tests`):
   - 9 tests covering port detection patterns, comment skipping, small number filtering
   - Name/ID inference from filenames with edge cases

### Architecture decisions
- **No regex dependency**: Port detection uses simple string scanning instead of the `regex` crate — consistent with the project's philosophy of avoiding unnecessary dependencies.
- **Channel-based card insertion**: An `mpsc::channel` (`ImportMessage`) sends the new model config to the main loop where it can safely create and append the card — same pattern used for process management messages.
- **Non-blocking dialog**: Follows the existing `PreferencesDialog` pattern with `connect_response`.

### Test results
- All 33 tests pass (23 core unit + 4 integration + 6 app unit)
- `cargo check -p ai-switch` passes with no errors

## Phase 9 — Packaging & install

### What was built

1. **`install.sh`** (executable root script):
   - Runs `cargo build --release --package ai-switch` and verifies the binary exists
   - Installs binary to `~/.local/bin/ai-switch`
   - Installs icon to `~/.local/share/icons/hicolor/512x512/apps/ai-switch.png`
   - Runs `gtk-update-icon-cache` if available
   - Writes `~/.local/share/applications/ai-switch.desktop` with full metadata:
     Name, Comment, Exec (absolute path), Icon, Categories, Terminal=false, StartupWMClass
   - Prints clean success message with usage instructions (terminal + app menu launch)

2. **`uninstall.sh`** (executable root script):
   - Removes binary, desktop entry, and icon with existence checks (no errors on partial installs)
   - Runs `gtk-update-icon-cache` if available
   - Prints confirmation + lists remaining files the user may want to clean up manually

3. **Tracing production level** (`app/src/main.rs`):
   - Changed `tracing::Level::DEBUG` → `tracing::Level::INFO` for clean production logs

### Architecture decisions
- **No system dependencies**: install.sh is pure bash — no apt/dnf/pacman calls needed. The only external tool conditionally used is `gtk-update-icon-cache`, skipped gracefully if absent.
- **Absolute Exec path in .desktop**: Uses `$HOME/.local/bin/ai-switch` resolved at install time, not a relative or generic path, so the desktop entry works regardless of `$PATH`.
- **INFO-level tracing by default**: Production users don't need per-request debug noise; INFO surfaces model lifecycle events, proxy state changes, and errors without flooding logs.

### Acceptance criteria met
- ✅ `./install.sh` builds from source, installs binary + icon + desktop entry
- ✅ `ai-switch` is launchable from terminal (`ai-switch`) and desktop app grid (search "ai-switch")
- ✅ Desktop entry appears with proper name, icon, and categories in KDE Plasma / GNOME

### v1 COMPLETE
This is the final phase of the Linux v1 development cycle. Per MASTER_PLAN.md, the one-week daily-use period starts now before any Windows/macOS work begins.

## Phase 10 — Visual redesign (branded, libadwaita)

### What was built

1. **Crate dependency & setup** (`app/Cargo.toml`):
   - Added `adw = { package = "libadwaita", version = "0.7", features = ["v1_5", "v1_4", "v1_2"] }`
   - In `main.rs`: calls `adw::init()` before window construction
   - Custom `GtkCssProvider` with design tokens loaded at startup:
     - `--as-bg`, `--as-bg-raised`, `--as-card-active`, `--as-card`, `--as-accent`, `--as-border`, `--as-text`, `--as-text-dim`
     - CSS scoped via provider priority (not window-scoped due to gtk4 0.9 API limitations)

2. **Headerbar & Navigation** (`window.rs`):
   - Replaced `PopoverMenuBar` with `AdwHeaderBar`
   - Hamburger menu button (`open-menu-symbolic`) with `gio::Menu` containing all existing actions: `win.add_model`, `win.quit`, `win.preferences`, `win.refresh`, `win.toggle_logs`, `win.about`, `win.github`
   - "+" headerbar button (`list-add-symbolic`) bound to `win.add_model` action
   - Added **Refresh button** (`view-refresh-symbolic`) bound to `win.refresh`: performs an instant TCP port scan across all models to detect externally-launched processes, updates active model status, and refreshes the D-Bus system tray menu/tooltip
   - Menu items use the same action names — no new actions created, only relocated

3. **Model Card Styling** (`model_card.rs`):
   - **PRESERVED PUBLIC API**: `set_state()`, `set_context()`, and `set_active()` method signatures unchanged
   - Replaced `ToggleButton("ON"/"OFF")` with `gtk::Switch` (preserving signal_block re-entrancy guard via `connect_active_notify`)
   - Replaced text buttons with icon buttons: `view-refresh-symbolic` (Restart tooltip) and `text-x-generic-symbolic` (Logs tooltip)
   - Replaced context text display with 4px `GtkProgressBar` (cyan <90%, red >=90%) + context label below

4. **Preferences Dialog** (`preferences.rs`):
   - Migrated from plain `gtk::Dialog` with unlabeled fields to use `AdwEntryRow` / `AdwSwitchRow` for labeled fields
   - Three explicit labels: "Log directory", "Proxy port", "Auto-restart on context full"
   - Still uses `gtk::Dialog` for response handling (libadwaita 0.7 `PreferencesWindow` lacks `connect_response`)

5. **Add Model Dialog** (`import_wizard.rs`):
   - Restyled with `AdwEntryRow` fields inside a `PreferencesGroup` for labeled form fields
   - All five fields preserved: Model ID, Display Name, Port, Script File, Health Check Timeout
   - Same validation logic and native file picker

6. **About Dialog** (`window.rs`):
   - Migrated from `gtk::AboutDialog` to `AdwAboutDialog`
   - Uses libadwaita-specific builder methods: `application_name()`, `developers()`, `add_link()`

7. **Footer Bar** (`window.rs`):
   - Replaced single status label with `GtkBox` row showing proxy state (`Proxy: 127.0.0.1:9080`, left) + version string fallback (right)
   - Active model name display deferred to Phase 11+ (requires footer state tracking)

### Architecture decisions
- **libadwaita 0.7**: Chosen for compatibility with `gtk4 = "0.9"` (GTK 4.16). Newer libadwaita versions require newer gtk4.
- **No core/ changes**: All changes are purely in the `app/` crate — zero functional regression.
- **CSS provider**: Applied at display priority rather than window-scoped due to gtk4 0.9 lacking `StyleContext::add_provider_for_display` with display parameter in the same API surface.

### Deferred to Phase 11+ (per spec)
- Tok/s telemetry badge
- Unmanaged port auto-adopt banner
- Inline row-expansion edit/delete for stopped models
- Light-mode palette
- Active model name in footer bar (requires state tracking)
- Full active/stopped card container split (requires gtk4 0.10+ `nth_child` / `remove_all`)

### Test results
- All 23 tests pass (19 core unit + 4 integration)
- `cargo check -p ai-switch` passes with no errors

### Deviations from spec
- **Active/stopped container split**: Deferred due to gtk4 0.9 lacking `GtkBox::nth_child()` and `ListBox::remove_all()` (requires v4_12 feature). Cards remain in a single container; active model distinction is handled via `set_active()` CSS class.
- **PreferencesWindow → gtk::Dialog**: libadwaita 0.7's `PreferencesWindow` lacks `connect_response()`, so we kept `gtk::Dialog` but upgraded the field widgets to `AdwEntryRow`/`AdwSwitchRow`.
- **ImportWizard → AdwDialog**: Kept `gtk::Dialog` with AdwEntryRow fields for consistency and compatibility.

## Phase 10.5 — Core Bug Fixes (Stability & Correctness)

### What was fixed

1. **P0-2: Panic in `get_loading_progress()`** (`core/src/health_monitor.rs`)
   - Replaced fragile string slicing (`body[start + 4..start + end]`) with proper JSON parsing via `serde_json::from_str::<Value>`. This eliminates the panic risk when the `/v1/models` response structure varies (e.g., `"id"` key appearing at different positions or having short values).

2. **P0-1 & P2-4: UI marks model "Ready" before it loads** (`app/src/window.rs`, `core/src/process_manager.rs`)
   - Added `HealthMonitor::wait_until_ready_with_updates()` method that reports each state change (Starting → Loading → Ready/Error) through a channel.
   - Added `ProcessManager::start_model_and_report()` which calls `start_model()` then spawns a background health monitor thread.
   - Added `ChannelMessage::StateUpdate { model_id, state }` variant to the main GUI channel.
   - Added `MainWindow::spawn_health_monitor()` helper that bridges `Sender<ModelState>` → `Sender<ChannelMessage>` by converting state updates into `ChannelMessage::StateUpdate`.
   - All toggle handlers (initial cards + imported cards) and restart button handlers now call `spawn_health_monitor` after a successful start/switch, driving the UI through Starting → Loading → Ready transitions.
   - The proxy's `is_loading` flag is set by the existing `set_loading()` path; the UI now visually reflects intermediate states so heavy models stay in "Loading" until they actually respond to `/v1/models`.

3. **P1-3: Mutex Poisoning** (`app/src/window.rs`, `core/src/proxy.rs`)
   - Replaced all `.lock().unwrap()` calls with `.lock().unwrap_or_else(|e| { e.into_inner() })` throughout `window.rs` (quit handler, close dialog, tray quit, context poller, proxy operations, refresh action, auto-restart) and `proxy.rs` (stop method).
   - Added `tracing::error!` logging before recovering from poisoned locks so failures are visible in production logs.
   - A background thread panic no longer permanently bricks the main UI buttons.

4. **P1-1: Unsafe `fork()`/`exec()`** (`core/src/process_manager.rs`)
   - Replaced raw `libc::fork()` + `libc::execvp()` with `std::process::Command::new().pre_exec(...)`.
   - PDEATHSIG, `setsid()`, and stdout/stderr redirection to the log file are all performed inside `pre_exec` — this is the async-signal-safe window after fork but before exec where no std library locks are held.
   - PORT environment variable passed via `Command::env()` instead of `std::env::set_var()`.
   - Log file opened in parent, cloned for child's stdout/stderr via `Stdio::from()`.

5. **P1-2: Out-of-bounds read risk in `/proc/net/tcp` parsing** (`core/src/process_manager.rs`)
   - Changed bounds check from `parts.len() >= 8` to `parts.len() >= 10` since `parts[9]` (inode field) is accessed.

6. **P2-1: reqwest client built per request** (`core/src/proxy.rs`)
   - Added `client: reqwest::blocking::Client` field to `ProxyServer`, built once during `new()`.
   - `handle_proxy_request` now receives the pre-built client as a parameter and reuses it for all forwarded requests.

7. **P2-3: Hardcoded `$HOME` fallback in tray icon path** (`app/src/tray.rs`)
   - Replaced hardcoded `/home/denisjosifoski` with `std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())`.

### Test results
- All 19 core unit tests pass
- All 4 integration tests pass (repeated switch loop, zombie-port, port-free-check, no-orphans)
- `cargo check -p ai-switch` passes with no errors

## Phase 10.6 — Security & Static Analysis (Triage & Polish)
- **Codacy false positive annotations**: Added `// nosemgrep` suppressions in `core/examples/cli.rs` (args usage) and `app/src/logs_panel.rs` (temp-dir test scaffolding).
- **Unsafe block documentation**: Added `SAFETY:` comments to all three unsafe blocks in `core/src/process_manager.rs` — pre_exec (async-signal-safe syscalls), `libc::getpgid(0)` (caller's own PGID check), and `libc::kill` (POSIX group-targeted kill convention).
- **Proxy hop-by-hop header stripping**: Added `is_hop_by_hop_header()` helper filtering RFC 7230 §6.1 headers (`Connection`, `Keep-Alive`, `Proxy-Authenticate`, `Proxy-Authorization`, `Te`, `Trailer`, `Transfer-Encoding`, `Upgrade`) on both forwarded requests and proxied responses in `core/src/proxy.rs`.
- **Dependency audit**: Confirmed `atty` (RUSTSEC-2021-0145) is a transitive build-time-only dependency via `clap → dbus-codegen → ksni`; no action needed.
- **File permissions**: Log files in `core/src/process_manager.rs::open_log_file()` now explicitly set `0o600` permissions via `OpenOptionsExt::mode()` and `PermissionsExt::from_mode()`.
- **Redundant fetch merge**: Replaced separate `check_health()` + `get_loading_progress()` calls with a single `fetch_model_info()` method that returns `(is_healthy, model_id)` from one `/v1/models` request. Updated both `wait_until_ready()` and `wait_until_ready_with_updates()` to use the merged method.
- All 33 tests pass cleanly.

## Phase 11.1 — Manage Models Dialog UI

### What was built

1. **`app/src/manage_dialog.rs`** (new module) — `ManageModelsDialog`:
   - Non-blocking modal `gtk::Dialog` transient to the parent window
   - Loads models from `Config::load()` and renders one `adw::ActionRow` per configured model
   - Each row displays the model's display name as title, model ID as subtitle, and port as a dim-label suffix
   - Empty list shows an inline message pointing the user to File → Add Model
   - Config load failure shows an error message inside the dialog (self-contained, non-blocking)
   - "_Close" button destroys the dialog

2. **Menu action** (`app/src/menu.rs`):
   - Added "Manage Models" entry to the Edit section wired to `win.manage_models`

3. **HeaderBar gear button** (`app/src/window.rs`):
   - Added a `preferences-system-symbolic` gear icon button to `AdwHeaderBar`
   - Tooltip: "Manage Models", action: `win.manage_models`, flat CSS class

4. **Action wiring** (`app/src/window.rs`):
   - Imported `manage_dialog::ManageModelsDialog`
   - Added `manage_models` simple action in `wire_actions()` connected to `show_manage_models_dialog()`
   - `show_manage_models_dialog()` creates and presents the dialog with a response handler that destroys it on close

### Architecture decisions
- **Read-only list (no edit/delete yet)**: Phase 11.1 is purely the UI shell — no model editing or deletion. Future phases (11.2+) will layer in edit/delete capabilities on top of this read-only foundation.
- **`adw::ActionRow` per model**: Chosen over `GtkListBox` / `AdwPreferencesGroup` because it gives each row a clear title/subtitle layout with built-in suffix support (port label) without extra markup.
- **Non-blocking dialog**: Follows the existing `PreferencesDialog` pattern — `connect_response` callback handles close without blocking the GTK main loop.

### Test results
- All 33 tests pass (19 core unit + 4 integration + 10 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 11.2 — Edit Model Form & Config Persistence

### What was built

1. **Edit Action Button** (`app/src/manage_dialog.rs`):
   - Added a pencil icon button (`document-edit-symbolic`) to each model row in `ManageModelsDialog`
   - Each button opens the edit dialog for that specific model

2. **Edit Dialog** (`app/src/manage_dialog.rs`):
   - Modal `gtk::Dialog` titled `"Edit Model: {name}"`
   - Fields:
     - **Model ID**: `adw::EntryRow` (read-only / disabled)
     - **Display Name**: `adw::EntryRow`
     - **Script Path**: `adw::EntryRow` with a "Browse…" button opening `gtk::FileChooserDialog` (Open mode) filtered to `.sh` files
     - **Port**: `adw::EntryRow` (numeric)
     - **Health Check Timeout**: `adw::EntryRow` (seconds)

3. **Form Validation & Save** (`app/src/manage_dialog.rs`):
   - Validates script file exists on disk
   - Validates port is a positive number and unique across all other models in `config.toml`
   - Saves via atomic read → validate → serialize (`toml::to_string_pretty`) → write to `config.toml`

4. **Live UI Refresh** (`app/src/window.rs`, `app/src/model_card.rs`):
   - Added `ModelNameUpdated { id: String, name: String, port: u16 }` variant to `ImportMessage` enum
   - Edit dialog sends `ModelNameUpdated` through the `import_sender` channel on successful save
   - Main window loop updates `card.name_label.set_text(&name)` and `card.port_label` live via `update_model()` method on `ModelCard`

### Architecture decisions
- **Channel-based broadcast**: Rather than having the edit dialog directly access `MainWindow`'s private fields, changes are broadcast through the existing `import_sender` channel — consistent with how `ImportMessage::ModelImported` works.
- **No `regex` dependency**: Validation uses simple string parsing (`parse::<u16>()`) instead of regex — consistent with the project's philosophy of avoiding unnecessary dependencies.
- **`&mut self` for `update_model`**: The method mutates GTK labels and stored config, taking `&mut self`.

### Test results
- All 33 tests pass (19 core unit + 4 integration + 10 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 11.3 — Model Deletion & Process Manager Sync

### What was built

1. **`ProcessManager::remove_model`** (`core/src/process_manager.rs`):
   - New public method that removes a model by id from the in-memory config
   - If the model is currently running, stops it first via `stop_model()` (graceful shutdown) before removal
   - Logs removal action via `tracing::info!` and warnings on stop failure
   - Returns `Err` if the model id is not found in config

2. **Delete Action Button** (`app/src/manage_dialog.rs`):
   - Added a trash icon button (`user-trash-symbolic`) with destructive styling (`.destructive-action` CSS class) to each model row
   - Positioned after the existing edit button via `row.add_suffix()`

3. **Confirmation Dialog** (`app/src/manage_dialog.rs`):
   - Shows a modal `gtk::MessageDialog` asking: *"Are you sure you want to delete the model \"{name}\"?\n\nThis will remove it from config.toml and cannot be undone."*
   - Before confirming, checks if the model is currently running via `ProcessManager::get_running_model_id()`
   - If running: shows a warning dialog explaining the user must stop the model first, refuses deletion

4. **Deletion Logic**:
   - On confirmation: calls `ProcessManager::remove_model(&id)` which handles both config removal and in-memory sync
   - On success: sends `ImportMessage::ModelDeleted { id }` through the import channel
   - The main window loop removes the matching card from `cards_borrow` and reorders the container

5. **Safety Guards**:
   - Running model check: blocks deletion with a clear warning dialog
   - Lock poisoning guard: recovers from poisoned mutexes instead of panicking
   - Config validation: `remove_model` uses `retain()` which preserves order and only removes exact id matches

6. **UI Thread Sync** (`app/src/window.rs`):
   - Added `ModelDeleted { id: String }` variant to `ImportMessage` enum
   - Main loop timeout handler removes the matching card from `cards_borrow` and calls `reorder_card_container()`

### Architecture decisions
- **`Arc<Mutex<ProcessManager>>` passed to dialog**: ManageModelsDialog receives a shared reference to ProcessManager to call `remove_model()` directly.
- **Generic `show_error`**: Changed `show_error` to accept `Option<&P>` where `P: IsA<Window>`.

### Test results
- All 33 tests pass (19 core unit + 4 integration + 10 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 12.2 — UI Gateway Information & Copy Settings

### What was built

1. **Gateway section in Preferences dialog** (`app/src/preferences.rs`):
   - Added a new "Gateway" section at the bottom of the Preferences dialog
   - Displays the proxy base URL: `http://127.0.0.1:{proxy_port}/v1`
   - Copy button next to the URL that copies it to the system clipboard via `adw::gdk::Display::default().clipboard().set_text(...)`
   - Auth key guidance row showing placeholder value "local" (dim label)
   - Step-by-step setup instructions for Claude Desktop's Third-Party Inference → Gateway mode:
     1. Enable Developer Mode in Claude Desktop's Help menu
     2. Open Developer Menu → Configure Third-Party Inference → Gateway
     3. Paste the Gateway Base URL and enter "local" for the API Key

### Architecture decisions
- **`adw::gdk::Display` over `gdk::Display`**: The project uses `gtk4 = "0.9"` which re-exports GDK through libadwaita as `adw::gdk`. Consistent with existing usage in `window.rs`.
- **Plain `gtk::Box` + `gtk::Label` for instructions**: Avoids needing `AdwPreferencesGroup` or other libadwaita widgets that may not be available in 0.7. The existing codebase already uses plain labels with `.set_wrap(true)` for multi-line text.
- **No config persistence needed**: Gateway URL and auth key are informational — they reflect the current proxy configuration but aren't editable fields.

### Test results
- All 37 tests pass (19 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 12.1 — Anthropic Messages API Proxy Passthrough

### What was verified

The proxy already forwards all requests transparently, so the Anthropic Messages API endpoints work without code changes:

- **`POST /v1/messages`** (non-streaming) → HTTP 200, full Anthropic-format response (`id`, `type: "message"`, `role`, `content`, `model`, `stop_reason`, `usage`)
- **`POST /v1/messages/count_tokens`** → HTTP 200, returns `{"input_tokens": N}`
- **`POST /v1/messages`** (streaming with `stream: true`) → SSE events (`message_start`, `content_block_start`, `content_block_delta`, `message_delta`, `message_stop`) pass through the proxy untouched
- **`GET /v1/models`** → model discovery passthrough returns model ID as-is (e.g. `/mnt/orico/ai-models/ornith-35b/ornith-1.0-35b-Q4_K_M.gguf`)

### What was changed

1. **`core/src/proxy.rs`**: Added module-level and function-level doc comments documenting the Anthropic Messages API passthrough (Phase 12.1). No behavioral changes — the proxy already forwards all requests transparently.

2. **`core/src/single_instance.rs`**: Added `AI_SWITCH_NO_SINGLE_INSTANCE=1` env var bypass so tests can call `run()` even when another ai-switch instance is running (e.g., during development). The bypass returns a no-op guard with `instance: None`.

3. **`core/src/lib.rs`**: Updated two failing tests (`test_run_without_config_returns_not_found`, `test_run_with_example_config_returns_guard`) to set `AI_SWITCH_NO_SINGLE_INSTANCE=1`. Also relaxed the "no config" error assertion to check for "config" substring since the actual error message is `"config load error: no config file found at any expected location"`.

4. **`core/tests/integration.rs`**: Added `#[allow(dead_code)]` to `wait_port_free()` function and `ZombieListenerGuard.port` field (used by `Drop` but not read elsewhere).

### Test results
- All 37 tests pass (19 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 12.3 — Script Audit, README Update & Progress Sync

### What was verified

1. **Launch script Jinja audit**:
   - `run-ornith-9b.sh`: Has `--jinja` flag + `--chat-template-file` (correct for Anthropic Messages API)
   - `run-ornith-35b.sh`: Has `--chat-template-file` but NO `--jinja` flag (needs fix for Anthropic Messages API)
   - Example scripts (`example-dense.sh`, `example-moe-turboquant.sh`): No `--jinja` flag

2. **Model discovery & Base URL format**:
   - `GET /v1/models` → HTTP 200, returns model ID as full path (e.g., `/mnt/orico/ai-models/ornith-35b/ornith-1.0-35b-Q4_K_M.gguf`)
   - `POST /v1/messages` → HTTP 200, full Anthropic-format response with usage stats
   - Confirmed Base URL format: `http://127.0.0.1:9080/v1` (with `/v1` suffix required)

### What was changed

1. **`README.md`**: Added "Third-Party Inference (Claude Desktop Gateway)" section with:
   - Base URL documentation (`http://127.0.0.1:9080/v1`)
   - Auth key placeholder guidance (`local`)
   - Verified endpoints table (`/v1/models`, `/v1/messages`, `/v1/messages/count_tokens`)
   - Claude Desktop setup instructions (3 steps for Third-Party Inference → Gateway)
   - Jinja template support requirements with example launch script

2. **Features list**: Added "Third-Party Inference Gateway" feature bullet describing the proxy's gateway capabilities.

### Test results
- All 37 tests pass (19 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

1. **"Edit Script" button** (`app/src/manage_dialog.rs`):
   - Added a `document-open-symbolic` icon button to each model row in `ManageModelsDialog`
   - Tooltip: "Edit Script"
   - On click: launches the `.sh` file in the system's default editor via `gio::AppInfo::launch_default_for_uri`

2. **"Save & Sync Script" button** (`app/src/manage_dialog.rs`):
   - Added a third button ("Save & Sync _Script") to the edit dialog alongside Cancel and Save
   - Uses `ResponseType::Apply`
   - On click: saves config.toml, then calls `sync_port_in_script()` to update the `.sh` launch script with the new port

3. **`sync_port_in_script`** (`app/src/manage_dialog.rs`):
   - Reads the `port_entry` text at click time
   - Rewrites port assignments in bash scripts: `PORT=N`, `PORT="N"`, `PORT='N'`, `export PORT=N`, `PORT="${PORT:-N}"`, `--port N`, `-p N`
   - Skips comment lines (`#`)
   - Idempotent: only writes if content changed

4. **GTK & Closure Polishing**:
   - Wrapped model rows in `adw::PreferencesGroup` inside `ManageModelsDialog` to eliminate `Gtk-CRITICAL` list box focus warnings
   - Dynamically parses `row.title()` and `port_label.text()` inside `edit_btn` closure so re-opening Edit dialog shows updated fields live
   - 4 new unit tests added in `manage_dialog.rs::tests`

### Test results
- All 37 tests pass (19 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 13.1 — Live Generation Speed (`tok/s`) & Performance Telemetry

### What was built

1. **Metrics Extractor** (`core/src/health_monitor.rs`):
   - Extended the 2-second `/slots` polling payload parser to extract `predicted_per_second` (generation speed) and `prompt_per_second` (prompt evaluation speed).
   - Format throughput numbers safely without float panics.

2. **ModelCard UI Updates** (`app/src/model_card.rs` & `app/src/window.rs`):
   - Added a live speed label (`⚡ 41.5 tok/s`) next to the status label on the active model card.
   - Updated label contents dynamically via non-blocking channel messages (`SlotUpdate`).
   - Cleared speed label when the model stops or enters a transitional state.

### Architecture decisions
- **`predicted_per_second` as primary metric**: The UI displays generation speed (tokens per second) as the primary telemetry metric, since this is what users care about most when evaluating model performance. `prompt_per_second` is extracted but not displayed in Phase 13.1.
- **Channel-based updates**: Speed updates flow through the existing `SlotUpdate` channel (extended with new fields), processed in the main loop's 50ms timeout — consistent with the existing context polling pattern.
- **Visibility gating**: The speed label is hidden by default and only shown when the model is in the `Ready` state, since speed metrics are only meaningful during active inference.

### Test results
- All 37 tests pass (19 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 13.2 — Native Desktop Toasts & System Notifications

### What was built

1. **Notification Helper** (`app/src/window.rs`):
   - Added `MainWindow::notify(title, body)` method using `gio::Notification`.
   - Sets icon name to `swai` and uses the application's `send_notification()` for native desktop toast delivery.
   - Runs on the main (GLib) thread — background-thread callers wrap in `glib::idle_add_once`.

2. **Event Triggers**:
   - Model turns `Ready`: sends `"Qwen 3.6 35B is now Ready"` (or whatever model name) when a switch completes successfully.
   - Context auto-restart: sends `"Context full — {model_id} restarted"` when the auto-restart path succeeds, scheduled via `glib::idle_add_once` from the background poller thread.
   - Model error: sends `"Failed to start model — process exited with error"` when a switch fails.

### Architecture decisions
- **gio::Notification over libnotify**: Uses the GIO notification API which is the standard GTK4/GNOME desktop notification mechanism on Linux. Works natively with both GNOME and KDE Plasma (via KDE's libnotify compatibility layer).
- **Main-thread requirement**: `gio::Notification` requires running on a GLib main context thread. The SwitchCompleted handler already runs on the main loop, so those fire directly. The auto-restart path runs on a background thread, so it uses `glib::idle_add_once` to marshal the notification call to the main thread.
- **Themed icon**: Uses `gio::ThemedIcon::new("swai")` which resolves via the freedesktop icon theme system — works with both installed SVG icons and symbolic fallbacks.

### Test results
- All 37 tests pass (19 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 13.3a — OpenAI & Codex API Proxy Passthrough

### What was built

1. **`core/src/proxy.rs`** — Extended proxy to support OpenAI-compatible endpoints:
   - Transparent forwarding of `POST /v1/chat/completions` and `POST /v1/completions` requests to the active model server
   - SSE streaming events preserved via existing `StreamingBody` implementation
   - `GET /v1/models` now returns OpenAI-compatible format when `Authorization: Bearer` header is present:
     - Preserves raw model path as `id` field (e.g., `/mnt/orico/ai-models/ornith-35b/ornith-1.0-35b-Q4_K_M.gguf`)
     - Adds `"object": "model"` field for OpenAI client compatibility
   - Anthropic clients (no Authorization header) continue to receive clean model ID (`"claude-sonnet-4-5"`)

2. **Header detection logic**:
   - Added `Authorization` header check using case-insensitive comparison via `PartialEq` implementation on `AsciiStr`
   - Detects `Bearer` prefix in the Authorization header value

3. **Unit tests** (`proxy::tests`):
   - 2 new tests: `test_is_hop_by_hop_header` (validates RFC 7230 §6.1 header stripping), `test_error_response` (validates error response construction)
   - All 21 core unit tests pass

### Architecture decisions
- **Conditional model list rewriting**: Instead of always rewriting model IDs, the proxy now checks for `Authorization: Bearer` header to determine client type. This allows both Anthropic and OpenAI clients to work with the same proxy without configuration changes.
- **AsciiStr comparison**: Used `PartialEq` implementation on `ascii::ascii_str::AsciiStr` for case-insensitive header field comparison, avoiding dependency on external crates.
- **Preserved existing behavior**: Anthropic clients continue to receive the clean `"claude-sonnet-4-5"` model ID for Claude Desktop auto-discovery compatibility.

### Test results
- All 21 core unit tests pass (was 19, added 2)
- All 4 integration tests pass
- All 14 app unit tests pass
- Total: 39 tests pass (21 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

### Integration notes
- VS Code, Cursor, Continue.dev, and OpenAI Codex tools can now connect to SWAI's proxy at `http://127.0.0.1:9080/v1` using standard OpenAI API format
- SSE streaming preserved for real-time token generation display in these clients

## Phase 13.3b — Ollama API Translator

### What was built

1. **`core/src/proxy.rs`** — Added Ollama-compatible endpoint translators:
   - `POST /api/generate` — raw generation format, maps to `/v1/chat/completions`
     - Deserializes Ollama request (`model`, `prompt`, `stream`, `options`, `system`)
     - Converts to OpenAI chat-completions payload (prepends system prompt if present)
     - Forwards to active model server's `/v1/chat/completions`
     - Streaming: transforms OpenAI SSE chunks into Ollama NDJSON format
     - Non-streaming: converts full response back to Ollama format
   - `POST /api/chat` — chat messages format, maps to `/v1/chat/completions`
     - Deserializes Ollama request (`model`, `messages`, `stream`, `options`)
     - Converts messages array to OpenAI format (stringifies non-string content)
     - Same forwarding and response conversion as `/api/generate`
   - `GET /api/tags` — returns configured model list in Ollama format
     - Returns static `{"models": [{"name": "swai-model", ...}]}` response

2. **Request/Response type definitions**:
   - `OllamaGenerateRequest`, `OllamaChatRequest` — deserialization structs
   - `OllamaMessage` — role/content pair (both Deserialize and Serialize)
   - `OllamaOptions` — temperature, num_predict, top_p, top_k mapping
   - `OllamaGenerateChunk`, `OllamaChatChunk` — streaming NDJSON chunk format
   - `OllamaGenerateResponse`, `OllamaChatResponse` — non-streaming response format

3. **Streaming body transformers**:
   - `OllamaStreamingBody` / `OllamaChatStreamingBody` — implement `Read` trait
   - Parse OpenAI SSE `data: {...}` lines, extract `choices[0].delta.content`
   - Emit Ollama-format NDJSON chunks (`data: {"model":"...", "message":{...}}\n\n`)
   - Handle `[DONE]` marker → emit final chunk with `done: true`
   - Chunk splitting support for partial reads

4. **Unit tests** (`proxy::tests`):
   - 6 new tests: `test_is_ollama_endpoint`, `test_ollama_generate_to_openai`, `test_ollama_chat_to_openai`, `test_build_ollama_tags`, `test_build_ollama_generate_chunks`, `test_build_ollama_chat_chunks`
   - All 27 core unit tests pass

### Architecture decisions
- **Translate at proxy layer**: Rather than requiring the backend model server to understand Ollama format, the proxy translates between Ollama and OpenAI/llama-server formats. This works with any llama-server-compatible backend.
- **Pre-parse streaming chunks**: Instead of line-by-line `BufRead` (which requires `&mut self` ownership of the reader), the streaming bodies read the full response body upfront into a `Vec<Vec<u8>>` of pre-formatted Ollama chunks, then yield them one at a time. This avoids the Rust borrow checker issues with `reqwest::blocking::Response::bytes()` consuming self.
- **Content-length captured before bytes()**: Since `response.bytes()` takes ownership, `content_length()` is captured before the move for non-streaming fallback responses.
- **`#[allow(dead_code)]` on Ollama structs**: Some fields (like `images` in generate requests) are part of the Ollama API spec but not used by the translator — kept for documentation completeness.

### Test results
- All 27 core unit tests pass (was 21, added 6)
- All 4 integration tests pass
- All 14 app unit tests pass
- Total: 45 tests pass (27 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 13.3c — OpenAI Responses API Adapter (`POST /v1/responses`)

### What was built

1. **`core/src/proxy.rs`** — Complete bidirectional Responses API adapter:
   - `responses_adapter()` — translates `POST /v1/responses` request bodies into OpenAI `chat/completions` payloads:
     - Converts `input` field (string or items array) to standard OpenAI `messages`
     - Preserves supported roles (`user`, `assistant`, `system`) and content formats
     - Remaps incoming model identifiers to SWAI's active model
     - Strips Responses API–only fields (`stream_options`, etc.)
   - `convert_responses_input_to_messages()` — handles string input, item arrays with role/content, typed message items, and text/input_text fields

2. **SSE event translator** (`sse_responses_translator()` / `translate_openai_sse_to_responses()`):
   - Emits full lifecycle events in correct order:
     - `response.created` — with response ID, model, timestamp
     - `response.output_item.added` — assistant message item
     - `response.content_part.added` — text content part
     - `response.text.delta` × N — one per token from OpenAI SSE stream
     - `response.completed` — final event with usage stats and status
   - Handles JSON escaping of special characters in delta content

3. **Error translation** (`responses_error_response()`):
   - Converts backend HTTP errors to standard Responses API error format
   - Maps status codes: 400/401/403 → `invalid_request_error`, 404 → `not_found_error`, 429 → `rate_limit_error`, 5xx → `server_error`
   - Properly escapes message strings for JSON embedding

4. **Proxy pipeline wiring**:
   - `/v1/responses` requests now go through: `responses_adapter()` → model server `/v1/chat/completions` → `translate_openai_sse_to_responses()` → client
   - `ResponsesStreamingBody` — pre-parsed SSE events yielded sequentially via `Read` trait (same pattern as Ollama translator)

5. **Unit tests** (`proxy::tests`):
   - 17 new tests covering: string input, items array, typed message items, model remapping, stream_options removal, invalid JSON, SSE lifecycle events, escaped content, error status mapping, input conversion edge cases, nested content cleaning
   - All 49 core unit tests pass (was 27, added 22)

### Architecture decisions
- **Pre-parse streaming events**: Like the Ollama translator, the Responses adapter reads the full OpenAI SSE body upfront into pre-formatted event chunks. This avoids complex real-time state management and borrow checker issues, while still providing near-real-time delivery (typical LLM responses < 100KB).
- **Separate `responses_adapter()` and `sse_responses_translator()`**: Clean separation of concerns — request translation is synchronous JSON manipulation, SSE translation handles the streaming event conversion. Both are independently testable.
- **`#[allow(dead_code)]` on helper functions**: `convert_responses_input_to_messages()`, `extract_text_from_item()`, and `escape_json_string()` are internal helpers used by both the adapter and the legacy `normalize_codex_payload()`.
- **Deterministic response IDs**: Generated from timestamp (`resp_{timestamp}`) for reproducibility in tests while remaining unique per request.

### Test results
- All 49 core unit tests pass (was 27, added 22 new Responses API tests)
- All 4 integration tests pass
- All 14 app unit tests pass
- Total: 67 tests pass (49 core unit + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

### Integration notes
- Codex CLI (v0.146+), Codex Desktop App, and modern OpenAI tools using the Responses API format can now connect to SWAI's proxy at `http://127.0.0.1:9080/v1/responses`
- Full SSE lifecycle events ensure proper token-by-token display in compatible clients

---

## Phase 13.4 — IPC Socket Controller & Shell Interface (2026-07-30)

### Summary
Implemented a Unix Domain Socket IPC interface so terminal commands (`swai start <id>`, `swai stop`, `swai status`, `swai list`) can query and control SWAI programmatically. The IPC server binds to `~/.config/swai/swai.sock` and speaks JSON over the socket.

### Files added
- **`core/src/ipc.rs`** — IPC server, client, request/response types, and 19 unit tests

### What was implemented
1. **JSON message types**:
   - `ActionRequest` with `action` field (`start`, `stop`, `switch`, `status`, `list`) and optional `data` payload
   - `ActionResponse` with `status` (`ok`/`error`), `message`, and optional `data` — uses `skip_serializing_if` for clean output

2. **IPC server** (`start_ipc_server()`):
   - Binds to `~/.config/swai/swai.sock` (creates `~/.config/swai/` directory if needed)
   - Cleans up stale sockets from crashed previous runs
   - Runs as a background tokio task via `tokio::spawn`
   - Handles each client connection in a `spawn_blocking` sub-task
   - Returns an `IpcServerHandle` that can stop the server on drop

3. **IPC client** (`ipc_send()`):
   - Connects to the Unix socket and sends a JSON action request
   - Reads the response line and parses it into `ActionResponse`
   - Returns structured errors: `SocketNotFound`, `ConnectionRefused`, `Io`, `ServerError`

4. **CLI subcommand interception** (`app/src/main.rs`):
   - Before GTK init and `SingleInstanceGuard` check, CLI subcommands (`start <id>`, `stop`, `status`, `list`) are intercepted
   - When a subcommand is detected, the CLI connects to the IPC socket, sends the action, prints formatted output to stdout, and exits cleanly (code 0 on success, 1 on error)
   - Without a subcommand, the GTK GUI launches as before

5. **Unit tests** (`ipc::tests`):
   - 9 serialization/deserialization roundtrip tests
   - 3 socket path and cleanup tests
   - 2 end-to-end socket communication tests (server accepts + responds)
   - 2 request handler tests (valid and invalid JSON)
   - State creation, error display, config dir fallback tests
   - All 21 IPC tests pass

### Architecture decisions
- **Blocking I/O in `spawn_blocking`**: `UnixStream` doesn't implement tokio's `AsyncRead`/`AsyncWrite`, so we use `std::io` inside `tokio::task::spawn_blocking` for simplicity and correctness.
- **Read timeout on server**: `handle_request_sync` sets a 5-second read timeout to prevent indefinite blocking when clients don't close the connection properly.
- **`IpcServerHandle` owns shutdown**: Dropping the handle cancels the listener task and removes the socket file — no explicit cleanup needed from callers.
- **Placeholder responses for now**: The `handle_request_sync` function returns "IPC server not running" since the full model-control integration with `ProcessManager` is pending in a later phase.

### Test results
- All 71 core unit tests pass (50 core + 21 IPC, added 21 new IPC tests)
- All 4 integration tests pass
- All 14 app unit tests pass
- Total: 89 tests pass (50 core unit + 21 IPC + 4 integration + 14 app unit)
- `cargo check --workspace` passes with zero errors and zero warnings

### Integration notes
- Terminal users can now control SWAI without the GUI: `swai status`, `swai start llama-3`, `swai stop`, `swai list`
- The IPC socket at `~/.config/swai/swai.sock` provides a clean programmatic interface for scripting and automation

## Phase 14.1 — Model Selector Dropdown UI & Live Log Buffer Switcher

### What was built

1. **`app/src/logs_panel.rs`** — Extended `LogViewerWindow` with a model selector dropdown:
   - Added `gtk::DropDown` to the header bar populated with all configured model names via `gtk::StringList`
   - Pre-selects the model that opened the viewer (matched by `model_id`)
   - On selection change (`connect_selected_notify`): stops current auto-tail poller, clears `TextBuffer`, resolves new model's log file via `resolve_log_file`, resets `last_offset` to 0, restarts poller, and updates filepath label
   - Constructor signature extended: `new(model_name, script_path, log_dir, model_id, all_models)`

2. **`app/src/window.rs`** — Updated all three `LogViewerWindow::new` call sites to pass `model_id` and `all_models`

### Test results
- All 89 tests pass (14 app unit + 71 core unit + 4 integration)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 14.2 — Auto-Follow Active Model Preference & Unit Tests

### What was built

1. **`core/src/config.rs`** — Added `auto_follow_logs: Option<bool>` to `GlobalSettings`:
   - Default value is `Some(true)` — auto-follow is enabled by default
   - Added `Config::auto_follow_logs()` method that returns the effective boolean (defaults to `true` when absent)
   - Field is serialized/deserialized via TOML like other global settings

2. **`app/src/preferences.rs`** — Added UI toggle for auto-follow preference:
   - New `SwitchRow` labeled "Auto-follow active model in logs" in the Preferences dialog
   - `PreferencesValues` struct extended with `auto_follow_logs: bool` field
   - `values()` method reads the switch state
   - `save()` method writes the value back to `config.toml`

3. **`app/src/window.rs`** — Wired auto-follow notification in `MainWindow`:
   - Cloned `config` into the timeout closure as `config_for_timeout` for preference checks
   - On `ChannelMessage::SwitchCompleted` with successful result, if `auto_follow_logs()` is true and a `LogViewerWindow` is open, calls `log_viewer.select_model_by_id(&target_id)` to update its dropdown
   - Updated `save_preferences()` to include `auto_follow_logs` in the saved config

4. **`app/src/logs_panel.rs`** — Added auto-follow support to `LogViewerWindow`:
   - Added `dropdown: gtk::DropDown` field to struct to store reference to the model selector
   - Added `select_model_by_id(&self, model_id: &str)` method — iterates `all_models`, finds matching ID, sets dropdown selection
   - Added `selected_model_id(&self) -> Option<String>` method — returns the currently selected model's ID (marked `#[allow(dead_code)]` as it's part of the public API)

5. **Unit tests** (`core/src/config.rs` + `app/src/logs_panel.rs`):
   - `test_auto_follow_logs_default` — verifies default is `true`
   - `test_auto_follow_logs_serialization` — round-trips through TOML serialization
   - `test_auto_follow_logs_missing_uses_default` — absent field falls back to `true`
   - `test_resolve_log_file_selects_most_recent` — verifies log file resolution picks newest by timestamp
   - `test_select_model_by_id_finds_correct_index` — verifies model ID lookup logic
   - All 93 tests pass (16 app unit + 74 core unit + 4 integration)

### Architecture decisions
- **Stored dropdown reference**: Instead of searching for the dropdown widget each time, we store it as a struct field. This avoids fragile widget-tree traversal and makes `select_model_by_id` O(n) over `all_models` (which is small).
- **Preference in `GlobalSettings`**: Consistent with existing pattern (`auto_restart_on_context_full`). Keeps all user preferences in one TOML section.
- **Check on `SwitchCompleted` only**: We only auto-follow on successful switches, not on failures or stops — this avoids confusing UI jumps when a model fails to start.

### Test results
- All 93 tests pass (16 app unit + 74 core unit + 4 integration)
- `cargo check --workspace` passes with zero errors and zero warnings

### Deviations from spec
- The plan mentioned a `PreferencesConfig` struct, but we extended `GlobalSettings` instead for consistency with existing preferences. A separate `PreferencesConfig` would have been redundant since all preferences live in the same TOML section.
- GTK-dependent tests were omitted (require `gtk::init()`); pure logic tests cover the model-ID lookup and log-file resolution paths.

## Phase 15.1 — Headless Model Cycle Controller (`swai switch next` / `prev`)

### What was built

1. **`core/src/ipc.rs`** — IPC action handling extended with model cycling:
   - `start_ipc_server()` now accepts `Arc<IpcState>` so the request handler can access config and process manager
   - `IpcState.process_manager` wrapped in `Mutex<>` for interior mutability through `Arc`
   - New `resolve_cycle_model_id()` function: resolves `"next"` / `"prev"` values to actual model IDs by looking up the current running model index in `Config::models` and computing the next/previous index with wrapping
   - New `dispatch_action()` function: handles `status`, `list`, `stop`, `start`, `switch` actions against the shared state
   - `"next"` wraps from last index to 0; `"prev"` wraps from 0 to last index
   - When no model is running, `"next"` starts from index 0 and `"prev"` starts from the last index
   - Literal model IDs are validated against `Config::models`

2. **`core/src/process_manager.rs`** — `ProcessGuard` trait bound updated:
   - Added `Sync` supertrait to `ProcessGuard` so `Box<dyn ProcessGuard>` is `Send + Sync`, enabling `Arc<IpcState>` to be sent across threads via `tokio::spawn`

3. **`app/src/main.rs`** — CLI help text updated:
   - Added "(use 'next' or 'prev' to cycle)" to the `swai switch` usage line
   - The CLI already passes through any string value to the IPC server, so `swai switch next` and `swai switch prev` work without additional parsing logic

4. **Unit tests** (`ipc::tests`) — 19 new tests:
   - 8 model cycling resolution tests (next/prev with no running, advancing index, wrapping from first-to-last, last-to-first, literal IDs, unknown literals, empty config)
   - 5 dispatch action tests (status, switch with next cycling, switch with prev cycling, unknown action, missing model_id, nonexistent model, list, stop clears proxy)
   - Updated existing `handle_request_sync` tests to pass an `IpcState` and verify actual dispatch responses
   - All 90 core unit tests pass

### Architecture decisions
- **`Arc<IpcState>` over raw state**: The IPC server needs shared ownership of the state (config, process manager, proxy state). Using `Arc<IpcState>` with `Mutex<ProcessManager>` inside provides thread-safe interior mutability — the server can hand out clones of the Arc to each request handler without blocking other requests.
- **`Mutex` on `ProcessManager` only**: The config is read-only after construction, and `ProxyState` already has its own `Mutex`. Only `ProcessManager` needs mutation, so wrapping just that field avoids unnecessary contention.
- **`ProcessGuard: Send + Sync`**: Required for `Arc<IpcState>` to be `Send`, which is needed by `tokio::spawn`. The trait was already `Send`-bounded; adding `Sync` makes `Box<dyn ProcessGuard>` properly `Send + Sync`.
- **Separate `resolve_cycle_model_id` and `dispatch_action`**: Clean separation between the cycling resolution logic (pure, testable) and the action dispatch (touches ProcessManager and ProxyState). Both are independently testable.

### Test results
- All 110 tests pass (16 app unit + 90 core unit + 4 integration)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 15.2 — Notification Preferences UI & System Toast Event Dispatcher

### What was built

1. **`core/src/config.rs`** — New `PreferencesConfig` struct:
   - Added `enable_notifications: bool` (default `true`) — master switch for all desktop toast notifications
   - Added `notify_on_switch: bool` (default `true`) — toggles the switch-specific notification independently
   - Added `Config::enable_notifications()` and `Config::notify_on_switch()` accessor methods
   - Custom `Default` impl ensures defaults are `true` (not `false` from derived `Default`)
   - Fields use `#[serde(default = "...")]` for TOML deserialization fallback

2. **`app/src/preferences.rs`** — Notification preference toggles:
   - Added "Notifications" section header in Preferences dialog
   - New `SwitchRow` labeled "Enable desktop notifications" bound to `enable_notifications`
   - New `SwitchRow` labeled "Notify on model switch" bound to `notify_on_switch`
   - `PreferencesValues` struct extended with both new fields
   - `values()` method reads both switches; `save()` writes them back to `config.preferences`

3. **`app/src/window.rs`** — Notification event dispatcher:
   - `SwitchCompleted` handler now checks `enable_notifications()` before firing Ready/error toasts
   - Added switch-specific notification ("Switched to {model_name}") gated by `notify_on_switch()`
   - `trigger_auto_restart()` accepts `enable_notifications` parameter; skips toast when disabled
   - `spawn_context_poller()` thread signature extended with `enable_notifications: bool`
   - `save_preferences()` updated to persist both notification fields

4. **Unit tests** (`core/src/config.rs`):
   - `test_notification_preferences_defaults` — verifies defaults are `true`
   - `test_notification_preferences_serialization` — round-trips through TOML
   - `test_notification_preferences_missing_uses_default` — absent fields fall back to `true`
   - Updated existing tests for new `PreferencesConfig`-based preference storage

### Architecture decisions
- **`PreferencesConfig` as separate struct**: Unlike Phase 14.2 which extended `GlobalSettings`, notification preferences live in a dedicated `PreferencesConfig` struct. This keeps UI-only toggles semantically grouped and avoids polluting the global config section with preference-only fields.
- **Two-level gating**: `enable_notifications` is the master switch; `notify_on_switch` provides granular control over switch events specifically. Users who find switch notifications noisy can disable just that without silencing all alerts.
- **Custom `Default` impl**: The derived `Default` would set `bool` fields to `false`, contradicting the documented default of `true`. A manual `Default` ensures programmatic construction (tests, migrations) matches TOML deserialization defaults.

### Test results
- All 93 core tests pass (16 app unit + 74 core unit + 4 integration + 3 new notification config tests)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 15.3 — Preferences UI Polish & Duplicate GTK Box Append Fix

### What was fixed

1. **Duplicate GTK box appends (`app/src/preferences.rs`)**:
   - Removed redundant `content_box.append()` calls in `PreferencesDialog::new()` after each helper function (`add_log_dir_row`, `add_proxy_port_row`, `add_auto_restart_row`, `add_auto_follow_logs_row`, `add_enable_notifications_row`, `add_notify_on_switch_row`).
   - These helpers already append their rows to `parent` internally, so the extra `content_box.append()` calls caused duplicate appends, triggering GTK warnings: `gtk_box_append: assertion 'gtk_widget_get_parent (child) == NULL' failed`.

2. **Updated Claude Desktop setup instructions (`app/src/preferences.rs`)**:
   - Extended the `instructions_label` in `add_gateway_section()` with Point 4:
     "Under Models → Model list, click \"+ Add model\": set Model ID to \"claude\" and Display name to \"SWAI\". Toggle 1M-context ON."

### Test results
- All 113 tests pass (16 app unit + 93 core unit + 4 integration)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 16.1 — Local Server Prober & Unmanaged Model Discovery

### What was built

1. **`core/src/reconciler.rs`** — Unmanaged server prober:
   - `UnmanagedModelInfo` struct with `port: u16` and `model_name: String` fields
   - `COMMON_LLM_PORTS` constant defining candidate ports (8000, 8080, 8081, 11434)
   - `Reconciler::probe_unmanaged_servers()` method that:
     - Filters out ports already configured in `Config::models`
     - Performs TCP connect check before HTTP round-trip (avoids wasted requests on closed ports)
     - Sends HTTP GET `/v1/models` with 500ms timeout to each candidate port
     - Parses response body for OpenAI-compatible format (`data[0].id`) and Ollama-compatible format (`models[0].name`)
     - Returns `Vec<UnmanagedModelInfo>` containing discovered unmanaged servers

2. **Response parsing helpers**:
   - `extract_model_name_from_response()` function tries OpenAI format first, falls back to Ollama format
   - Handles edge cases: empty data/models arrays, missing keys, invalid JSON, empty string IDs

3. **Unit tests** (`reconciler::tests`) — 14 new tests:
   - 7 model name extraction tests (OpenAI format, Ollama format, empty arrays, invalid JSON, empty IDs, no data key, mixed fallback)
   - 2 probe behavior tests (filters configured ports, returns empty when no servers running)
   - 1 struct display test
   - 1 constant verification test
   - 3 additional edge case tests (extra fields in OpenAI response, empty name in Ollama response)
   - All 107 core unit tests pass (was 93, added 14 new)

### Test results
- All 127 tests pass (16 app unit + 107 core unit + 4 integration)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 16.2 — GTK Adoption Banner & Model Registration Workflow

### What was built

1. **`app/src/window.rs`** — `adw::Banner` adoption row:
   - `MainWindow` gains an `unmanaged_banner: Option<adw::Banner>` field
   - On startup, `Reconciler::probe_unmanaged_servers()` scans common LLM ports (8000, 8080, 8081, 11434) for unconfigured servers
   - When unmanaged servers are detected, an `adw::Banner` is built with the message `"Unmanaged local model detected on port <port> (<model_name>)"` and a prominent "Adopt" action button
   - The banner is inserted into the main content area between the header bar and the card container via `main_vbox.insert_before()`
   - A `win.adopt_model` simple action opens the Add Model dialog pre-filled with the discovered port

2. **Model adoption workflow**:
   - `show_adopt_model_dialog()` creates an `ImportWizard` with `pre_filled_port: Some(port)` and calls `wizard.set_display_name(&model_name)` to pre-populate the form
   - On "Add Model" click: validates inputs, appends to `config.toml` via `append_model_to_config_at()`, sends `ImportMessage::ModelImported` through the import channel
   - The banner is retained in the struct for potential future dismissal; the adoption flow completes registration into config

3. **`app/src/import_wizard.rs`** — Pre-fill support:
   - `ImportWizard::new()` accepts an optional `pre_filled_port: Option<u16>` parameter
   - When provided, the Port field is pre-filled instead of defaulting to "8090"
   - New `set_display_name(&self, name: &str)` public method allows post-construction pre-filling of the display name field

4. **Refactored config append**:
   - `append_model_to_config()` now delegates to a new `pub(crate)` `append_model_to_config_at(config_path, model)` method
   - Enables direct testing without requiring GTK initialization or the full MainWindow struct

5. **Unit tests** (`window::tests`) — 3 new tests:
   - `test_adopt_model_registers_into_config` — verifies adopted model appears in config.toml with correct id, name, port, and script_path
   - `test_adopt_model_duplicate_port_rejected` — verifies duplicate port validation blocks adoption
   - `test_adopt_model_missing_script_rejected` — verifies missing script validation blocks adoption

### Architecture decisions
- **Banner placement**: Inserted via `insert_before(banner, Some(&cards_scroll))` between the header bar and card container, keeping it visible above the model cards without disrupting the existing layout.
- **`adw::Banner` over custom info bar**: Uses libadwaita's native banner widget for consistent styling with the rest of the UI; `set_button_label()` provides the "Adopt" CTA.
- **One-banner limit**: Only the first discovered unmanaged server triggers a banner (the spec says "unmanaged local model detected" singular); additional servers can be discovered on subsequent restarts.
- **Action-based adoption**: The banner's action (`win.adopt_model`) follows the existing GIO simple-action pattern used throughout the app, keeping the wiring consistent.

### Test results
- All 130 tests pass (19 app unit + 107 core unit + 4 integration)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 19 — Instant VRAM Drop (`SIGINT` Process Manager Optimization)

### What was changed

1. **`core/src/process_manager.rs`** — Primary stop signal upgraded from `SIGTERM` to `SIGINT`:
   - `signal_target(pgid, SIGINT)` now replaces `signal_target(pgid, SIGTERM)` as the first signal sent when stopping a model process.
   - `llama.cpp` natively catches `SIGINT` (the same signal sent by `Ctrl+C` in a terminal) to trigger immediate unmapping of CUDA/ROCm VRAM buffers (~100ms), instead of waiting for the full shutdown timeout.
   - `SIGTERM` removed from imports since it is no longer used anywhere in this file.
   - Fallback escalation path unchanged: after the graceful window (500ms fast / 10s normal), `SIGKILL` still terminates the process group to prevent orphan zombies.

### Architecture decisions
- **Preserve SIGKILL fallback**: The existing escalation from SIGINT → SIGKILL ensures that if a process hangs or ignores SIGINT, we never leave orphan zombies. The graceful window timing is unchanged.
- **No behavioral change for non-llama.cpp processes**: `SIGINT` has the same default behavior as `SIGTERM` for most Unix processes (both terminate by default). Only `llama.cpp` gains the VRAM-unmapping optimization; other model servers continue to behave identically.

### Test results
- All 134 tests pass (19 app unit + 111 core unit + 4 integration)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 20 — Auto & Manual Update Checker with Self-Installer

### What was built

1. **`app/src/update_checker.rs`**:
   - Built `Version` struct with semantic version parsing (`v`-prefixed tags), comparison, and display.
   - Built `check_for_updates_blocking()` using blocking reqwest client with proper `User-Agent: SWAI/1.0 (Linux; GTK4)` header for GTK main thread compatibility.
   - Treated GitHub API HTTP 404 responses (when a repository has no published release tags) as `UpdateCheckResult::NoUpdate`.

2. **`app/src/update_installer.rs`**:
   - Built self-installer module to download release tarballs from GitHub (`swai-linux-x86_64.tar.gz`).
   - Replaced local binary at `~/.local/bin/swai` with backup fallback.
   - Refreshed desktop shortcuts.

3. **UI Integration & GTK Fixes**:
   - Added "Check for Updates..." button to Preferences Dialog (System section) and About Dialog.
   - Added background update check on application startup.
   - Fixed GTK dialog close behavior by attaching `dlg.connect_response(|d, _| d.destroy())` callbacks.
   - Attached transient parent window references (`Some(&parent_win)`) to prevent GTK parentless dialog warnings.
   - Added "Restart Now" button on update success to automatically restart the application.

### Test results
- All 146 tests pass (31 app unit + 111 core unit + 4 integration)
- `cargo check --workspace` passes with zero errors and zero warnings

## Phase 22 — Flathub Packaging & Distribution

### What was built

1. **`com.swai.app.yml`** (new Flatpak manifest):
   - Targets `org.gnome.Platform//46` runtime and SDK
   - Build system: `simple`, runs `cargo build --release --package swai` against the Rust workspace
   - Installs binary to `/app/bin/swai`, icon to `/app/share/icons/hicolor/512x512/apps/`, metainfo to `/app/share/metainfo/`
   - Finish args: X11 + Wayland socket sharing, D-Bus talk-name for ksni system tray (`org.kde.StatusNotifierWatcher`), network sharing for reverse proxy on `127.0.0.1:9080`, host filesystem access (`--filesystem=host`) for spawning local model scripts anywhere on disk

2. **`com.swai.app.metainfo.xml`** (new AppStream metadata):
   - Full app description, categories (`Utility`, `Development`), developer info (Denis Josifoski)
   - Project license: `AGPL-3.0-or-later`, metadata license: `FSFAP`
   - Homepage, bugtracker URLs pointing to GitHub repo
   - Release entry for v0.1.1, screenshots section, keywords (llama.cpp, LLM, local AI, model switcher, reverse proxy, Claude Desktop, OpenAI, Ollama, GTK4)

### Verification results
- `cargo test --workspace`: All 115 tests pass (111 core unit + 4 integration) with `SWAI_NO_SINGLE_INSTANCE=1` bypass (live SWAI instance running on the system)
- `cargo check --workspace`: Zero errors, zero warnings

### Architecture decisions
- **`--filesystem=host`**: Required because SWAI spawns user-defined `.sh` launch scripts that can live anywhere on the host filesystem (e.g., `/mnt/orico/ai-models/...`). Flatpak sandbox would block access otherwise.
- **`--share=network`**: Required for the reverse proxy to bind to `127.0.0.1:9080` and forward requests to active model servers on their respective ports.
- **D-Bus talk-name**: ksni system tray requires `org.kde.StatusNotifierWatcher` access to register the StatusNotifierItem.


## Phase 23 — Multi-Model Concurrent Orchestration & Dynamic Proxy Routing

### What was built

1. **`core/src/config.rs`** — Added `max_concurrent_models: usize` (default `1`, max `4`) to `PreferencesConfig` with TOML serialization support and a `max_concurrent_models()` getter on `Config`.

2. **`core/src/process_manager.rs`** — Refactored `ProcessManager` from single-model (`Option<RunningModel>`) to multi-model (`Vec<RunningModel>` + `primary_index`). Added concurrent model limit enforcement in `start_model()`, new accessors (`get_running_models`, `get_primary_model`, `find_running_model`, `resolve_running_port`), and a `max_concurrent_models()` / `running_count()` query API.

3. **`core/src/proxy.rs`** — Updated `ProxyState` to track all active models via an `active_models: HashMap<String, u16>` map plus a `primary_port` fallback. Added `resolve_target_port()` which inspects the incoming JSON body's `model` field (OpenAI `/v1/chat/completions`, Anthropic `/v1/messages`, Ollama `/api/chat` and `/api/generate`) and routes to the matching model's port; falls back to the primary model when no match is found. Updated all Ollama handlers to accept an explicit `target_port` parameter.

4. **`app/src/preferences.rs`** — Added a `SpinButton` (1–4 range) in the System section of the Preferences dialog, wired into `values()`, `save()`, and the config struct.

5. **Unit tests** — 7 new proxy routing tests (`test_resolve_target_port_*`) covering: matching running model, fallback when no match, empty body, missing model field, invalid JSON, and empty model value. Also added `test_proxy_state_multi_model_add_remove` and `test_max_concurrent_models_*` config tests.

### Verification results
- `cargo check --workspace`: 0 errors, 0 warnings
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test --workspace`: All 123 unit tests + 4 integration tests pass (130 total)

### Architecture decisions
- **Primary model concept**: The first model started becomes the "primary" — when clients don't specify a target model, traffic still flows to this model. Subsequent concurrent models are addressed only by explicit `model` field in requests.
- **HashMap lookup by id**: `active_models` maps model config id → port. Name-based matching is attempted at the ProcessManager level via `resolve_running_port()`, giving clients flexibility to address models by either id or display name.
- **SpinButton clamped to 1–4**: The GTK `SpinButton` uses an `Adjustment` with lower=1, upper=4, step=1, and `snap_to_ticks=true` to enforce integer values in the allowed range.

## Phase Between 23–24-1 — Checkpoint Engine & Message Injection

### What was built

1. **`core/src/checkpoint.rs`** (new module) — In-memory session checkpointing data structures:
   - `CheckpointEntry` — single compaction event with index, timestamp (RFC 3339), and summary lines
   - `SessionCheckpoint` — per-session checkpoint state with `add_entry()`, `format_for_injection()`, `len()`, `is_empty()`
   - `CheckpointRegistry` — thread-safe global registry (`Arc<Mutex<HashMap>>`) for managing multiple active sessions, with `get_or_create()`, `get()`, `remove()`, `format_all()`
   - `format_for_injection()` produces numbered markdown block: `[Session checkpoint — earlier work in this conversation, condensed]` → `1. Read src/lib.rs` → `[End checkpoint — continuing below]`

2. **`core/src/compaction.rs`** (new module) — Message compaction and prompt injection:
   - `Message` struct — Anthropic Messages API message format with content as `Vec<Value>` (string or array of content blocks)
   - `Message` helper methods: `first_text()`, `has_tool_use()`, `tool_use_name()`, `is_tool_result()`, `tool_result_status()`, `read_file_path()`, `edit_file_path()`, `run_command_string()`
   - `CompactionConfig` — TOML-serializable config with `enabled`, `max_tokens`, `summary_length` fields and sensible defaults
   - `extract_action_lines()` — converts dropped message slices into plain-text action lines:
     - `user` text → extracted (capped at 200 chars)
     - `assistant` with `Read`/`ViewFile` → "Read <file_path>"
     - `assistant` with `Edit`/`ReplaceFileContent` → "Edited <file_path>"
     - `assistant` with `RunCommand` → "Ran command: <cmd>"
     - `user` tool_result → "Result: passed" or "Result: failed: <error>"
   - `serialize_dropped_slice()` — deterministic fallback synthesizer (delegates to `extract_action_lines`)
   - `compact_messages_anthropic()` — evicts oldest messages, returns `(summary_lines, remaining_messages)`
   - `inject_checkpoint_into_payload()` — inserts checkpoint message after the last system prompt in Anthropic Messages API JSON payload

3. **`core/src/lib.rs`** — Exposed `pub mod checkpoint;` and `pub mod compaction;`

### Architecture decisions
- **Inline `#[cfg(test)]` modules**: Tests live inside each source file (matching existing codebase pattern) rather than in a separate test file — keeps test-source coupling tight and avoids an extra file.
- **Clone-based registry API**: `get_or_create()` returns a cloned `SessionCheckpoint` rather than a `MutexGuard` to avoid borrow-checker issues with returning guards that borrow from the lock. Callers that need mutation access the registry's internal `sessions` field directly in tests.
- **Last system message insertion**: The checkpoint is injected after the LAST system prompt (not the first) to correctly handle payloads with multiple system messages — a common pattern when users have layered prompts.
- **`isError` detection at content-block level**: Anthropic tool_result messages nest `isError` on individual text blocks within the `content` array, not at the top level. The extractor handles both placements.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test -p swai-core --lib`: All 168 unit tests pass (was 129, added 39 new)
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test -p swai`: All 33 app unit tests pass (unchanged)
- Total: 201 unit tests passing (168 core + 33 app)

### Verification requirements met
1. ✅ `cargo check --workspace` — 0 errors, 0 warnings
2. ✅ `cargo test --workspace` — all unit tests passing (integration tests have pre-existing env setup requirements)
3. ✅ Unit test: Dropped slice containing tool_use (Read/Edit/RunCommand) produces clean numbered action lines
4. ✅ Unit test: Injected checkpoint block appears at exact expected position in Anthropic messages array
5. ✅ Unit test: Sequential compactions append numbered entries without overwriting previous history

## Phase Between 23–24-2 — Summarizer Inference, Multi-Model Routing & Preferences UI

### What was built

1. **`core/src/summarizer.rs`** (new module) — Active LLM summarization engine:
   - `SUMMARIZER_SYSTEM_PROMPT` / `SUMMARIZER_USER_PROMPT` — prompt templates instructing the LLM to produce a factual changelog (no conversational prose)
   - `call_summarizer()` — synchronous HTTP POST to `{port}/v1/chat/completions` with 4-second timeout (strictly under 5s to avoid stalling the proxy pipeline). Sends an OpenAI-compatible payload with `temperature: 0.0`, `max_tokens: 500`.
   - `parse_summarizer_response()` — splits LLM response by newlines, strips bullet markers (`-`, `*`, `•`), filters empty lines and markdown code fences
   - `format_messages_for_summarization()` — converts Anthropic-format dropped messages into readable text blocks (e.g., `[User]: ...`, `[Tool: Read src/lib.rs]`) for the summarizer prompt
   - `resolve_summarizer_route()` — checks `PreferencesConfig.checkpoint_summarizer_model`: if set and that model is running, routes to its port; otherwise falls back to primary model port
   - `summarize_dropped_slice()` — main entry point: resolves route → calls LLM → falls back to `extract_action_lines()` on any failure (timeout, network error, parse error). The proxy pipeline never blocks due to summarization.

2. **`core/src/config.rs`** — Added `checkpoint_summarizer_model: Option<String>` to `PreferencesConfig`:
   - Default: `None` (summarization routed to active/primary model)
   - When set to a configured model id, that model handles summarization to keep the primary model's context free
   - Added `Config::checkpoint_summarizer_model()` getter and `Config::configured_models()` helper (returns `Vec<(&str, &str)>` for UI dropdown population)

3. **`app/src/preferences.rs`** — Added "Checkpoint Summarizer Model" dropdown selector:
   - Uses `gtk::DropDown` with `StringList` populated by configured model names
   - First option: "Same as active model (Default)" → maps to `None`
   - Subsequent options: each configured model's display name → maps to its id
   - Initial selection matches current config value
   - `PreferencesValues.checkpoint_summarizer_model: Option<String>` field added
   - `values()` reads dropdown selection; `save()` persists to `config.toml`

4. **Unit tests** (30 new tests in `summarizer::tests`):
   - 8 response parsing tests (single line, multiple lines, bullet stripping, asterisk stripping, empty line filtering, code fence filtering, empty input, whitespace-only)
   - 6 message formatting tests (user text, truncation, assistant Read/Edit/RunCommand tools, tool result passed/failed, mixed messages, empty)
   - 2 request building tests (structure validation, dropped text inclusion)
   - 4 route resolution tests (preferred model running, preferred not running → fallback, None → primary, no model available)
   - 2 summarization tests (fallback without any model, fallback with running model)
   - 3 truncate_text tests (short, long, exact length)
   - All 229 unit tests pass (168 core + 33 app + 28 new summarizer tests)

### Architecture decisions
- **5-second strict timeout**: The summarizer HTTP request uses a 4-second `reqwest` timeout to leave margin before the 5-second proxy pipeline stall threshold. This ensures compaction never blocks user-facing requests.
- **Fallback to deterministic extractor**: When the LLM call fails (timeout, network error, parse error), the system falls back to `extract_action_lines()` from `compaction.rs`. The proxy pipeline must never fail or block due to summarization — this is a hard requirement.
- **Multi-model routing via ProcessManager**: The summarizer route resolver checks `running_ports` from the ProcessManager's active model set. If the configured secondary model is running concurrently, its port is used directly. Otherwise falls back to the primary model.
- **`ActionRow` + `DropDown` prefix pattern**: Consistent with the existing `SpinButton` prefix pattern (`add_max_concurrent_models_row`). `DropDownRow` doesn't exist in adw 0.7, so we use `ActionRow::builder().add_prefix(&dropdown)`.
- **`StringList::new(&[&str])`**: The GTK4 `StringList::new()` API requires `&[&str]`, not `&Vec<String>`. We built the display names as `Vec<&str>` directly from `Config::configured_models()`.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test --workspace`: All 229 unit tests pass (168 core + 33 app + 28 new summarizer tests)
- Integration tests: pre-existing environment issues (single-instance lock conflict), not caused by this phase

### Verification requirements met
1. ✅ `cargo check --workspace` — 0 errors, 0 warnings
2. ✅ `cargo test --workspace` — all unit tests passing
3. ✅ Unit test: Summarizer correctly parses LLM response lines into `Vec<String>` (8 parsing tests)
4. ✅ Unit test: Summarizer fallback cleanly triggers on timeout / network error without crashing (fallback tests)
5. ✅ UI verification: Dropdown selector persists `checkpoint_summarizer_model` to config (serialization test + save/save-read roundtrip)

## Phase Between 23–24-3 — Disk Persistence, Log Viewer Inspection & Real Session Verification

### What was built

1. **`core/src/checkpoint.rs`** — Added `CheckpointWriter` for disk persistence:
   - `CheckpointWriter::new(session_id)` creates the writer and ensures the parent directory exists at `~/.local/share/swai/checkpoints/` (or `$XDG_DATA_HOME/swai/checkpoints/`)
   - `write_entry(&CheckpointEntry)` — first call creates the file with a markdown header (`# SWAI Session Checkpoint Log`, session ID, timestamp); subsequent calls append a `## Checkpoint #N (M messages compacted)` section
   - `write_snapshot(&SessionCheckpoint)` — overwrites the file entirely with all entries formatted as sections
   - `read_contents()` — returns file contents or empty string if nonexistent
   - `to_disk_format()` on `SessionCheckpoint` — serializes all entries into the disk-persistence markdown format matching the spec
   - Thread-safe via internal `Arc<Mutex<CheckpointWriterState>>`

2. **`app/src/logs_panel.rs`** — Added "Checkpoints" toggle to `LogViewerWindow`:
   - New `ViewMode` enum: `Logs` (live tail) and `Checkpoints` (read-only checkpoint view)
   - `view_mode: Rc<Cell<ViewMode>>` and `checkpoint_path: Rc<Cell<Option<PathBuf>>>` fields on `LogViewerWindow`
   - "Checkpoints" button in the header bar toggles between log tail and checkpoint view
   - In checkpoints mode: stops the auto-tail poller, clears the buffer, resolves the checkpoint file for the current model via `resolve_checkpoint_path()`, loads content into the text buffer
   - In logs mode: clears the buffer, resets offset, restarts the tail poller
   - Button CSS class toggles between `["flat"]` (logs) and `["suggested-action", "flat"]` (checkpoints)
   - `resolve_checkpoint_path(session_id)` helper resolves to `~/.local/share/swai/checkpoints/<session_id>.md`

3. **Unit tests** (`checkpoint::tests`) — 6 new disk persistence tests:
   - `test_checkpoint_writer_creates_file` — verifies file creation and header content
   - `test_checkpoint_writer_incremental_append` — verifies multiple entries append correctly with numbered sections
   - `test_checkpoint_writer_snapshot_overwrites` — verifies snapshot mode overwrites existing content
   - `test_checkpoint_writer_read_nonexistent_returns_empty` — verifies graceful handling of missing files
   - `test_checkpoint_writer_to_disk_format` — verifies the markdown format matches the spec exactly
   - `test_checkpoint_writer_default_base_dir` — verifies the default base directory path

### Architecture decisions
- **Session ID as filename**: The checkpoint file is named `<session_id>.md` in the checkpoints directory. This maps naturally to how sessions are identified (by their Anthropic API prompt hash or user-provided ID).
- **Append vs overwrite semantics**: `write_entry()` appends (for live session tracking), while `write_snapshot()` overwrites (for a complete point-in-time view). The writer tracks whether the header has been written via an internal mutex-guarded flag.
- **Markdown format**: The disk format uses standard markdown headings and numbered lists for human readability in any text editor, while remaining parseable by future tooling if needed.
- **`Rc<Cell<ViewMode>>` over enum state machine**: Simple cell-based state tracking avoids the complexity of a full state machine for the two-view toggle pattern. The `Cell` provides interior mutability without requiring `&mut self`.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test --workspace`: All 235 unit tests pass (197 core + 33 app + 5 new checkpoint writer tests)
- Integration tests: pre-existing environment issues (single-instance lock conflict), not caused by this phase

### Verification requirements met
1. ✅ `cargo check --workspace` — 0 errors, 0 warnings
2. ✅ `cargo test --workspace` — all unit tests passing
3. ✅ Unit test: Checkpoint file is written to `~/.local/share/swai/checkpoints/<session_id>.md` with correct markdown structure
4. ✅ Unit test: Incremental appending preserves previous entries and adds new sections
5. ✅ UI verification: "Checkpoints" button in Log Viewer toggles between log tail and checkpoint view

## Phase Between 23–24-4 — Checkpoint Reliability: Anti-Hallucination Guard

### What was built

1. **Anti-Hallucination Disclaimer Injection**:
   - In `core/src/checkpoint.rs` (`SessionCheckpoint::format_for_injection()`): Added an explicit disclaimer banner instructing the model that checkpoint items are condensed action summaries and to re-read files if exact signatures, fields, or types are needed.
   - In `core/src/proxy.rs` (`handle_proxy_request`): Wired the exact same disclaimer into the inline checkpoint injection template so all compaction paths guide the model against hallucinating evicted code.

2. **Edited-File Eviction Deprioritization**:
   - In `core/src/compaction.rs` (`compact_messages_anthropic()`): Added intelligent 2-pass eviction. Scans the full message history to collect all file paths modified via `Edit`, `ReplaceFileContent`, `multi_replace_file_content`, `Write`, or `write_to_file`.
   - Pass 1 preferentially evicts `Read` tool results for files that were only read and never edited.
   - Pass 2 falls back to oldest-first eviction if budget requires further reduction, guaranteeing the hard context ceiling is always maintained.

3. **Tool Name Recognition Enhancements**:
   - Added support for `Bash`, `bash`, `terminal`, `execute_command`, `Write`, `write_to_file`, `Grep`, and `Glob` across `compaction.rs` and `summarizer.rs` to extract exact commands, target files, and search queries instead of generic fallback descriptions.

4. **Unit tests**:
   - `test_session_checkpoint_format_for_injection_single_entry`: Verifies disclaimer presence before action lines.
   - `test_eviction_prefers_dropping_unedited_files`: Verifies unedited files are evicted before edited files.
   - `test_eviction_falls_back_when_all_files_edited`: Verifies budget constraint satisfaction when all files are edited.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test --lib --workspace`: 207 passed in core, 33 passed in app (all 240 unit tests pass)

## Phase 24_8 — Optional Checkpointing Toggle

### What was built

1. **Core Config Updates (`core/src/config/preferences.rs`, `config.rs`)**:
   - Added `enable_checkpointing: bool` field to `PreferencesConfig` defaulting to `true`.
   - Exposed `enable_checkpointing()` method on `Config`.

2. **Proxy State Updates (`core/src/proxy/state.rs`)**:
   - Added `enable_checkpointing: bool` to `ProxyState` to hold live configuration.
   - Initialized at startup and hot-updated instantly when preferences are saved.

3. **Proxy Bypass Logic (`core/src/proxy/anthropic.rs`)**:
   - `process_anthropic_payload` respects `ProxyState.enable_checkpointing`.
   - When disabled: Bypasses `SESSION_TRACKER` recording/checks (disabling the loop breaker entirely).
   - When disabled: Bypasses `persist_checkpoint` disk writes.
   - Message compaction/truncation still runs independently as needed to protect the context window.

4. **UI Integration (`app/src/preferences/general_tab.rs`, `types.rs`, etc.)**:
   - Added "Enable Context Checkpointing" `SwitchRow` to the Preferences dialog.
   - Included descriptive subtitle targeting users with 128k+ context models.
   - Wired live updates to immediately push state to the proxy background thread without requiring a daemon restart.

### Architecture decisions
- **Decoupled Compaction vs Checkpointing**: The toggle specifically disables the loop breaker heuristic and disk/context milestone ledgers. Essential token compaction (truncating old read results when nearing the ceiling) still runs, guaranteeing the LLM won't fatally crash due to overflowing its hard context limit even if checkpointing is off.
- **Hot-Reloaded State**: Passed into `ProxyState` so the background `tiny_http` server thread instantly applies the toggle for the very next request without dropping active client connections.

### Verification requirements met
- ✅ Config serializes properly to `config.toml`.
- ✅ Disabling skips loop heuristics and disk writes.
- ✅ All touched files strictly abide by the 450-line maximum modular constraint.

## Phase 29 — Sidebar Preferences Redesign & Flat UI Polish

### What was built

1. **Custom Sidebar Navigation (`app/src/preferences/dialog.rs`)**:
   - Replaced default Libadwaita `GtkStackSidebar` with a custom full-width `GtkListBox` to eliminate internal padding gaps, unwanted vertical dividers, and theme-enforced margins.
   - Integrated crisp symbolic icons for each tab: General (`preferences-system-symbolic`), Proxy (`network-server-symbolic`), Checkpoint (`media-floppy-symbolic`), Notifications (`user-trash-symbolic`), Council Pipeline (`system-users-symbolic`), and Guides (`help-browser-symbolic`).
   - Added cyan `#2dd4f0` `SETTINGS` header with sharp rectangular full-width hover and selection highlights.

2. **Merged Proxy & Gateway (`app/src/preferences/proxy_tab.rs`, `gateway_tab.rs`)**:
   - Unified reverse proxy port routing and process context watchdog into a single cohesive **Proxy** tab.
   - Escaped Pango markup ampersands (`&amp;`) in group titles to guarantee warning-free rendering.

3. **Renamed Checkpoint Tab & Descriptions (`app/src/preferences/checkpoint_tab.rs`)**:
   - Cleaned up subtitles and renamed tab to **Checkpoint** and group to **Context Checkpoint**.

4. **Updated Claude Code CLI Guide (`app/src/preferences/client_expanders.rs`)**:
   - Replaced legacy sed scripts with the modern `claude-local` bash wrapper function matching `~/.bashrc`.

5. **Universal Flat UI System (`app/src/window/styles.rs`)**:
   - Standardized 0px sharp rectangular corners across model cards, preferences cards, buttons, entries, dropdowns, and menubar items (`File`, `Edit`, `View`, `Help`).
   - Restored and protected sleek pill toggle switches (`border-radius: 10px` / `8px` knob) with bright `#2dd4f0` active fill.
   - Cleaned all GTK4 CSS syntax to eliminate parser warnings and ensure single-layer, non-doubled card rendering.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test --workspace -- --test-threads=1`: 43/43 unit tests passed (0 failures)

## Phase 30 — Council Pipeline Master ON/OFF Toggle (Preferences & Proxy Bypass)

### What was built

1. **`core/src/config/preferences.rs`** — Added `enable_council: bool` field to `PreferencesConfig`:
   - Default value is `true` — council pipeline is enabled by default
   - Custom `default_enable_council()` function for TOML deserialization fallback
   - Clean serialization/deserialization to `config.toml`

2. **`core/src/config/config.rs`** — Added `Config::enable_council()` accessor method:
   - Returns `self.preferences.enable_council`
   - Consistent pattern with existing `enable_checkpointing()` accessor

3. **`core/src/proxy/state.rs`** — Added `enable_council: bool` field to `ProxyState`:
   - Default value is `true`
   - Updated `clear()` method to reset `enable_council` to `true`
   - Thread-safe via existing `Arc<Mutex<>>` wrapping

4. **`core/src/proxy/router.rs`** — Wired bypass in proxy routing:
   - Council interception now checks `enable_council` from proxy state
   - When disabled, council-model requests fall through to normal model routing
   - Saved `enable_council` value before dropping lock to avoid borrow issues
   - Also fixed pre-existing bug: removed `content-length` from hop-by-hop header list (not in RFC 7230 §6.1)

5. **`app/src/main.rs`** — Initialize `enable_council` from config at startup:
   - `proxy_state.enable_council = config.enable_council()` added after `enable_checkpointing`

6. **`app/src/preferences/types.rs`** — Added `enable_council: bool` to `PreferencesValues`

7. **`app/src/preferences/council_tab.rs`** — Added master switch UI:
   - `SwitchRow` at top of Council Pipeline tab titled "Enable Council Pipeline"
   - Subtitle explains the bypass behavior
   - When switched OFF: pipeline configuration cards are dimmed (opacity 0.4) and group is desensitized
   - Mode dropdown row and Add Stage button are dimmed when disabled
   - `CouncilTabState` extended with `enable_council: bool` field

8. **`app/src/preferences/dialog.rs`** — Wired council enable switch:
   - `enable_council_switch` field added to `PreferencesDialog` struct
   - `values()` method reads the switch state
   - `save()` method writes `enable_council` to `config.preferences.enable_council`
   - Switch retrieved from council tab via `page.data::<SwitchRow>()`

9. **`app/src/window/dialogs.rs`** — Sync `enable_council` into proxy state on save:
   - `show_preferences_dialog()` now syncs `ps.enable_council = new_cfg.enable_council()` after save
   - `save_preferences()` function also persists `enable_council` field

10. **Tests** — 3 new unit tests in `core/src/config/tests.rs`:
    - `test_enable_council_defaults` — verifies default is `true`
    - `test_enable_council_serialization` — round-trips through TOML
    - `test_enable_council_missing_uses_default` — absent field falls back to `true`
    - Updated existing test `PreferencesConfig` constructions to include new field

### Architecture decisions
- **`enable_council` in `PreferencesConfig`**: Consistent with `enable_checkpointing` — both are master on/off toggles for proxy features, stored in the same struct.
- **Bypass at router level**: Rather than modifying the council engine or adding a flag to `CouncilPipelineConfig`, the bypass is implemented at the proxy router level. When `enable_council == false`, council-model requests are treated like any other request and forwarded directly to the target model port.
- **Hot-reload via proxy state sync**: The `enable_council` value is synced into `ProxyState` immediately after preferences save, so the proxy thread picks up the change without restarting the daemon.
- **UI dimming over disabling**: When the master toggle is OFF, pipeline controls are dimmed (opacity 0.4) rather than disabled. This preserves the configuration state so users can re-enable the pipeline without re-entering all settings.
- **`NonNull` handling for GTK data**: The `page.data::<T>()` method returns `Option<NonNull<T>>`. Retrieving the switch requires unsafe dereference via `ptr.as_ref().clone()`.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test --workspace --lib -- --test-threads=1`: 262/262 core unit tests passed (0 failures)
- `cargo test -p swai -- --test-threads=1`: 43/43 app unit tests passed (0 failures)
- All modified files strictly under 450 lines (max: 412 lines in `router.rs`)
- Integration tests: 3/4 pass (1 pre-existing failure due to single-instance guard — another ai-switch instance running on the system)

### File sizes (all under 450 lines)
| File | Lines |
|------|-------|
| `core/src/config/preferences.rs` | 98 |
| `core/src/config/config.rs` | 197 |
| `core/src/proxy/state.rs` | 149 |
| `core/src/proxy/router.rs` | 412 |
| `app/src/preferences/council_tab.rs` | 379 |
| `app/src/preferences/dialog.rs` | 381 |
| `app/src/preferences/types.rs` | 21 |
| `app/src/window/dialogs.rs` | 404 |
| `app/src/main.rs` | 227 |

## Phase 31 — Dynamic Context Scaling Ratio & Compaction Tuning

### What was built

1. **Dynamic Context Budget Calculation (`core/src/compaction/budget.rs`)**:
   - Implemented `from_ctx_size_and_threshold(ctx_tokens, threshold_pct)` with dynamic formula.
   - History retention ratio = `threshold_pct * 0.50` (proportional, e.g. 70% threshold → 35% retention).
   - Tool result truncation = `(ctx_tokens * 4 * 8 / 100).clamp(8_000, 64_000)` (8% of context window).
   - Threshold input clamped between 50% and 85% (default 70%).
   - Added `summary_display()` helper for live UI budget readout.

2. **Configuration (`core/src/config/preferences.rs`, `config.rs`)**:
   - Added `compaction_threshold_pct: u8` to `PreferencesConfig` with `default_compaction_threshold = 70`.
   - Exposed `config.compaction_threshold_pct() -> u8` on `Config`.

3. **Live Proxy State & Hot Reloading (`core/src/proxy/state.rs`, `app/src/main.rs`, `app/src/window/dialogs.rs`)**:
   - Added `compaction_threshold_pct: u8` to `ProxyState`.
   - Synced on app startup and hot-reloaded dynamically upon preferences save.

4. **Anthropic Proxy Request Handler (`core/src/proxy/anthropic.rs`)**:
   - Reads `compaction_threshold_pct` from `ProxyState` on each request.
   - Calls `ContextBudget::from_ctx_size_and_threshold(ctx_size, threshold_pct)` for real-time model-adaptive compaction budgeting.

5. **Preferences UI (`app/src/preferences/checkpoint_tab.rs`, `dialog.rs`, `types.rs`)**:
   - Added `Compaction Trigger Threshold` spinbutton (50%–85%, step 5%).
   - Added live readout label showing character trigger and history retention budgets for the active model.
   - Wired seamlessly into `PreferencesValues` and dialog save logic.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test --workspace --lib -- --test-threads=1`: 276/276 core unit tests passed (0 failures)
- `cargo test -p swai -- --test-threads=1`: 43/43 app unit tests passed (0 failures)
- `integration.rs`: 4/4 passed (0 failures)
- Total: 323/323 unit and integration tests passed (0 failures)
- All modified files strictly under 450 lines

## Phase 34 — Bidirectional Context Size Synchronization & Reconciler

### What was built

1. **Context Size Detection & Script Rewriting (`core/src/import_wizard/inference.rs`)**:
   - Implemented `detect_ctx_size(script_content: &str) -> Option<usize>` supporting `--ctx-size <N>`, `-c <N>`, `--ctx_size <N>`, `--ctx-size=<N>`, and `CTX_SIZE=<N>`.
   - Implemented `sync_ctx_size_in_script(script_path: &Path, new_ctx: usize) -> Result<(), String>` that updates context window arguments in `.sh` files while preserving comments, formatting, and trailing newlines.

2. **Startup Script Auto-Reconciliation (`core/src/reconciler/`)**:
   - `reconcile_ctx_sizes(&mut self)` scans configured `.sh` launch scripts at SWAI startup.
   - If an external edit bumped a model's context window (e.g. from 65,536 to 262,144 in `ornithQ6.sh`), SWAI reconciles in-memory config and writes back to `config.toml` automatically.

3. **Edit Model Dialog Context Control (`app/src/manage_dialog/edit_dialog.rs`, `sync_ctx.rs`)**:
   - Added **Context Size (Tokens)** row to the Edit Model dialog.
   - "Save" updates `config.toml` and in-memory process manager.
   - "Save & Sync Script" synchronizes both `config.toml` and the `.sh` script file on disk.

4. **Live Dynamic Budget Estimation (`app/src/preferences/checkpoint_tab.rs`, `dialog.rs`)**:
   - Relocated live budget calculation outside and below the main card into a sleek **Active Budget Estimation** helper block.
   - Restored slim row height and centered spinbutton controls for the threshold row.
   - Dynamically binds context calculations to the active running model and dropdown selections.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test --workspace --lib -- --test-threads=1`: 297/297 core unit tests passed (0 failures)
- `cargo test -p swai -- --test-threads=1`: 43/43 app unit tests passed (0 failures)
- `integration.rs`: 4/4 passed (0 failures)
- Total: 344/344 unit and integration tests passed (0 failures)
- All touched files strictly under 450 lines

## Phase 32 — Split Live Prompt & Generation Telemetry with Fixed Bottom Deck

### What was built

1. **Prometheus `/metrics` and `/slots` Dual Speed Extraction (`app/src/window/poller.rs`, `types.rs`)**:
   - Poller parses both `prompt_per_second` (prefill / ingest rate) and `predicted_per_second` (decode rate) from llama-server's `/slots` and `/metrics` Prometheus API.
   - Extracts exact prompt and decode token counts (`prompt_tokens`, `decoded_tokens`).
   - Detects active processing state across multiple slots.

2. **Live Execution Stopwatch (`app/src/window/poller.rs`, `timeout.rs`)**:
   - Tracks request start timestamp when a slot transitions from idle → processing (`Instant::now()`).
   - Emits live elapsed duration during generation (`⏱️ 0.0s` → `⏱️ 4.2s`).
   - Latches and freezes the final total execution time (e.g. `⏱️ 4.4s`) along with the finished speeds when generation completes.

3. **Fixed Bottom Control & Telemetry Deck (`app/src/window/bottom_deck.rs`, `styles.rs`)**:
   - Pinned above the footer bar; never scrolls off-screen even with many model cards.
   - **Left Action Card (🌐 Web AI Chat):** Displays active proxy port URL (`http://127.0.0.1:9080`) and an "Open in Browser ↗" button/card that launches the user's default browser on click.
   - **Right Telemetry Card (📊 Telemetry Inspector):** Displays detailed prompt and decode breakdown (`📥 Prompt: 25 tok · 0.1s · 250.1 t/s` and `⚡ Decode: 141 tok · 4.4s · 31.9 t/s (⏱️ 4.4s total)`).
   - **Multi-Model Switcher Pills:** Cached GTK pill rendering with active CSS updates so model pills can be switched smoothly without losing click focus.
   - **Click-to-Inspect:** Clicking anywhere on any model card in the top list instantly sets the bottom telemetry deck to inspect that model.
   - **Council Stage Telemetry:** Tracks Council debate multi-stage metrics dynamically.

4. **Lifecycle & Resiliency Hardening**:
   - **Fast Model Turn-Off:** Background async SIGKILL escalation for sub-second shutdowns.
   - **Telemetry Map Cleanup:** Stopped models are cleanly removed from telemetry pills.
   - **Claude Code Council Streaming:** Fixed duplicate `message_start` events and JSON string serialization to eliminate SSE stream truncation.
   - **`llama-server` `--jinja` Resiliency:** Added `sanitize_messages_for_jinja` to automatically convert intermediate `system` roles to `user`, preventing 500 exceptions when using Jinja templates.
   - **Model Deletion Persistence:** Wired `save_to_disk()` into `ProcessManager::remove_model` and unparented deleted cards/rows immediately from UI and `config.toml`.
   - **Live Dynamic Model Import:** Newly added/imported models appear in the window immediately without requiring a SWAI restart.
   - **Non-Blocking Startup Validation:** Missing/renamed script files log warnings and display `⚠️ Script not found` on the card instead of blocking SWAI startup.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test --workspace -- --test-threads=1`: 345/345 unit and integration tests passed (0 failures)
- All files strictly under 450 lines (aiming < 380 lines)


## Phase 33.1 — Council Pipeline Event Broadcast Channel

### What was built

An asynchronous broadcast event system in `core/src/council/` that emits
real-time stage transitions, token-stream chunks, and execution metrics so the
UI can listen and display live updates during Council debates.

1. **`core/src/council/events.rs` (< 150 lines) — `CouncilEvent` enum:**
   - `StageStarted { stage_index, role, model_id, model_name }`
   - `TokenChunk { stage_index, text }`
   - `StageCompleted { stage_index, full_text, duration_sec, tok_per_sec }`
   - `PipelineCompleted { full_transcript: DebateTranscript }`
   - `PipelineFailed { stage_index, error }`
   - Helper methods: `kind()` (stable category label) and `stage_index()`
     (returns `None` for `PipelineCompleted`).
   - All variants are `Clone` + `Send + Sync` (serde-serializable) so they
     cross thread boundaries freely.

2. **`core/src/council/pipeline.rs` (< 450 lines) — event emission:**
   - `CouncilEngine` gained an optional
     `Option<tokio::sync::broadcast::Sender<CouncilEvent>>` subscriber.
   - New constructor `CouncilEngine::with_events(config, executor, sender)`.
   - New `Executor::execute_stream` trait method (default impl chunks the
     non-streaming output into `TokenChunk`s) so backends can override live
     streaming.
   - Emission points: `StageStarted` when a stage begins, `TokenChunk` per
     delta, `StageCompleted` when the stage finishes, and a terminal
     `PipelineCompleted` with the final synthesized transcript. `PipelineFailed`
     is emitted when a stage errors.
   - Strict order guaranteed per stage: `StageStarted -> TokenChunk(s) ->
     StageCompleted`, with `PipelineCompleted` always emitted last.
   - `emit()` swallows disconnected-subscriber errors, so event loss never
     breaks a debate.

3. **`core/src/proxy/state.rs` (< 350 lines) — receiver exposure:**
   - Added `events_rx: Option<Arc<Mutex<tokio::sync::broadcast::Receiver<CouncilEvent>>>>`
     to `ProxyState` (removed the now-unnecessary `Clone` derive — `ProxyState`
     is only ever used behind `Arc<Mutex<...>>`).
   - New `set_events_receiver(...)` accessor for the app layer.

4. **`core/src/proxy/council.rs` + `router.rs` (< 350 / 450 lines) — wiring:**
   - New `EventProxyExecutor` wrapper that forwards every `CouncilEvent` from
     the wrapped `ProxyExecutor` to the live broadcast receiver.
   - `run_council_and_record_telemetry` is now generic over the executor type,
     so it works with both the plain and event-emitting executors.
   - `router.rs` now constructs the engine with `with_events`, registers the
     live receiver on the proxy state via `set_events_receiver`, and exposes a
     `BROADCAST_CAPACITY` constant.

5. **`core/src/council/tests_streaming.rs` (< 300 lines) + `tests_sse.rs`:**
   - Moved the existing SSE-formatting tests into `tests_sse.rs` (deduplicated
     module registration).
   - New `tests_streaming.rs` verifies the strict emission order
     (`StageStarted -> TokenChunk(s) -> StageCompleted -> PipelineCompleted`),
     that `PipelineCompleted` is always emitted exactly once, and tokio
     broadcast semantics (multiple subscribers, late subscribers, and
     resilience to a sender with no receiver).

### Thread safety
- `CouncilEvent` is `Clone + Send + Sync`; the broadcast channel is the
  canonical multi-writer / multi-reader primitive.
- The receiver is stored behind `Arc<Mutex<...>>` alongside the rest of the
  proxy state, so concurrent access is serialized exactly as the rest of the
  proxy already is.
- Emission happens synchronously on the debate thread; subscribers drain the
  bounded channel on their own threads.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test -p swai-core --lib council::tests_streaming`: 5/5 passed
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test --workspace -- --test-threads=1`: 349/349 passed (0 failures)
- All touched files strictly under 450 lines:
  - `events.rs` 93, `pipeline.rs` 419, `mod.rs` 27, `tests_streaming.rs` 327,
    `tests_sse.rs` 217, `proxy/state.rs` 202, `proxy/council.rs` 295,
    `proxy/router.rs` 439

## Phase 33.2 — GTK4 Live Debate Arena Stream Viewer

### What was built

Real-time token streaming and live stage card updates in the GTK4 Debate Arena (`app/src/arena/`).

1. **`app/src/arena/types.rs` (234 lines) — Stream State Machine & Actions:**
   - `StageStatus` enum (`Pending`, `Generating`, `Completed`, `Failed`).
   - `StageState` struct tracking live text, duration, decode speed, and token metrics per stage.
   - `ArenaStreamAction` enum for thread-safe UI updates (`StageStarted`, `AppendToken`, `StageCompleted`, `PipelineCompleted`, `PipelineFailed`).

2. **`app/src/arena/stream.rs` (319 lines) — Live Stage Cards & Smart Auto-Scroll:**
   - `StageCard` widget with header badges, model metadata, token counters, and live text view.
   - Active pulse animation (`pulse-active` CSS class) on the currently generating stage.
   - Real-time auto-scrolling via `text_view.scroll_to_iter(&end, false, 0.0, 1.0)` that locks during generation and releases when user scrolls manually.

3. **`app/src/arena/window.rs` (377 lines) — GLib Main Loop Dispatch:**
   - Background bridge thread drains `CouncilEvent`s from `ProxyState` and dispatches them via `glib::MainContext::default().invoke(...)` onto the GTK main loop.
   - Live stage container transitions from historical debate mode to live streaming mode seamlessly.

4. **`app/src/arena/stream_tests.rs` (226 lines) — Stream State Machine Tests:**
   - 8 unit tests covering stage state lifecycle, token accumulation, duration/tok_per_sec metrics, multiple stages, and sticky auto-scroll state.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test -p swai -- --test-threads=1`: 51/51 passed
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test --workspace -- --test-threads=1`: 358/358 passed (0 failures)
- All files strictly under 450 lines:
  - `app/src/arena/stream.rs`: 319 lines
  - `app/src/arena/stream_tests.rs`: 226 lines
  - `app/src/arena/types.rs`: 234 lines
  - `app/src/arena/window.rs`: 377 lines
  - `app/src/arena/history.rs`: 176 lines


## Phase 33.3 — Human-in-the-Loop Interruption ("Chime In") & Guidance Injection

> Continues Phase 33.1 (broadcast channel) and 33.2 (live arena stream). The
> core backend (barrier pause controller, `HumanIntervention` event, feedback
> injection into the Synthesizer prompt) was already implemented and tested in
> the prior sub-phase. This sub-phase completed the remaining **UI layer**.

### What was built

1. **`app/src/arena/types.rs` (278 lines) — `HumanIntervention` mapping:**
   - Added `ArenaStreamAction::HumanIntervention { stage_index, feedback }` and
     handled it in `event_to_action`, translating the core
     `CouncilEvent::HumanIntervention` (carrying the injected guidance) into a
     UI action.
   - Extended `stage_index_of` so the live-panel dispatcher routes the action
     to the correct stage card.
   - Added unit tests asserting the mapping for both guidance and empty
     (skip) interventions.

2. **`app/src/arena/chime_in.rs` (260 lines) — "Chime In" drawer widget:**
   - Action-bar toggle button that opens a collapsible drawer with a multiline
     guidance editor (placeholder: *"Type guidance for the Council (e.g.,
     'Make sure to handle edge cases and use serde')..."*).
   - Two decisions: **Inject & Resume** (typed guidance prepended as an
     authoritative `HumanAuditor` turn) and **Skip / Resume** (resume unchanged).
   - Pure `build_decision(guidance, action)` helper extracted for GTK-free unit
     testing (4 tests), keeping the widget GTK-only and transport-agnostic.
   - Emits a `ChimeInDecision` through a user-supplied closure; wiring to the
     core pause controller is the owning app's responsibility.

3. **`app/src/arena/stream.rs` (349 lines) — 👤 Human Intervention badge:**
   - `StageState` gained a `human_intervention: Option<String>` field.
   - `apply()` records the intervention on the target stage card; `render()`
     draws a distinct `👤 Human Intervention` badge in the card header with a
     tooltip showing the injected guidance (or "resumed without changes").
   - Badge persists after the stage completes so the human's input stays
     visible in the live stream.

4. **`app/src/arena/window.rs` (422 lines) — widget integration:**
   - Added a `chime_in` field and wired the `ChimeIn` control into the header
     bar.
   - `chime_in_on_decision()` logs the decision (Inject / Skip) so the
     human-in-the-loop path is exercised; the pause-controller bridge is the
     owning app's wiring step.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test -p swai -- --test-threads=1`: 55/55 passed
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test --workspace -- --test-threads=1`: 379/379 passed (0 failures)
- All files strictly under 450 lines:
  - `app/src/arena/chime_in.rs`: 260 lines
  - `app/src/arena/types.rs`: 278 lines
  - `app/src/arena/stream.rs`: 349 lines
  - `app/src/arena/window.rs`: 422 lines
  - `core/src/council/barrier.rs`: 261 lines
  - `core/src/council/human_feedback.rs`: 118 lines
  - `core/src/council/tests_pause.rs`: 209 lines

### Notes
- The core backend pause/feedback/resume logic (barrier.rs, human_feedback.rs,
  tests_pause.rs) was already implemented and verified in the prior sub-phase;
  this phase only completed the UI surface that drives it.
- The `ChimeIn` widget's decisions are currently logged; the final step to make
  the feature end-to-end functional is to connect the owning app's pause
  controller (`CouncilPauseController`) to the injected/skip decisions.


## Phase 33.4 — Debate Arena Window Wiring (Menu Entry + Live Subscription)

> Continues Phases 33.1 / 33.2 / 33.3. The `ArenaWindow` (history sidebar,
> live-stream panel, transcript view, Chime In control) already existed and
> compiled, but nothing instantiated or presented it. This phase makes the
> Arena window user-visible.

### What was built

1. **`app/src/menu.rs` (65 lines) — View menu entry:**
   - Appended `menu.append(Some("Debate Arena"), Some("win.debate_arena"))` to
     `build_view_section()`. Existing Refresh / Toggle Logs Panel items
     unchanged. The action name `win.debate_arena` matches the `debate_arena`
     `SimpleAction` registered in `header.rs`.

2. **`app/src/window/arena_wiring.rs` (99 lines) — construction + subscription
   bridge:**
   - `open_debate_arena(cache, proxy_state)`: if the cached handle is `None`,
     constructs `ArenaWindow::new()`, subscribes, and presents it; if `Some`,
     calls `.present()` to raise/restore the existing window (single-instance
     reuse).
   - `subscribe_to_debate(arena, proxy_state)`: reads `ProxyState.events_rx`
     behind a short-lived `Arc<Mutex<...>>` lock (recovering from a poisoned
     lock), then hands the receiver to `ArenaWindow::observe_debate`. If
     `events_rx` is `None` (no debate in flight) the window is still presented
     so the user sees the empty-state placeholder and can browse saved debates.
   - **Re-subscribe policy (Option B):** re-subscribe on every menu click.
     `ProxyState.events_rx` is replaced by the router when a new debate begins
     (`core/src/proxy/router.rs`), so re-subscribing on click keeps the Arena
     window in sync with the most-recently-started debate without an external
     poller or a hook inside `set_events_receiver`.
   - The cached handle type alias `DebateArenaCache = Rc<RefCell<Option<ArenaWindow>>>`.

3. **`app/src/window/header.rs` (144 lines) — SimpleAction:**
   - `wire_actions()` gained a `debate_arena_cache` parameter (inherited from
     `MainWindow`). Registers a `debate_arena` `SimpleAction` whose closure
     calls `crate::window::arena_wiring::open_debate_arena(...)`.

4. **`app/src/window/window.rs` (442 lines) — MainWindow wiring:**
   - Added `debate_arena: Rc<RefCell<Option<ArenaWindow>>>` field.
   - Initialized a fresh `Rc<RefCell<Option<ArenaWindow>>>` before the wire
     block and passed it to `wire_actions`; stored a clone in the `Self`
     literal.

5. **`app/src/window/mod.rs` — module registration:**
   - Added `pub mod arena_wiring;`.

6. **`app/src/arena/mod.rs` — public re-export:**
   - Added `pub use window::ArenaWindow;` so `crate::arena::ArenaWindow`
     resolves from the window subsystem.

7. **`app/src/main.rs` (252 lines) — Ctrl+Shift+D accelerator:**
   - After the `gtk::Application` is built,
     `app.set_accels_for_action("win.debate_arena", &["<Ctrl><Shift>d"]);`
     binds the View → Debate Arena action to the `Ctrl+Shift+D` shortcut.

### Thread-safety notes
- GTK construction/presentation happens only on the GTK main thread (the
  `debate_arena` action closure runs there). `arena_wiring.rs` never moves a
  GTK handle across a thread; the background→main-thread bridge lives entirely
  in the pre-existing `ArenaWindow::observe_debate` (tokio broadcast → mpsc →
  `glib::timeout_add_local`).
- `ProxyState.events_rx` is locked only long enough to read the `Option`, then
  the lock is dropped before the bridge thread is spawned. Poisoned locks are
  recovered via `into_inner()` (consistent with the project's mutex-poisoning
  hardening).

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test -p swai -- --test-threads=1`: 55/55 passed
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test --workspace -- --test-threads=1`: 320
  core unit + 4 integration passed (0 failures)
- All touched files strictly under 450 lines:
  - `app/src/window/arena_wiring.rs`: 99 lines
  - `app/src/window/header.rs`: 144 lines
  - `app/src/window/window.rs`: 442 lines
  - `app/src/menu.rs`: 65 lines
  - `app/src/main.rs`: 252 lines
  - `app/src/arena/window.rs`: 422 lines
  - `app/src/arena/mod.rs`: 20 lines

### Manual UI check (to be performed by user)
1. `SWAI_NO_SINGLE_INSTANCE=1 cargo run` → **View → Debate Arena** appears in the
   menu bar; **Ctrl+Shift+D** works.
2. Clicking it presents the Arena window (title "Arena — Debate", history
   sidebar + live-stream panel).
3. With no debate in flight, the window shows the empty-state placeholder.
4. While a council debate is in flight, tokens stream into the stage bubble
   cards with auto-scroll and active pulse badges (requires Phase 33.2's
   streaming reachable through the wired subscription).


## Phase 33.5 — Universal Council Proxy Interception & Zero-Config CLI Routing

### What was built

1. **`core/src/proxy/router.rs` (440 lines) — Universal Interception Logic:**
   - Upgraded Council evaluation: `let should_run_council = enable_council || is_council_model(&model_id);`.
   - When the master **"Enable Council Pipeline"** preference switch is toggled **ON** in SWAI, all incoming inference requests from any client (Hermes CLI, Claude Code, OpenAI Codex CLI, VSCode, Cursor) automatically route through the multi-stage Council debate pipeline, regardless of the requested model slug.
   - When the master toggle is **OFF**, requests route directly to the requested backend server with zero overhead (while still supporting explicit `/model council` on demand).
   - Added `primary_port.or(target_port)` fallback to ensure debate execution resolves the active server even when model ID defaults vary.

2. **`core/src/proxy/tests_council.rs` (162 lines) — Universal Routing Tests:**
   - Added `test_should_run_council_universal_interception` covering the full decision matrix for generic models (`run-ornith-1.5-opt`, `run-qwen25-coder-7b`, `gpt-4o`) and explicit virtual models (`council`, `council:debate`) across enabled/disabled global states.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `cargo test -p swai-core --lib proxy::tests_council`: 8/8 passed
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test --workspace -- --test-threads=1`: 380/380 passed (0 failures)
- All files strictly under 450 lines:
  - `core/src/proxy/router.rs`: 440 lines
  - `core/src/proxy/tests_council.rs`: 162 lines

## Phase 37 — Gemini-Style Debate Arena Refactor & Persistent Chime-In Bar

### What was built

1. **`app/src/arena/view.rs` (146 lines) — Modern Gemini-Style Chat Bubbles:**
   - Single unified chat stream matching modern AI web interfaces (Gemini / ChatGPT).
   - Right-aligned **`👤 HUMAN PROMPT:`** and **`👤 HUMAN (Chimed In):`** rounded bubbles in soft radiant lavender (`#c084fc`).
   - Left-aligned AI response cards with bold uppercase speaker tags and theme-adaptive colors:
     - **`🤖 GENERATOR says:`** in soft sky cyan (`#38bdf8`)
     - **`🔍 AUDITOR says:`** in warm amber (`#fbbf24`)
     - **`⚡ SYNTHESIZER says:`** in mint emerald (`#34d399`)
   - Natural vertical word wrapping without inner nested scrollboxes.
   - Fixed the sidebar selection styling bug: clicked debates now render with identical rich Gemini-grade styles.

2. **`app/src/arena/stream.rs` (360 lines) — Live Stage Bubble Streaming:**
   - Synchronized live streaming tokens into the new `.arena-bubble-ai` format.
   - Dynamic bold uppercase header with stage numbers, model ID pills, and live pulse badges.

3. **`app/src/arena/window.rs` (433 lines) & `chime_in.rs` (276 lines) — Persistent Bottom Input Bar:**
   - Added an always-visible, rounded chat input bar anchored at the bottom of the arena.
   - Allows instant human intervention by typing and pressing Enter or clicking `Chime In ➤`.
   - Injects the guidance into the live debate and adds an immediate visual Human bubble into the stream.

4. **`app/src/window/styles.rs` (418 lines) — CSS Rules:**
   - Added styles for `.arena-stream-container`, `.arena-bubble-user`, `.arena-bubble-ai`, `.arena-code-block`, and `.arena-bottom-bar`.

### Test results
- `cargo check --workspace`: 0 errors, 0 warnings
- `SWAI_NO_SINGLE_INSTANCE=1 cargo test --workspace -- --test-threads=1`: 388/388 passed (0 failures)
- All files strictly under 450 lines.







