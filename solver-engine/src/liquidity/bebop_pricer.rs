//! Bebop Price API — WebSocket streaming price levels from private market makers.
//!
//! Connects to wss://api.bebop.xyz/pmm/{network}/v3/pricing and receives
//! protobuf-encoded BebopPricingUpdate messages with bid/ask depth per pair.
//!
//! Use estimate_output() to check if Bebop can fill a trade before calling
//! the RFQ endpoint — saves rate limit quota and adds speed.
//!
//! Proto schema:
//!   message PriceUpdate { bytes base, bytes quote, uint64 last_update_ts, float[] bids, float[] asks }
//!   message BebopPricingUpdate { repeated PriceUpdate pairs }
//!   bids/asks are flat: [price1, size1, price2, size2, ...]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use std::sync::RwLock;
use prost::Message;
use tokio_tungstenite::tungstenite;
use tracing::{debug, info, warn};

// ── Protobuf types (hand-derived from bebop.proto) ──────────────────────────

#[derive(Clone, PartialEq, Message)]
pub struct PriceUpdate {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub base: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub quote: Option<Vec<u8>>,
    #[prost(uint64, optional, tag = "3")]
    pub last_update_ts: Option<u64>,
    #[prost(float, repeated, packed = "true", tag = "4")]
    pub bids: Vec<f32>,
    #[prost(float, repeated, packed = "true", tag = "5")]
    pub asks: Vec<f32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct BebopPricingUpdate {
    #[prost(message, repeated, tag = "1")]
    pub pairs: Vec<PriceUpdate>,
}

// ── Price level book ────────────────────────────────────────────────────────

/// A single price level: (price, size)
#[derive(Debug, Clone, Copy)]
pub struct Level {
    pub price: f64,
    pub size: f64,
}

/// Bid/ask depth for one token pair.
#[derive(Debug, Clone, Default)]
pub struct PairBook {
    pub bids: Vec<Level>, // best first (highest price)
    pub asks: Vec<Level>, // best first (lowest price)
    pub updated_at: u64,
}

/// Thread-safe price book for all pairs.
/// Key: (base_token_lowercase, quote_token_lowercase)
pub type PriceBook = Arc<RwLock<HashMap<(String, String), PairBook>>>;

pub fn new_price_book() -> PriceBook {
    Arc::new(RwLock::new(HashMap::new()))
}

// ── VWAP estimation ─────────────────────────────────────────────────────────

/// Estimate the output amount for a sell order using the ask side of the book.
/// Returns None if the pair isn't in the book or there's insufficient depth.
///
/// sell_token and buy_token are lowercase hex addresses.
/// sell_amount is in the sell token's base units (as f64).
pub fn estimate_output(
    book: &PriceBook,
    sell_token: &str,
    buy_token: &str,
    sell_amount: f64,
) -> Option<f64> {
    let books = book.read().unwrap();
    let pair = books.get(&(sell_token.to_lowercase(), buy_token.to_lowercase()))?;

    if pair.asks.is_empty() {
        return None;
    }

    // Walk the ask levels (lowest price first)
    let mut remaining = sell_amount;
    let mut total_output = 0.0;

    for level in &pair.asks {
        if remaining <= 0.0 {
            break;
        }
        let fill = remaining.min(level.size);
        total_output += fill * level.price as f64;
        remaining -= fill;
    }

    if remaining > 0.0 {
        None // insufficient depth
    } else {
        Some(total_output)
    }
}

// ── WebSocket streaming task ────────────────────────────────────────────────

/// Start the Bebop price streaming background task.
/// Connects to the WebSocket, decodes protobuf messages, updates the price book.
pub async fn start_price_stream(book: PriceBook, chain_id: u64) {
    let network = match chain_id {
        1 => "ethereum",
        10 => "optimism",
        56 => "bsc",
        137 => "polygon",
        8453 => "base",
        42161 => "arbitrum",
        _ => {
            info!(chain_id, "Bebop Price API: unsupported chain, skipping");
            return;
        }
    };

    let source = std::env::var("BEBOP_SOURCE").unwrap_or_default();
    let auth = std::env::var("BEBOP_SOURCE_AUTH").unwrap_or_default();

    if auth.is_empty() {
        info!("Bebop Price API: no BEBOP_SOURCE_AUTH set, skipping price stream");
        return;
    }

    let url = format!(
        "wss://api.bebop.xyz/pmm/{}/v3/pricing?format=protobuf&name={}&authorization={}&gasless=false&expiry_type=short",
        network, source, auth
    );

    info!(network, "Bebop Price API: starting stream");

    let mut backoff_secs = 1u64;

    loop {
        match connect_and_stream(&url, &book).await {
            Ok(()) => {
                info!("Bebop Price API: connection closed cleanly, reconnecting...");
                backoff_secs = 1;
            }
            Err(e) => {
                warn!(error = %e, backoff = backoff_secs, "Bebop Price API: connection error, retrying");
            }
        }

        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

async fn connect_and_stream(url: &str, book: &PriceBook) -> Result<(), String> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("WebSocket connect failed: {}", e))?;

    info!("Bebop Price API: connected");

    let (_, mut read) = ws_stream.split();
    let mut msg_count = 0u64;

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tungstenite::Message::Binary(data)) => {
                match BebopPricingUpdate::decode(data.as_ref()) {
                    Ok(update) => {
                        let mut books = book.write().unwrap();
                        for pair in &update.pairs {
                            let base = pair.base.as_ref()
                                .map(|b| format!("0x{}", hex::encode(b)))
                                .unwrap_or_default()
                                .to_lowercase();
                            let quote = pair.quote.as_ref()
                                .map(|b| format!("0x{}", hex::encode(b)))
                                .unwrap_or_default()
                                .to_lowercase();

                            if base.is_empty() || quote.is_empty() {
                                continue;
                            }

                            let bids = parse_levels(&pair.bids);
                            let asks = parse_levels(&pair.asks);

                            books.insert((base, quote), PairBook {
                                bids,
                                asks,
                                updated_at: pair.last_update_ts.unwrap_or(0),
                            });
                        }
                        msg_count += 1;
                        if msg_count % 100 == 0 {
                            debug!(pairs = books.len(), messages = msg_count, "Bebop price book updated");
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "Bebop: protobuf decode error");
                    }
                }
            }
            Ok(tungstenite::Message::Ping(data)) => {
                // Pong is handled automatically by tungstenite
                debug!("Bebop: ping received");
                let _ = data; // suppress unused warning
            }
            Ok(tungstenite::Message::Close(_)) => {
                info!("Bebop: server sent close");
                break;
            }
            Ok(_) => {} // text, pong, etc — ignore
            Err(e) => {
                return Err(format!("WebSocket read error: {}", e));
            }
        }
    }

    Ok(())
}

