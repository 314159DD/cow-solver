//! WebSocket pool monitor for real-time V2 Sync + V3 Swap event subscriptions.
//!
//! Connects to Alchemy's WebSocket endpoint and subscribes to pool events.
//! Events are received in real-time (~250ms on Arbitrum) and update the pool
//! cache immediately. This replaces the 10-second eth_getLogs polling loop
//! for much fresher pool data.
//!
//! Cost: $0 — WebSocket subscriptions don't consume Alchemy compute units.
//! Only event delivery counts, and that's minimal for our pool set.
//!
//! Research finding #2: "Sub-second pool updates" — this is the implementation.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tracing::{debug, info, warn};

/// Run the WebSocket pool event monitor.
///
/// Connects to Alchemy WSS endpoint, subscribes to Sync + Swap events
/// for all cached pools, and updates the pool cache in real-time.
pub async fn run_ws_monitor() {
    let rpc_url = match std::env::var("RPC_URL") {
        Ok(url) => url,
        Err(_) => {
            info!("WebSocket monitor disabled — no RPC_URL configured");
            return;
        }
    };

    // Convert HTTP URL to WebSocket URL
    let ws_url = rpc_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");

    info!(ws_url = %ws_url.chars().take(50).collect::<String>(), "WebSocket monitor starting");

    // Retry loop — reconnect on disconnection
    loop {
        match run_ws_connection(&ws_url).await {
            Ok(()) => {
                info!("WebSocket connection closed cleanly, reconnecting in 5s");
            }
            Err(e) => {
                warn!(error = %e, "WebSocket connection failed, retrying in 5s");
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run_ws_connection(ws_url: &str) -> Result<(), String> {
    let (ws_stream, _) = connect_async(ws_url)
        .await
        .map_err(|e| format!("WebSocket connect failed: {}", e))?;

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to V2 Sync + V3 Swap events for all cached pools
    let sync_topic = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";
    let swap_v3_topic = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";
    let mint_topic = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde";
    let burn_topic = "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c";

    let subscribe_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["logs", {
            "topics": [[sync_topic, swap_v3_topic, mint_topic, burn_topic]]
        }]
    });

    write.send(tokio_tungstenite::tungstenite::Message::Text(
        subscribe_msg.to_string().into()
    ))
    .await
    .map_err(|e| format!("WebSocket send failed: {}", e))?;

    info!("WebSocket: subscribed to V2 Sync + V3 Swap/Mint/Burn events");

    let mut events_processed = 0u64;
    let mut pools_updated = 0u64;

    // Process incoming events
    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "WebSocket read error");
                break;
            }
        };

        let text = match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => t,
            tokio_tungstenite::tungstenite::Message::Ping(_) => continue,
            tokio_tungstenite::tungstenite::Message::Close(_) => {
                info!("WebSocket: server closed connection");
                break;
            }
            _ => continue,
        };

        // Parse the event notification
        let value: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // eth_subscribe response (subscription ID) — skip
        if value.get("result").is_some() && value.get("params").is_none() {
            debug!("WebSocket: subscription confirmed");
            continue;
        }

        // Event notification: {"params": {"result": {log_object}}}
        let log = match value.pointer("/params/result") {
            Some(l) => l,
            None => continue,
        };

        events_processed += 1;
        if process_log_event(log) {
            pools_updated += 1;
        }

        // Periodic log
        if events_processed % 100 == 0 {
            debug!(events = events_processed, updates = pools_updated, "WebSocket: event stats");
        }
    }

    Ok(())
}

/// Process a single log event and update the pool cache.
/// Returns true if a pool was updated.
fn process_log_event(log: &serde_json::Value) -> bool {
    use crate::pool_indexer::{self, PoolType};

    let addr = match log["address"].as_str() {
        Some(a) => a.to_lowercase(),
        None => return false,
    };

    let topic0 = log["topics"].as_array()
        .and_then(|t| t.first())
        .and_then(|t| t.as_str())
        .unwrap_or("");

    let data = match log["data"].as_str() {
        Some(d) => d.trim_start_matches("0x"),
        None => return false,
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let block = pool_indexer::current_block();

    const SYNC_TOPIC: &str = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";
    const SWAP_V3_TOPIC: &str = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";

    if topic0 == SYNC_TOPIC {
        // V2 Sync: data = (reserve0, reserve1)
        if data.len() < 128 { return false; }
        let r0 = u128::from_str_radix(&data[0..64], 16).unwrap_or(0);
        let r1 = u128::from_str_radix(&data[64..128], 16).unwrap_or(0);
        if let Some(mut snap) = pool_indexer::pool_cache().get_mut(&addr) {
            if snap.pool_type == PoolType::V2 {
                snap.reserve0 = r0;
                snap.reserve1 = r1;
                snap.fetched_at = now;
                snap.block = block;
                return true;
            }
        }
    } else if topic0 == SWAP_V3_TOPIC {
        // V3 Swap: data = (amount0, amount1, sqrtPriceX96, liquidity, tick)
        if data.len() < 320 { return false; }
        let sqrt_price = u128::from_str_radix(&data[128..192], 16).unwrap_or(0);
        let liquidity = u128::from_str_radix(&data[192..256], 16).unwrap_or(0);
        let tick_raw = i64::from_str_radix(&data[256..320], 16).unwrap_or(0);
        let tick = if tick_raw > 0x7FFFFF { tick_raw as i32 - 0x1000000 } else { tick_raw as i32 };

        if sqrt_price > 0 {
            if let Some(mut snap) = pool_indexer::pool_cache().get_mut(&addr) {
                if snap.pool_type == PoolType::V3 {
                    snap.sqrt_price_x96 = Some(sqrt_price);
                    snap.tick = Some(tick);
                    snap.v3_liquidity = Some(liquidity);
                    snap.fetched_at = now;
                    snap.block = block;
                    return true;
                }
            }
        }
    }

    false
}
