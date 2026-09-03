# SWAI: Future Feature Ideas & Distribution Roadmap

This document tracks upcoming planned phases and future distribution/packaging ideas for post-v1.0 iterations.

---

## ⚡ TOP PRIORITY — Dynamic Model-Adaptive Context Compaction Budget

### Problem:
Different local models have drastically different context window capacities (e.g. 32k, 64k, 128k, 256k tokens). A single hardcoded compaction budget (e.g. 60k chars / ~16k tokens) is ideal for 64k models, but either overly restrictive for large-context models (128k/256k) or too loose for small 32k models.

### Proposed Architecture & Solution:
1. **Dynamic Context Scaling Ratio**:
   - Inspect the active model's configured `--ctx-size` (or `ctx_size` in `config.toml`).
   - Automatically compute the safe history compaction budget as a percentage of the total context window (e.g., default `25%` to `35%` of context window for conversation history, leaving `65%` to `75%` headroom for KV cache, system prompt, tools, and generation).
2. **Proportional Allocation Table**:
   - **32k context model**: History budget = ~10k tokens (~38k chars), leaving 22k tokens headroom.
   - **64k context model** (e.g., Ornith-35B): History budget = ~20k tokens (~75k chars), leaving 44k tokens headroom.
   - **128k context model** (e.g., Qwen-2.5-Coder / DeepSeek): History budget = ~45k tokens (~170k chars), leaving 83k tokens headroom.
   - **256k+ context model**: History budget = ~90k tokens (~340k chars), leaving 166k tokens headroom.
3. **Adaptive `tool_result` Truncation**:
   - Scale the per-tool-result truncation limit proportionally to the context budget (e.g., `min(ctx_tokens / 16, 16_000)` tokens per file read).
4. **Validation & Rollout**:
   - Test under real multi-step tasks across varying context windows in Phase 25/26.

---

## 🚀 Active Roadmap Summary (Phases 23 – 29)

- **Phase 23 — Multi-Model Concurrent Orchestration**: Run up to 4 models simultaneously with payload-based dynamic proxy routing.
- **Phase Between 23-24 — Context Checkpointing & Summarized Compaction**: Replace silent message eviction with session checkpoint summaries injected after system prompt.
- **Phase 24 — SWAI Council: Inter-Model Debate Broker & Synthetic Endpoint**: Multi-model peer review, critique, and synthesis pipeline with real-time GTK debate arena (sub-phases 24a–24g).
- **Phase Between 24-25 — Dynamic Model-Adaptive Context Compaction Budget**: Dynamically compute history compaction budgets and tool truncation limits based on the active model's configured context window (32k, 64k, 128k, 256k).
- **Phase 25 — Native Windows Port (WinUI 3 & System Tray)**: Bring native SWAI desktop experience to Windows.
- **Phase 26 — Native macOS Port (Cocoa & Menu Bar Item)**: Bring native SWAI desktop experience to macOS.
- **Phase 27 — Real-Time Hardware Telemetry (System RAM & Multi-Vendor VRAM Monitor)**: Display live System RAM and GPU VRAM memory usage across NVIDIA, AMD, Intel, and Apple Silicon.
- **Phase 28 — Cross-Platform Store & Package Manager Distribution**: Microsoft Store, `winget` repo, Homebrew Cask (`brew install`), and standalone `.dmg` packaging.
- **Phase 29 — Sidebar Preferences Redesign & UI Visual Polish**: KDE/GNOME-style sidebar tab navigation with preference search and UI visual polish.

---

## 📦 Future Distribution & Package Manager Ideas

### 1. Windows Distribution & Package Managers (Phase 25 Extension)
- **Microsoft Store (Win32 / MSIX)**: Publish free native WinUI 3 package directly to the Microsoft Store.
- **Windows Package Manager (`winget`)**: Submit manifest to official `microsoft/winget-pkgs` repository so users can install via single command:
  ```cmd
  winget install verdioso.swai
  ```
- **NSIS / WiX Installer**: Standalone setup executable (`SWAI-Setup-v1.x.x.exe`) with auto-updater support.