/// Parse flat float array [price1, size1, price2, size2, ...] into Vec<Level>.
fn parse_levels(flat: &[f32]) -> Vec<Level> {
    flat.chunks(2)
        .filter_map(|chunk| {
            if chunk.len() == 2 && chunk[0] > 0.0 && chunk[1] > 0.0 {
                Some(Level {
                    price: chunk[0] as f64,
                    size: chunk[1] as f64,
                })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_levels() {
        let flat = vec![1.5, 100.0, 1.6, 200.0, 1.7, 50.0];
        let levels = parse_levels(&flat);
        assert_eq!(levels.len(), 3);
        assert!((levels[0].price - 1.5).abs() < 0.001);
        assert!((levels[0].size - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_estimate_output() {
        let book = new_price_book();
        {
            let mut b = book.write().unwrap();
            b.insert(
                ("0xweth".to_string(), "0xusdc".to_string()),
                PairBook {
                    asks: vec![
                        Level { price: 3700.0, size: 0.5 },
                        Level { price: 3705.0, size: 1.0 },
                        Level { price: 3710.0, size: 2.0 },
                    ],
                    bids: vec![],
                    updated_at: 0,
                },
            );
        }

        // Sell 0.3 WETH — fills entirely at first level (3700)
        let output = estimate_output(&book, "0xweth", "0xusdc", 0.3);
        assert!(output.is_some());
        assert!((output.unwrap() - 1110.0).abs() < 0.1); // 0.3 * 3700

        // Sell 1.0 WETH — fills 0.5 at 3700, 0.5 at 3705
        let output = estimate_output(&book, "0xweth", "0xusdc", 1.0);
        assert!(output.is_some());
        let expected = 0.5 * 3700.0 + 0.5 * 3705.0;
        assert!((output.unwrap() - expected).abs() < 0.1);

        // Sell 5.0 WETH — insufficient depth (only 3.5 available)
        let output = estimate_output(&book, "0xweth", "0xusdc", 5.0);
        assert!(output.is_none());

        // Unknown pair
        let output = estimate_output(&book, "0xfoo", "0xbar", 1.0);
        assert!(output.is_none());
    }
}
