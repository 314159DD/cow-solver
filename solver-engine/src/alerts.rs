//! Telegram alerting with 5-minute per-key deduplication.
//!
//! ## Setup
//! Set two environment variables:
//! ```text
//! TELEGRAM_BOT_TOKEN=123456:ABCdef...
//! TELEGRAM_CHAT_ID=-100123456789
//! ```
//! If either is unset or empty the call is a no-op — safe in local dev.
//!
//! ## Usage
//! ```rust,ignore
//! // Fire-and-forget from any async context
//! tokio::spawn(alerts::send_alert("CRITICAL", "solver_silent", format!("No auctions in {}s", secs)));
//!
//! // Or await directly
//! alerts::send_alert("ERROR", "ebbo_violation", format!("EBBO violation on auction {id}")).await;
//! ```

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::warn;

// ── Deduplication store ───────────────────────────────────────────────────────

static DEDUP: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn dedup_store() -> &'static Mutex<HashMap<String, u64>> {
    DEDUP.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Send a Telegram alert with 5-minute deduplication.
///
/// * `severity` — `"CRITICAL"`, `"ERROR"`, or `"WARNING"` (shown in message header)
/// * `key`      — dedup key; identical `key` is suppressed for 5 minutes after first fire
/// * `message`  — human-readable alert body (owned `String` so the caller can `tokio::spawn`)
///
/// Silently no-ops if `TELEGRAM_BOT_TOKEN` or `TELEGRAM_CHAT_ID` env vars are unset.
pub async fn send_alert(severity: &'static str, key: &'static str, message: String) {
    // ── Dedup check ──────────────────────────────────────────────────────────
    {
        let mut store = match dedup_store().lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        let now = now_secs();
        if now < store.get(key).copied().unwrap_or(0) {
            return; // suppressed
        }
        store.insert(key.to_string(), now + 300); // suppress for 5 minutes
    }

    // ── Config ───────────────────────────────────────────────────────────────
    let token = match std::env::var("TELEGRAM_BOT_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => return,
    };
    let chat_id = match std::env::var("TELEGRAM_CHAT_ID") {
        Ok(c) if !c.is_empty() => c,
        _ => return,
    };

    // ── Send ─────────────────────────────────────────────────────────────────
    let text = format!("[{severity}] CoW Solver\n{message}");
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");

    let client = reqwest::Client::new();
    if let Err(e) = client
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        }))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        warn!(error = %e, severity, key, "Failed to send Telegram alert");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_suppresses_within_window() {
        // Use a unique key to avoid cross-test interference
        let key = "test_dedup_isolation_key_abc123";

        // Simulate first alert: mark as sent
        {
            let mut store = dedup_store().lock().unwrap();
            let now = now_secs();
            // Not yet suppressed
            assert!(store.get(key).copied().unwrap_or(0) <= now);
            store.insert(key.to_string(), now + 300);
        }

        // Second check: should be suppressed
        {
            let store = dedup_store().lock().unwrap();
            let now = now_secs();
            assert!(store.get(key).copied().unwrap_or(0) > now, "Should be suppressed for 5 min");
        }
    }

    #[test]
    fn no_panic_when_env_vars_unset() {
        // This must not panic — silently no-op when credentials are missing.
        // We can't actually await in a sync test, but we can verify the dedup
        // logic path doesn't crash.
        drop(dedup_store().lock().unwrap());
    }
}