### 2. macOS Distribution & Package Managers (Phase 26 Extension)
- **Homebrew Cask Formula (`brew install`)**: Create official tap (`verdioso/tap`) so Mac developers can install SWAI instantly without Apple App Store fees:
  ```bash
  brew install verdioso/tap/swai
  ```
- **Standalone `.dmg` Disk Image**: Provide signed/notarized `.dmg` drag-and-drop installer for direct download from GitHub Releases.

---

## 🧹 Maintenance & Storage Management Ideas

### One-Click Log & Checkpoint Cleanup in Preferences
Add dedicated storage cleanup actions inside the Preferences dialog (System / Storage section) to let users easily manage and reclaim disk space:
- **Button 1: "Clear All Logs"**:
  - Deletes or truncates all model server and process logs in `~/.local/share/swai/logs/` (or `$XDG_DATA_HOME/swai/logs/`).
  - Shows a brief confirmation toast / banner upon completion.
- **Button 2: "Clear All Checkpoints"**:
  - Deletes all session checkpoint markdown files in `~/.local/share/swai/checkpoints/` (or `$XDG_DATA_HOME/swai/checkpoints/`).
  - Automatically resets any open Checkpoint tab in the Log Viewer to "No checkpoints recorded yet".
  - Shows a brief confirmation toast / banner upon completion.

---

## 🤖 Two-Model Architecture: Background Scribe & Diff-Grounded Milestone Ledger

### Original Concept:
> *"what I want to say is: Lets say I have ornith 35B as master/coding model. Then beside him, I load one more, small model who only reads what coding model writes (not what he reads) and summarize that into single file via SKILL (or something, whatever). Whenever coding model gets lost or gets trapped into loop, council point him to that file to read what was done, so coding model knows where he stopped. Makes sense or not? And this would be available only if user actually load 2 models."*

### Refined Architecture & Design Decisions:

#### 1. Diff-Grounded Summarization (Deterministic Input vs Hallucination)
- **Never summarize chat transcripts**: Models can hallucinate progress in their conversational text before writing code.
- **Feed actual disk diffs**: SWAI captures deterministic `git diff` deltas and `write_to_file` / `replace_file_content` events, passing only genuine code modifications to the Scribe.
- Result: 100% grounded truth without hallucinated milestones.

#### 2. Structured Ledger Schema
The Scribe outputs to a strict, structured markdown schema:
```markdown
# Session Milestone Ledger (Verified by Scribe)

## Completed Changes
- `core/src/proxy/router.rs`: Added `is_council_model()` and `parse_pipeline_header()`.
- `app/src/arena/view.rs`: Created visual cards for Draft, Critique, and Consensus.

## Current Active Blocker
- Compiler error on `app/src/arena/view.rs:97`: Mismatched `gtk4::WrapMode` vs `pango::WrapMode`.

## Last Verified Working State
- `swai-core` unit tests: 259/259 passed.
```

#### 3. Two-Model Execution & Zero GPU VRAM Penalty
```
┌────────────────────────────────────────────────────────┐
│                      SWAI PROXY                        │
│                                                        │
│  [Ornith 35B (GPU)] ───Writes Code───► [Filesystem]   │
│           │                                  │         │
│   (When stuck in loop)                    git diff     │
│           ▲                                  ▼         │
│           │ Injects Ledger            [1.5B/3B (CPU)]  │
│           └─────────────────────────────── Scribe      │
└────────────────────────────────────────────────────────┘
```
- **CPU Offloading**: The 1.5B/3B Scribe can run entirely in CPU RAM (`--ngl 0`), using ~1.5 GB system memory and **0 MB GPU VRAM**, keeping 100% of GPU resources available for the 35B master model.
- **Passive Operation**: Scribe runs asynchronously in the background with zero latency penalty on master turns.

#### 4. Event Triggers & State Transition Lifecycle
SWAI triggers Scribe updates on three discrete deterministic event channels:
1. **`FileWriteEvent` (File Created / Edited)**:
   - Captures `git diff` delta of touched file.
   - Appends/updates `[Completed Changes]` section.
   - Automatically marks any active blocker referencing that file as *under resolution*.
