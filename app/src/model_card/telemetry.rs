use gtk::prelude::*;
use gtk4 as gtk;

use super::types::PollingState;
use super::view::ModelCard;

impl ModelCard {
    /// Update the context usage display and polling state.
    ///
    /// Called from the main thread (via `glib::MainContext::default().invoke()`)
    /// when a new /slots response is received.
    ///
    /// Renders context as a 4px GtkProgressBar with 4-tier coloring:
    ///   - Green  (#4ade80) for 0–40%
    ///   - Cyan   (#2dd4f0) for 41–75%
    ///   - Orange (#f59e0b) for 76–89%
    ///   - Red    (#ef4444) for 90–100%
    ///
    /// Also displays the active session cumulative tokens used (e.g. "TOTAL: 1,547 tokens used").
    pub fn set_context(&self, tokens_used: usize, n_ctx: usize, total_tokens: usize) {
        self.block_signals();

        // Update polling state.
        *self.polling_state.borrow_mut() = PollingState::Active { tokens_used, n_ctx };

        // Calculate percentage for the progress bar.
        let percentage = if n_ctx > 0 {
            tokens_used as f64 / n_ctx as f64
        } else {
            0.0
        };
        self.context_bar.set_fraction(percentage.min(1.0));

        // 4-tier color: pick the CSS class for progress bar and label.
        let (bar_class, label_class) = if percentage >= 0.90 {
            ("ctx-red", "ctx-text-red")
        } else if percentage >= 0.76 {
            ("ctx-orange", "ctx-text-orange")
        } else if percentage >= 0.41 {
            ("ctx-cyan", "ctx-text-cyan")
        } else {
            ("ctx-green", "ctx-text-green")
        };

        self.context_bar
            .set_css_classes(&["progressbar", bar_class]);

        // Helper to format integers with thousands separators (e.g. 1,547).
        let fmt = |n: usize| -> String {
            let s = n.to_string();
            let chars: Vec<char> = s.chars().rev().collect();
            let mut result = String::new();
            for (i, ch) in chars.iter().enumerate() {
                if i > 0 && i % 3 == 0 {
                    result.push(',');
                }
                result.push(*ch);
            }
            result.chars().rev().collect::<String>()
        };

        // Format the context label: "32,763 / 262,144 tokens (12.5%)".
        let text = format!(
            "{} / {} tokens ({:.1}%)",
            fmt(tokens_used),
            fmt(n_ctx),
            percentage * 100.0
        );
        self.context_label.set_text(&text);
        self.context_label
            .set_css_classes(&["caption", label_class]);

        // Format the session total tokens used: "TOTAL: x,xxx tokens used".
        if total_tokens > 0 {
            self.total_tokens_label.set_markup(&format!(
                "<b>TOTAL: {} tokens used</b>",
                fmt(total_tokens)
            ));
            self.total_tokens_label.set_visible(true);
        } else {
            self.total_tokens_label.set_text("");
            self.total_tokens_label.set_visible(false);
        }

        self.unblock_signals();
    }

    /// Reset context display (clear progress bar and return to dim state).
    pub fn clear_context(&self) {
        self.block_signals();
        *self.polling_state.borrow_mut() = PollingState::Inactive;
        self.context_bar.set_fraction(0.0);
        self.context_label.set_text("");
        self.total_tokens_label.set_text("");
        self.total_tokens_label.set_visible(false);
        self.unblock_signals();
    }
    /// Set the live generation speed label (e.g., "⚡ 41.5 tok/s").
    ///
    /// Called from the main thread when a new /slots response includes
    /// predicted_per_second. The label is shown only when the model is Ready.
    pub fn set_speed(&self, speed: f64) {
        self.block_signals();

        if speed > 0.0 {
            let text = format!("⚡ {:.1} tok/s", speed);
            self.speed_label.set_text(&text);
            self.speed_label
                .set_css_classes(&["caption", "speed-label"]);
            self.speed_label.set_visible(true);
        }

        self.unblock_signals();
    }

    /// Set the live prompt evaluation speed label (e.g., "📥 450.2 p-tok/s").
    ///
    /// Called from the main thread when a new /slots response includes
    /// prompt_per_second. The label is shown only when the model is Ready.
    pub fn set_prompt_speed(&self, prompt_per_second: f64) {
        self.block_signals();

        if prompt_per_second > 0.0 {
            let text = format!("📥 {:.1} p-tok/s", prompt_per_second);
            self.prompt_speed_label.set_text(&text);
            self.prompt_speed_label
                .set_css_classes(&["caption", "prompt-speed-label"]);
            self.prompt_speed_label.set_visible(true);
        }

        self.unblock_signals();
    }

    /// Set the live stopwatch label (e.g., "⏱ 4.2s").
    ///
    /// Called from the main thread when a SlotUpdate includes
    /// elapsed_duration_sec. The label shows live time during generation
    /// and latches the final time when generation completes.
    pub fn set_stopwatch(&self, elapsed_duration_sec: Option<f64>) {
        self.block_signals();

        if let Some(duration) = elapsed_duration_sec {
            let text = format!("⏱ {:.1}s", duration);
            self.stopwatch_label.set_text(&text);
            self.stopwatch_label
                .set_css_classes(&["caption", "stopwatch-label"]);
            self.stopwatch_label.set_visible(true);
        }

        self.unblock_signals();
    }

    /// Clear the prompt speed label (used when model stops or enters transitional state).
    pub fn clear_prompt_speed(&self) {
        self.block_signals();
        self.prompt_speed_label.set_text("");
        self.prompt_speed_label
            .set_css_classes(&["dim-label", "caption"]);
        self.prompt_speed_label.set_visible(false);
        self.unblock_signals();
    }

    /// Clear the stopwatch label (used when model stops or enters transitional state).
    pub fn clear_stopwatch(&self) {
        self.block_signals();
        self.stopwatch_label.set_text("");
        self.stopwatch_label
            .set_css_classes(&["dim-label", "caption"]);
        self.stopwatch_label.set_visible(false);
        self.unblock_signals();
    }

    /// Clear the speed label (used when model stops or enters transitional state).
    pub fn clear_speed(&self) {
        self.block_signals();
        self.speed_label.set_text("");
        self.speed_label.set_css_classes(&["dim-label", "caption"]);
        self.speed_label.set_visible(false);
        self.unblock_signals();
    }
}
