use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use swai_core::process_manager::ProcessManager;
use swai_core::proxy::ProxyState;

use super::types::{ChannelMessage, SlotInfo, SlotUpdate};
use super::watchdog::trigger_auto_restart;

pub fn spawn_context_poller(
    pm: Arc<Mutex<ProcessManager>>,
    sender: std::sync::mpsc::Sender<ChannelMessage>,
    slot_sender: std::sync::mpsc::Sender<SlotUpdate>,
    auto_restart_enabled: bool,
    proxy_state: Option<Arc<Mutex<ProxyState>>>,
    enable_notifications: bool,
) {
    let http_client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .expect("failed to build reqwest blocking client");

    let poll_running = Arc::new(AtomicBool::new(true));
    let poll_running_clone = Arc::clone(&poll_running);

    std::thread::spawn(move || {
        let mut last_poll_attempt = std::time::Instant::now();
        let mut last_tokens: std::collections::HashMap<String, (usize, std::time::Instant)> =
            std::collections::HashMap::new();

        // ── Live Execution Stopwatch ────────────────────────────
        let mut request_start: std::collections::HashMap<String, std::time::Instant> =
            std::collections::HashMap::new();
        let mut last_was_processing: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        let mut session_total_tokens: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        while poll_running_clone.load(Ordering::SeqCst) {
            for _ in 0..20 {
                if !poll_running_clone.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            if last_poll_attempt.elapsed() < std::time::Duration::from_secs(1) {
                continue;
            }

            let ready_ports: Vec<(String, u16)> = match pm.lock() {
                Ok(pm_lock) => pm_lock.running_model_ports(),
                Err(_) => continue,
            };

            for (model_id, port) in &ready_ports {
                let url = format!("http://127.0.0.1:{}/slots", port);

                match http_client.get(&url).send() {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            if let Ok(body) = resp.text() {
                                if let Some(slot_info) = parse_slots_response(&body) {
                                    let now = std::time::Instant::now();
                                    let mut calculated_speed = slot_info.predicted_per_second;
                                    let mut prompt_speed = slot_info.prompt_per_second;
                                    let mut metrics_tokens: Option<usize> = None;

                                    // Check Prometheus /metrics endpoint on llama-server
                                    let metrics_url = format!("http://127.0.0.1:{}/metrics", port);
                                    if let Ok(m_resp) = http_client.get(&metrics_url).send() {
                                        if m_resp.status().is_success() {
                                            if let Ok(m_body) = m_resp.text() {
                                                let (p_speed, g_speed, m_tot) =
                                                    parse_metrics_response(&m_body);
                                                if prompt_speed == 0.0 && p_speed > 0.0 {
                                                    prompt_speed = p_speed;
                                                }
                                                if calculated_speed == 0.0 && g_speed > 0.0 {
                                                    calculated_speed = g_speed;
                                                }
                                                metrics_tokens = m_tot;
                                            }
                                        }
                                    }

                                    // Fallback: Delta Token Speedometer
                                    if calculated_speed == 0.0 {
                                        if let Some(&(prev_tokens, prev_time)) =
                                            last_tokens.get(model_id)
                                        {
                                            let dt = now.duration_since(prev_time).as_secs_f64();
                                            if dt > 0.3 && slot_info.tokens_used >= prev_tokens {
                                                let delta_tokens =
                                                    slot_info.tokens_used - prev_tokens;
                                                if delta_tokens > 0 {
                                                    calculated_speed = delta_tokens as f64 / dt;
                                                }
                                            }
                                        }
                                    }
                                    last_tokens
                                        .insert(model_id.clone(), (slot_info.tokens_used, now));

                                    // ── Live Execution Stopwatch Logic ──────
                                    let was_processing =
                                        last_was_processing.get(model_id).copied().unwrap_or(false);
                                    let is_processing = slot_info.is_processing;

                                    if is_processing && request_start.get(model_id).is_none() {
                                        request_start.insert(model_id.clone(), now);
                                    }

                                    let elapsed_duration = if is_processing {
                                        // Still processing — calculate live elapsed time
                                        request_start
                                            .get(model_id)
                                            .map(|start| start.elapsed().as_secs_f64())
                                    } else if was_processing {
                                        // Just finished processing — latch the final duration
                                        request_start
                                            .remove(model_id)
                                            .map(|start| start.elapsed().as_secs_f64())
                                    } else {
                                        None
                                    };

                                    // ── Active Session Cumulative Tokens Tracking ──
                                    if let Some(m_tot) = metrics_tokens {
                                        session_total_tokens.insert(model_id.clone(), m_tot);
                                    } else if was_processing && !is_processing {
                                        let prev_tok =
                                            last_tokens.get(model_id).map(|(t, _)| *t).unwrap_or(0);
                                        session_total_tokens
                                            .entry(model_id.clone())
                                            .and_modify(|t| *t += prev_tok)
                                            .or_insert(prev_tok);
                                    }

                                    let committed =
                                        session_total_tokens.get(model_id).copied().unwrap_or(0);
                                    let total_session_tokens = if metrics_tokens.is_some() {
                                        committed
                                    } else if is_processing {
                                        committed + slot_info.tokens_used
                                    } else {
                                        committed
                                    };

                                    last_was_processing.insert(model_id.clone(), is_processing);

                                    let _ = slot_sender.send(SlotUpdate {
                                        model_id: model_id.clone(),
                                        tokens_used: slot_info.tokens_used,
                                        n_ctx: slot_info.n_ctx,
                                        predicted_per_second: calculated_speed,
                                        prompt_per_second: prompt_speed,
                                        prompt_tokens: slot_info.prompt_tokens,
                                        decoded_tokens: slot_info.decoded_tokens,
                                        is_processing: slot_info.is_processing,
                                        elapsed_duration_sec: elapsed_duration,
                                        total_tokens: total_session_tokens,
                                    });

                                    if auto_restart_enabled
                                        && slot_info.n_ctx > 0
                                        && (slot_info.tokens_used as f64 / slot_info.n_ctx as f64)
                                            >= 0.98
                                    {
                                        trigger_auto_restart(
                                            &pm,
                                            &sender,
                                            &slot_sender,
                                            &proxy_state,
                                            model_id,
                                            enable_notifications,
                                            slot_info.n_ctx,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            "slots poll failed for model '{}' on port {}: {}",
                            model_id,
                            port,
                            e
                        );
                    }
                }
            }

            last_poll_attempt = std::time::Instant::now();
        }

        tracing::info!("context poller thread exited");
    });
}

/// Parse the /slots JSON response to extract context and speed information.
pub fn parse_slots_response(body: &str) -> Option<SlotInfo> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;

    // Helper to extract speed metrics from a slot (checks top-level, timings, metrics, and stats).
    let extract_speed = |slot: &serde_json::Value| -> (f64, f64) {
        let find_num = |key: &str| -> Option<f64> {
            slot.get(key)
                .or_else(|| slot.get("timings").and_then(|t| t.get(key)))
                .or_else(|| slot.get("metrics").and_then(|m| m.get(key)))
                .or_else(|| slot.get("stats").and_then(|s| s.get(key)))
                .and_then(|v| v.as_f64())
        };

        let predicted = find_num("predicted_per_second")
            .or_else(|| find_num("predicted_ms"))
            .or_else(|| find_num("tokens_per_second"))
            .unwrap_or(0.0);

        let prompt = find_num("prompt_per_second")
            .or_else(|| find_num("prompt_ms"))
            .unwrap_or(0.0);

        (predicted, prompt)
    };

    // Case A: Top-level Array.
    if let Some(arr) = json.as_array() {
        if let Some(first_slot) = arr.first() {
            let n_ctx = first_slot
                .get("n_ctx")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let mut tokens_used: u64 = 0;
            let mut prompt_tokens_acc: usize = 0;
            let mut decoded_tokens_acc: usize = 0;
            let (mut predicted_per_second, mut prompt_per_second) = (0.0, 0.0);
            let mut is_processing = false;

            for slot in arr {
                let prompt = slot
                    .get("n_prompt_tokens_processed")
                    .or_else(|| slot.get("n_prompt_tokens"))
                    .or_else(|| slot.get("prompt_tokens_total"))
                    .or_else(|| slot.get("n_past"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                let gen = slot
                    .get("next_token")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|tok| tok.get("n_decoded"))
                    .or_else(|| slot.get("generation_tokens_total"))
                    .or_else(|| slot.get("n_decoded"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                tokens_used += prompt + gen;
                prompt_tokens_acc += prompt as usize;
                decoded_tokens_acc += gen as usize;

                // Detect processing state directly from llama-server's slot status.
                if !is_processing {
                    if slot
                        .get("is_processing")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        is_processing = true;
                    } else if gen > 0 || prompt > 0 {
                        let (p, pr) = extract_speed(slot);
                        if p > 0.0 || pr > 0.0 {
                            is_processing = true;
                        }
                    }
                }

                // Accumulate speed metrics (take the first valid values).
                if predicted_per_second == 0.0 {
                    let (p, pr) = extract_speed(slot);
                    if p > 0.0 || pr > 0.0 {
                        predicted_per_second = p;
                        prompt_per_second = pr;
                    }
                }
            }

            if n_ctx > 0 {
                return Some(SlotInfo {
                    tokens_used: tokens_used as usize,
                    n_ctx,
                    predicted_per_second,
                    prompt_per_second,
                    prompt_tokens: prompt_tokens_acc,
                    decoded_tokens: decoded_tokens_acc,
                    is_processing,
                });
            }
        }
    }

    // Case B: Top-level Object.
    let n_ctx = json.get("n_ctx").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut tokens_used: u64 = 0;
    let mut prompt_tokens_acc: usize = 0;
    let mut decoded_tokens_acc: usize = 0;
    let (mut predicted_per_second, mut prompt_per_second) = (0.0, 0.0);
    let mut is_processing = false;

    if let Some(slots) = json.get("slots").and_then(|v| v.as_array()) {
        for slot in slots {
            let prompt = slot
                .get("n_prompt_tokens_processed")
                .or_else(|| slot.get("n_prompt_tokens"))
                .or_else(|| slot.get("prompt_tokens_total"))
                .or_else(|| slot.get("n_past"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let gen = slot
                .get("next_token")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|tok| tok.get("n_decoded"))
                .or_else(|| slot.get("generation_tokens_total"))
                .or_else(|| slot.get("n_decoded"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            tokens_used += prompt + gen;
            prompt_tokens_acc += prompt as usize;
            decoded_tokens_acc += gen as usize;

            // Detect processing state
            if !is_processing {
                if slot
                    .get("is_processing")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    is_processing = true;
                } else if gen > 0 || prompt > 0 {
                    let (p, pr) = extract_speed(slot);
                    if p > 0.0 || pr > 0.0 {
                        is_processing = true;
                    }
                }
            }

            // Accumulate speed metrics (take the first valid values).
            if predicted_per_second == 0.0 {
                let (p, pr) = extract_speed(slot);
                if p > 0.0 || pr > 0.0 {
                    predicted_per_second = p;
                    prompt_per_second = pr;
                }
            }
        }
    }

    if n_ctx > 0 {
        Some(SlotInfo {
            tokens_used: tokens_used as usize,
            n_ctx,
            predicted_per_second,
            prompt_per_second,
            prompt_tokens: prompt_tokens_acc,
            decoded_tokens: decoded_tokens_acc,
            is_processing,
        })
    } else {
        None
    }
}

/// Parse prompt_per_second, predicted_per_second, and cumulative total tokens from /metrics Prometheus text.
pub fn parse_metrics_response(body: &str) -> (f64, f64, Option<usize>) {
    let mut prompt_speed = 0.0;
    let mut gen_speed = 0.0;
    let mut prompt_tokens_total: Option<usize> = None;
    let mut gen_tokens_total: Option<usize> = None;
    for line in body.lines() {
        let trimmed = line.trim();
        let stripped = trimmed.strip_prefix("llamacpp:").unwrap_or(trimmed);
        if let Some(val_str) = stripped.strip_prefix("prompt_tokens_seconds ") {
            prompt_speed = val_str.trim().parse::<f64>().unwrap_or(0.0);
        } else if let Some(val_str) = stripped.strip_prefix("predicted_tokens_seconds ") {
            gen_speed = val_str.trim().parse::<f64>().unwrap_or(0.0);
        } else if let Some(val_str) = stripped.strip_prefix("prompt_tokens_total ") {
            prompt_tokens_total = val_str.trim().parse::<usize>().ok();
        } else if let Some(val_str) = stripped.strip_prefix("tokens_predicted_total ") {
            gen_tokens_total = val_str.trim().parse::<usize>().ok();
        } else if let Some(val_str) = stripped.strip_prefix("predicted_tokens_total ") {
            gen_tokens_total = val_str.trim().parse::<usize>().ok();
        }
    }
    let total_tokens = match (prompt_tokens_total, gen_tokens_total) {
        (Some(p), Some(g)) => Some(p + g),
        (Some(p), None) => Some(p),
        (None, Some(g)) => Some(g),
        (None, None) => None,
    };
    (prompt_speed, gen_speed, total_tokens)
}