2. **`CommandResultEvent` (Exit Code > 0 / Compiler Error)**:
   - Extracts exact error code and target file:line (e.g. `error[E0515]: cannot return value referencing function parameter 'w' on window.rs:119`).
   - Updates `[Current Active Blocker]` with the latest 2–3 failure items.
3. **`TestPassEvent` (Exit Code == 0 on Build/Test)**:
   - Parses deterministic test counter (e.g. `259 passed; 0 failed`).
   - Updates `[Last Verified Working State]` with commit/time and test count.
   - Clears `[Current Active Blocker]`.

#### 5. Invocation Strategy: Per-Event Streaming vs On-Demand
- **Per-Event Incremental Updates (Selected)**: Rather than re-parsing the entire diff history when a loop occurs, SWAI pushes tiny async event snippets (~50 tokens each) to the Scribe queue on every write/test event.
- **Instant Intervention**: When the Loop Breaker trips (5+ turns without writes), the ledger file is already 100% pre-compiled on disk and ready for immediate, zero-latency injection into the master model's prompt.

#### 6. Authoritative Loop Breaker Intervention
When SWAI's deterministic loop heuristics trip (5+ turns without file writes, or repeated read cycles), the proxy injects:
```markdown
<system-reminder>
⚠️ LOOP DETECTED: You have made no code modifications for 5 turns.
Here is your verified milestone ledger from the filesystem:

- Completed: Created ArenaWindow, history.rs, view.rs
- Active Blocker: Fix compile error on window.rs:119 (use downcast::<Label>().ok())
- Last Verified: cargo test -p swai-core (259/259 passed)

DO NOT re-read existing files. Trust this ledger and proceed directly with your implementation.
</system-reminder>
```

#### 7. Graceful Fallback
- **If 2 models are active**: Scribe provides real-time diff-grounded milestone tracking on CPU RAM.
- **If 1 model is active**: SWAI uses the rule-based Loop Breaker.

---

*(Completed phases 1–22 and retired Phase 21 are documented in `PROGRESS.md`).*

---

## ⚡ UI Dashboard: Split Live Prompt/Generation Telemetry

### Problem:
Currently, the UI only displays the generation speed (`predicted_per_second` / "tok/s") in the ModelCard. The user cannot natively see the prompt evaluation speed inside SWAI, even though `prompt_per_second` is successfully extracted from the `llama-server` `/v1/slots` API payload in `core/src/proxy.rs` and `app/src/window/poller.rs`.

### Proposed Architecture & Solution:
1. **ModelCard UI Upgrade (`app/src/model_card/view.rs`)**:
   - Add a small secondary `gtk::Label` below or beside the generation speed label.
   - Use a muted, dim CSS class (e.g., `dim-label`) so it doesn't clutter the primary telemetry.
2. **Data Pipeline Integration**:
   - Update `ChannelMessage::SlotUpdate` to carry `prompt_per_second` alongside `predicted_per_second`.
   - Implement `set_prompt_speed(&self, prompt_speed: f64)` in the `ModelCard` struct.
3. **Behavioral Triggers**:
   - Only display the label if `prompt_speed > 0.0`.
   - Hide or fade the label when generation finishes.

---

## 🎛️ Optional Checkpointing Toggle (Preferences)

