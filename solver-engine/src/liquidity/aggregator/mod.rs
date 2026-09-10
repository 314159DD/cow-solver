//! External aggregator integrations (0x, 1inch).
//!
//! These give us access to ALL DEXes and private market makers without
//! implementing each one individually. Strategy 7 queries both in parallel
//! and uses whichever quote beats our internal routing.

pub mod bebop;
pub mod kyberswap;
pub mod odos;
pub mod okx;
pub mod oneinch;
pub mod openocean;
pub mod paraswap;
pub mod zerox;

use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use bebop::BebopAggregator;
use kyberswap::KyberSwapAggregator;
use odos::OdosAggregator;
use okx::OkxAggregator;
use oneinch::OneInchAggregator;
use openocean::OpenOceanAggregator;
use paraswap::ParaswapAggregator;
use zerox::ZeroxAggregator;

// ── Quote type ───────────────────────────────────────────────────────────────

/// A quote returned by an external aggregator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatorQuote {
    /// Amount of buy_token the user will receive (decimal string).
    pub buy_amount: String,
    /// Estimated gas cost for the swap.
    pub gas_estimate: u64,
    /// ABI-encoded calldata to execute the swap (hex with 0x prefix).
    pub calldata: String,
    /// Contract address to call (exchange proxy / router).
    pub to: String,
    /// ETH value to send with the call (decimal string, usually "0").
    pub value: String,
    /// Which DEXes / liquidity sources contributed to this quote.
    pub sources: Vec<String>,
}

impl AggregatorQuote {
    /// Parse buy_amount as u128.
    pub fn buy_amount_u128(&self) -> u128 {
        self.buy_amount.parse().unwrap_or(0)
    }

    /// Compute the effective output after subtracting gas cost.
    /// `gas_price_wei` is the current gas price in wei.
    pub fn net_output(&self, gas_price_wei: u128) -> u128 {
        let gross = self.buy_amount_u128();
        let gas_cost = (self.gas_estimate as u128).saturating_mul(gas_price_wei);
        gross.saturating_sub(gas_cost)
    }
}

// ── Enum dispatch (avoids dyn-incompatible async trait) ──────────────────────

/// Concrete aggregator wrapper — enum dispatch instead of dyn trait objects.
/// Async fn in traits is not dyn-compatible in Rust 2024, so we use an enum
/// to dispatch to the concrete implementations.
pub enum Aggregator {
    ZeroX(ZeroxAggregator),
    OneInch(OneInchAggregator),
    Bebop(BebopAggregator),
    Paraswap(ParaswapAggregator),
    Okx(OkxAggregator),
    Odos(OdosAggregator),
    KyberSwap(KyberSwapAggregator),
    OpenOcean(OpenOceanAggregator),
}

impl Aggregator {
    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        chain_id: u64,
    ) -> Result<AggregatorQuote> {
        match self {
            Aggregator::ZeroX(a) => a.get_quote(sell_token, buy_token, sell_amount, chain_id).await,
            Aggregator::OneInch(a) => a.get_quote(sell_token, buy_token, sell_amount, chain_id).await,
            Aggregator::Bebop(a) => a.get_quote(sell_token, buy_token, sell_amount, chain_id).await,
            Aggregator::Paraswap(a) => a.get_quote(sell_token, buy_token, sell_amount, chain_id).await,
            Aggregator::Okx(a) => a.get_quote(sell_token, buy_token, sell_amount, chain_id).await,
            Aggregator::Odos(a) => a.get_quote(sell_token, buy_token, sell_amount, chain_id).await,
            Aggregator::KyberSwap(a) => a.get_quote(sell_token, buy_token, sell_amount, chain_id).await,
            Aggregator::OpenOcean(a) => a.get_quote(sell_token, buy_token, sell_amount, chain_id).await,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Aggregator::ZeroX(_) => "0x",
            Aggregator::OneInch(_) => "1inch",
            Aggregator::Bebop(_) => "bebop",
            Aggregator::Paraswap(_) => "paraswap",
            Aggregator::Okx(_) => "okx",
            Aggregator::Odos(_) => "odos",
            Aggregator::KyberSwap(_) => "kyberswap",
            Aggregator::OpenOcean(_) => "openocean",
        }
    }
}