### Problem:
Context checkpointing (saving a milestone ledger of the agent's progress to prevent loops and context exhaustion) is incredibly valuable for smaller context models (32k–64k). However, for users running massive context models (like 128k or 262k), the risk of running out of context or looping is significantly lower, making the checkpointing system unnecessary overhead.

### Proposed Solution:
1. **Global Checkpoint Toggle (`app/src/preferences/`)**:
   - Add a `Enable Context Checkpointing` switch inside the Preferences (or Checkpointing tab).
   - Backed by a new `enable_checkpointing = true/false` field in the `config.toml` global section.
2. **Proxy Bypass (`core/src/proxy/`)**:
   - When the LLM calls a file-write tool (`write_to_file`, `replace_file_content`, etc.), the proxy intercepts it to generate a milestone diff.
   - If `enable_checkpointing == false`, bypass this interception completely, significantly speeding up the proxy pipeline and saving the CPU Scribe workload.
3. **Loop Breaker Disabling**:
   - Tie the Loop Breaker heuristic logic to this toggle. If disabled, the proxy acts as a pure passthrough router without any loop intervention.

---

## 💬 Live Council Debate Stream

### Problem:
The Council Pipeline successfully saves Markdown transcripts to the disk after the debate finishes, but users have no visibility into the debate *while it is happening*. For long, multi-stage generation and auditing cycles, the user just waits blankly until the final text streams to their client.

### Proposed Solution:
1. **Live Debate Chat UI (`app/src/arena/`)**:
   - Create a real-time UI component (the "Debate Arena") in the GTK app where the user can watch the Generator draft and the Auditor critiques stream live.
2. **Proxy State Broadcast (`core/src/proxy/`)**:
   - Instead of isolating the `CouncilEngine` purely behind the `tiny_http` response thread, the engine can emit live progression events (e.g., `Generator started`, `Auditor 1 critiquing`) through a broadcast channel back to the UI.
3. **Visual Feedback**:
   - Render models as distinct "chat heads" or columns, visualizing the dialogue exactly as it occurs, similar to a group chat among AI peers.
4. **Human-in-the-Loop Interruption**:
   - Add an "Interrupt / Chime In" button to the Debate Arena. This allows the user to pause the pipeline, type their own critique or proposal (e.g., *"Llama makes a good point, but also make sure you use a HashMap for performance"*), and inject it as an authoritative "Human Auditor" turn before the Synthesizer finalizes the code.

---

## ⚖️ Council Pipeline Master Toggle (Preferences / Bypass)

### Problem:
Users frequently run multiple concurrent models in SWAI (e.g., `max_concurrent_models = 2+`) for independent multi-client setups—such as running Claude Code on Model A while simultaneously running Cursor or another tool on Model B. Currently, running multiple models can imply Council debate arbitration, even when the user only wants concurrent independent routing without any synthetic multi-agent debate overhead.

### Proposed Architecture & Solution:
1. **Master Council Switch in Preferences (`app/src/preferences/`)**:
   - Add a global `Enable Council Pipeline` toggle switch inside the **Council Pipeline** preferences tab.
   - Backed by `enable_council = true/false` in `[council]` or `[preferences]` section of `config.toml`.
2. **Proxy Bypass & Transparent Multi-Model Routing (`core/src/proxy/`)**:
   - When `enable_council == false`:
     - Bypasses any synthetic Council interception (`swai-council` endpoint or `x-swai-pipeline` headers).
     - Directly routes incoming requests to the target model specified in the request payload (`model: "<model-id>"`).
     - Allows pure, concurrent multi-model operation without triggering auditor critiques or synthesizer rounds.
3. **UI Visual State**:
   - When the toggle is OFF, dim or disable the Council-specific configuration rows in the Preferences dialog to make it obvious that the debate pipeline is dormant.

---

## 🔁 Post-Debate Arena Human Follow-Up & Continuous Iteration

### Problem:
When the Synthesizer finishes its job and concludes the debate session (shipping the final code back to the client), the Council session closes. If the human operator writes in the Debate Arena chat field after the debate has finished, the message displays visually, but no model responds because no active pipeline session is listening.

### Proposed Architecture & Solution:
1. **Interactive Post-Debate Continuation (`app/src/arena/window.rs`)**:
   - When the user types and sends guidance into the bottom chat field while NO debate is currently running (pipeline has completed):
   - Rather than dropping into a void, trigger a new follow-up Council cycle directly from the Arena UI!
2. **Auditor Re-Engagement & Steering (`core/src/council/`)**:
   - The **Auditor** receives the full conversation history + the previous Synthesizer code output + the new human message.
   - The Auditor either continues where it stopped, reviews the human's new requirements, or provides concrete critique/instructions for the Synthesizer.
   - The **Synthesizer** runs right after the Auditor, modifying and updating the existing code on disk to satisfy the human's post-debate request.
3. **Full Multi-Turn Chat Experience**:
   - Turns the Debate Arena into an ongoing, persistent multi-turn collaborative workspace where the human can continuously talk to their Council before, during, AND after coding tasks!