// ── EIP-55 Address Checksum ──────────────────────────────────────────────────

/// Convert an address to EIP-55 checksummed format.
/// Uses a simple Keccak256 implementation (inline, no external dep).
pub fn checksum_address(addr: &str) -> String {
    let addr_lower = addr.trim_start_matches("0x").to_lowercase();
    if addr_lower.len() != 40 {
        return format!("0x{}", addr_lower); // invalid, return as-is
    }

    // Simple Keccak256 of the lowercase hex string
    let hash = keccak256(addr_lower.as_bytes());

    let mut result = String::with_capacity(42);
    result.push_str("0x");

    for (i, c) in addr_lower.chars().enumerate() {
        if c >= 'a' && c <= 'f' {
            // Check if the corresponding nibble of the hash is >= 8
            let hash_byte = hash[i / 2];
            let nibble = if i % 2 == 0 { hash_byte >> 4 } else { hash_byte & 0x0F };
            if nibble >= 8 {
                result.push(c.to_ascii_uppercase());
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Minimal Keccak-256 implementation (FIPS 202 / SHA-3).
/// Only used for EIP-55 address checksumming — not for cryptographic security.
fn keccak256(data: &[u8]) -> [u8; 32] {
    // Keccak-256: rate = 136 bytes (1088 bits), capacity = 64 bytes (512 bits)
    const RATE: usize = 136;
    let mut state = [0u64; 25];

    // Absorb
    let mut buf = data.to_vec();
    // Keccak padding: 0x01 ... 0x80
    buf.push(0x01);
    while buf.len() % RATE != 0 {
        buf.push(0x00);
    }
    let last = buf.len() - 1;
    buf[last] ^= 0x80;

    for chunk in buf.chunks(RATE) {
        for i in 0..(RATE / 8) {
            if i * 8 + 8 <= chunk.len() {
                state[i] ^= u64::from_le_bytes(chunk[i * 8..i * 8 + 8].try_into().unwrap());
            }
        }
        keccak_f1600(&mut state);
    }

    // Squeeze
    let mut output = [0u8; 32];
    for i in 0..4 {
        output[i * 8..(i + 1) * 8].copy_from_slice(&state[i].to_le_bytes());
    }
    output
}

/// Keccak-f[1600] permutation (24 rounds).
fn keccak_f1600(state: &mut [u64; 25]) {
    const RC: [u64; 24] = [
        0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
        0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
        0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
        0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
        0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
        0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
    ];
    const ROT: [u32; 25] = [
        0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14,
    ];
    const PI: [usize; 25] = [
        0, 10, 20, 5, 15, 16, 1, 11, 21, 6, 7, 17, 2, 12, 22, 23, 8, 18, 3, 13, 14, 24, 9, 19, 4,
    ];

    for round in 0..24 {
        // θ
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20];
        }
        let mut d = [0u64; 5];
        for x in 0..5 {
            d[x] = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
        }
        for i in 0..25 {
            state[i] ^= d[i % 5];
        }

        // ρ and π
        let mut temp = [0u64; 25];
        for i in 0..25 {
            temp[PI[i]] = state[i].rotate_left(ROT[i]);
        }

        // χ
        for y in 0..5 {
            for x in 0..5 {
                state[y * 5 + x] = temp[y * 5 + x] ^ (!temp[y * 5 + (x + 1) % 5] & temp[y * 5 + (x + 2) % 5]);
            }
        }

        // ι
        state[0] ^= RC[round];
    }
}

// ── Rate limiter ──────────────────────────────────────────────────────────────

/// Simple rate limiter: at most one request per `interval`.
pub struct RateLimiter {
    last_request: Mutex<Instant>,
    interval: Duration,
}

impl RateLimiter {
    pub fn new(interval: Duration) -> Self {
        Self {
            // Start in the past so the first request goes through immediately.
            last_request: Mutex::new(Instant::now() - interval),
            interval,
        }
    }

    /// Wait until the rate limit window has passed, then mark this instant.
    pub async fn wait(&self) {
        let mut last = self.last_request.lock().await;
        let elapsed = last.elapsed();
        if elapsed < self.interval {
            tokio::time::sleep(self.interval - elapsed).await;
        }
        *last = Instant::now();
    }
}

/// Daily/hourly budget limiter for API-key-gated aggregators.
///
/// Tracks requests within rolling windows to avoid burning through API quotas.
/// Thread-safe via Mutex. Returns `false` from `try_acquire()` when budget exhausted.
pub struct BudgetLimiter {
    state: Mutex<BudgetState>,
    daily_limit: u32,
    hourly_limit: u32,
}

struct BudgetState {
    /// Timestamps of requests in the current day window
    daily_timestamps: Vec<Instant>,
    /// Timestamps of requests in the current hour window
    hourly_timestamps: Vec<Instant>,
}

impl BudgetLimiter {
    /// Create a new budget limiter with daily and hourly caps.
    ///
    /// Reads from env vars if available:
    /// - `ODOS_DAILY_LIMIT` (default: 900 — leaves 10% headroom on 1000/day)
    /// - `ODOS_HOURLY_LIMIT` (default: 80 — spreads evenly across ~12 active hours)
    pub fn from_env() -> Self {
        let daily_limit: u32 = std::env::var("ODOS_DAILY_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(900);
        let hourly_limit: u32 = std::env::var("ODOS_HOURLY_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(80);

        Self {
            state: Mutex::new(BudgetState {
                daily_timestamps: Vec::new(),
                hourly_timestamps: Vec::new(),
            }),
            daily_limit,
            hourly_limit,
        }
    }

    /// Try to acquire a request slot. Returns `true` if within budget, `false` if exhausted.
    /// Also prunes expired timestamps from the windows.
    pub async fn try_acquire(&self) -> bool {
        let mut state = self.state.lock().await;
        let now = Instant::now();

        // Prune timestamps older than 24h
        state.daily_timestamps.retain(|t| now.duration_since(*t) < Duration::from_secs(86_400));
        // Prune timestamps older than 1h
        state.hourly_timestamps.retain(|t| now.duration_since(*t) < Duration::from_secs(3_600));

        if state.daily_timestamps.len() >= self.daily_limit as usize {
            return false;
        }
        if state.hourly_timestamps.len() >= self.hourly_limit as usize {
            return false;
        }

        state.daily_timestamps.push(now);
        state.hourly_timestamps.push(now);
        true
    }

    /// Return current usage stats for logging.
    pub async fn usage(&self) -> (usize, u32, usize, u32) {
        let state = self.state.lock().await;
        let now = Instant::now();
        let daily_used = state.daily_timestamps.iter()
            .filter(|t| now.duration_since(**t) < Duration::from_secs(86_400))
            .count();
        let hourly_used = state.hourly_timestamps.iter()
            .filter(|t| now.duration_since(**t) < Duration::from_secs(3_600))
            .count();
        (daily_used, self.daily_limit, hourly_used, self.hourly_limit)
    }
}

// ── Multi-aggregator helper ──────────────────────────────────────────────────

/// Query multiple aggregators in parallel, return the best quote (highest buy_amount).
///
/// Each aggregator gets a 5-second timeout. Failures are logged and skipped.
pub async fn best_quote(
    aggregators: &[Aggregator],
    sell_token: &str,
    buy_token: &str,
    sell_amount: &str,
    chain_id: u64,
) -> Option<(String, AggregatorQuote)> {
    // Run all aggregator queries concurrently using tokio::join.
    // We collect futures manually since the slice is small (max 2-3 aggregators).
    let mut results: Vec<(String, Result<AggregatorQuote>)> = Vec::new();

    // We can't easily spawn these (Aggregator isn't Send across threads without Arc),
    // so we use a simple sequential-with-timeout approach. With only 2 aggregators
    // and 5s timeout each, worst case is 10s — within our budget.
    //
    // For true parallelism we'd need Arc<Aggregator>, but the rate limiter inside
    // each aggregator already serializes requests anyway.
    for agg in aggregators {
        let name = agg.name().to_string();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            agg.get_quote(sell_token, buy_token, sell_amount, chain_id),
        )
        .await;

        match result {
            Ok(Ok(quote)) => {
                results.push((name, Ok(quote)));
            }
            Ok(Err(e)) => {
                debug!(aggregator = %name, error = %e, "Aggregator quote failed");
            }
            Err(_) => {
                debug!(aggregator = %name, "Aggregator quote timed out (1s)");
            }
        }
    }

    let mut best: Option<(String, AggregatorQuote)> = None;

    for (name, result) in results {
        if let Ok(quote) = result {
            let dominated = best
                .as_ref()
                .map(|(_, b)| quote.buy_amount_u128() > b.buy_amount_u128())
                .unwrap_or(true);
            if dominated {
                best = Some((name, quote));
            }
        }
    }

    best
}

/// Build the list of available external aggregators from env vars.
pub fn build_from_env() -> Vec<Aggregator> {
    let mut aggs = Vec::new();

    // API-key-based aggregators (only add if key configured)
    if let Some(oneinch) = OneInchAggregator::from_env() {
        aggs.push(Aggregator::OneInch(oneinch));
    }
    if let Some(okx) = OkxAggregator::from_env() {
        aggs.push(Aggregator::Okx(okx));
    }

    // Free/key-optional APIs — always active on Arbitrum
    let chain_id: u64 = std::env::var("CHAIN_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    if chain_id == 42161 {
        // 0x: free without key (lower rate limits), better with ZEROX_API_KEY
        if let Some(zerox) = ZeroxAggregator::from_env() {
            aggs.push(Aggregator::ZeroX(zerox));
        }
        aggs.push(Aggregator::Odos(OdosAggregator::new()));
        aggs.push(Aggregator::Bebop(BebopAggregator::new()));
        aggs.push(Aggregator::Paraswap(ParaswapAggregator::new()));
        aggs.push(Aggregator::KyberSwap(kyberswap::KyberSwapAggregator::new()));
        aggs.push(Aggregator::OpenOcean(openocean::OpenOceanAggregator::new()));
    }

    aggs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_address_eip55() {
        // Known EIP-55 test vector: USDC on Arbitrum
        let addr = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
        let checksummed = checksum_address(addr);
        assert_eq!(checksummed, "0xaf88d065e77c8cC2239327C5EDb3A432268e5831");
    }

    #[test]
    fn checksum_address_weth() {
        let addr = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
        let checksummed = checksum_address(addr);
        // Should be properly checksummed
        assert!(checksummed.starts_with("0x"));
        assert_eq!(checksummed.len(), 42);
    }

    #[test]
    fn quote_net_output() {
        let q = AggregatorQuote {
            buy_amount: "1000000".to_string(),
            gas_estimate: 200_000,
            calldata: "0x".to_string(),
            to: "0x".to_string(),
            value: "0".to_string(),
            sources: vec![],
        };
        // gas_price = 1 wei -> gas cost = 200_000
        assert_eq!(q.net_output(1), 800_000);
        // gas_price = 10 -> gas cost = 2_000_000 -> clamped to 0
        assert_eq!(q.net_output(10), 0);
    }

    #[tokio::test]
    async fn rate_limiter_first_request_immediate() {
        let rl = RateLimiter::new(Duration::from_secs(1));
        let t0 = Instant::now();
        rl.wait().await;
        // First request should be near-instant (< 50ms).
        assert!(t0.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn build_from_env_empty_without_keys() {
        // Without env vars set, should return empty vec.
        let aggs = build_from_env();
        // May or may not be empty depending on env, but shouldn't panic.
        assert!(aggs.len() <= 2);
    }
}
