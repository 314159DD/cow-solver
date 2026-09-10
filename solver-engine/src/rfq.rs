//! RFQ Service (C.1)
//!
//! Fetches firm quotes from private market makers (Wintermute, Hashflow, Bebop)
//! when on-chain liquidity is thin. RFQ quotes are non-public and often better
//! than AMM prices for large orders.
//!
//! ## Kill Switch
//! Set `RFQ_ENABLED=false` to disable all RFQ calls (e.g. during outages).
//! Individual providers can be disabled with `RFQ_WINTERMUTE_ENABLED=false` etc.
//!
//! ## Architecture
//! ```text
//! solve handler ──▶ rfq::fetch_best_quote(token_in, token_out, amount_in)
//!                        │
//!              parallel fetch from all enabled providers
//!                        │
//!           ┌────────────┼────────────┐
//!      Wintermute    Hashflow       Bebop
//!           └────────────┼────────────┘
//!                        │
//!               best quote by output amount
//!                        │
//!              RfqQuote { amount_out, provider, quote_age_ms }
//! ```
//!
//! ## Latency Budget: 500ms max (parallel fetch, all providers in one shot)

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

// ── Types ─────────────────────────────────────────────────────────────────────

/// A firm quote from a private market maker.
#[derive(Debug, Clone)]
pub struct RfqQuote {
    /// Token being sold (input)
    pub token_in: String,
    /// Token being bought (output)
    pub token_out: String,
    /// Amount in (wei)
    pub amount_in: u128,
    /// Amount out offered by the market maker (wei)
    pub amount_out: u128,
    /// Which provider gave this quote
    pub provider: RfqProvider,
    /// How old this quote is in milliseconds
    pub quote_age_ms: u64,
    /// Optional expiry block
    pub valid_until_block: Option<u64>,
}

impl RfqQuote {
    /// Effective price: amount_out / amount_in (as f64 for comparison).
    pub fn effective_price(&self) -> f64 {
        if self.amount_in == 0 {
            0.0
        } else {
            self.amount_out as f64 / self.amount_in as f64
        }
    }
}

/// Which RFQ provider supplied this quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfqProvider {
    Wintermute,
    Hashflow,
    Bebop,
}

impl RfqProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            RfqProvider::Wintermute => "wintermute",
            RfqProvider::Hashflow => "hashflow",
            RfqProvider::Bebop => "bebop",
        }
    }
}

/// Input for an RFQ quote request.
#[derive(Debug, Clone)]
pub struct RfqRequest {
    pub token_in: String,
    pub token_out: String,
    /// Amount in (wei, decimal string)
    pub amount_in: u128,
    /// Chain ID
    pub chain_id: u64,
    /// Taker address (our settlement contract)
    pub taker: String,
}

// ── Kill switches ─────────────────────────────────────────────────────────────

pub fn is_rfq_enabled() -> bool {
    std::env::var("RFQ_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

fn is_provider_enabled(provider: &str) -> bool {
    let key = format!("RFQ_{}_ENABLED", provider.to_uppercase());
    std::env::var(&key)
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

// ── Timeout ───────────────────────────────────────────────────────────────────

const RFQ_TIMEOUT_MS: u64 = 500;

// ── Public API ────────────────────────────────────────────────────────────────

/// Fetch the best RFQ quote across all enabled providers.
///
/// Fires all provider requests in parallel. Returns the best quote by
/// `amount_out`, or `None` if no provider responded in time or all failed.
///
/// Must complete within 500ms — the caller should already have a fallback
/// AMM solution ready.
pub async fn fetch_best_quote(req: &RfqRequest) -> Option<RfqQuote> {
    if !is_rfq_enabled() {
        debug!("RFQ disabled via kill switch");
        return None;
    }

    let start = Instant::now();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(RFQ_TIMEOUT_MS))
        .build()
        .ok()?;

    // Launch all enabled providers in parallel
    let mut handles = Vec::new();

    if is_provider_enabled("wintermute") {
        let c = client.clone();
        let r = req.clone();
        handles.push(tokio::spawn(async move { fetch_wintermute(&c, &r).await }));
    }
    if is_provider_enabled("hashflow") {
        let c = client.clone();
        let r = req.clone();
        handles.push(tokio::spawn(async move { fetch_hashflow(&c, &r).await }));
    }
    if is_provider_enabled("bebop") {
        let c = client.clone();
        let r = req.clone();
        handles.push(tokio::spawn(async move { fetch_bebop(&c, &r).await }));
    }

    // Collect results
    let mut quotes: Vec<RfqQuote> = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Some(q)) => quotes.push(q),
            Ok(None) => {}
            Err(e) => warn!(error = %e, "RFQ provider task panicked"),
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;
    record_request(quotes.len());

    debug!(
        providers_responded = quotes.len(),
        elapsed_ms,
        token_in = %req.token_in,
        token_out = %req.token_out,
        "RFQ fetch complete"
    );

    // Return best quote by amount_out
    quotes.into_iter().max_by(|a, b| {
        a.effective_price()
            .partial_cmp(&b.effective_price())
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

// ── Provider implementations ─────────────────────────────────────────────────

/// Wintermute RFQ — uses their internal quoting API.
/// Endpoint and auth token must be configured via env vars.
async fn fetch_wintermute(client: &reqwest::Client, req: &RfqRequest) -> Option<RfqQuote> {
    let endpoint = std::env::var("WINTERMUTE_RFQ_URL").ok()?;
    let api_key = std::env::var("WINTERMUTE_API_KEY").unwrap_or_default();

    let start = Instant::now();

    let resp = client
        .post(&endpoint)
        .header("X-Api-Key", &api_key)
        .json(&serde_json::json!({
            "tokenIn": req.token_in,
            "tokenOut": req.token_out,
            "amountIn": req.amount_in.to_string(),
            "chainId": req.chain_id,
            "taker": req.taker,
        }))
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;

    let amount_out: u128 = resp["amountOut"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| resp["amountOut"].as_u64().map(|v| v as u128))?;

    let quote_age_ms = start.elapsed().as_millis() as u64;

    Some(RfqQuote {
        token_in: req.token_in.clone(),
        token_out: req.token_out.clone(),
        amount_in: req.amount_in,
        amount_out,
        provider: RfqProvider::Wintermute,
        quote_age_ms,
        valid_until_block: resp["validUntilBlock"].as_u64(),
    })
}

/// Hashflow RFQ — open API, no auth required.
async fn fetch_hashflow(client: &reqwest::Client, req: &RfqRequest) -> Option<RfqQuote> {
    let endpoint = std::env::var("HASHFLOW_RFQ_URL")
        .unwrap_or_else(|_| "https://api.hashflow.com/taker/v3/rfq".to_string());

    let start = Instant::now();

    let resp = client
        .post(&endpoint)
        .json(&serde_json::json!({
            "baseToken": req.token_in,
            "quoteToken": req.token_out,
            "baseTokenAmount": req.amount_in.to_string(),
            "networkId": req.chain_id,
            "effectiveTrader": req.taker,
        }))
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;

    let quote = &resp["quotes"]
        .as_array()?
        .first()?;

    let amount_out: u128 = quote["quoteTokenAmount"]
        .as_str()
        .and_then(|s| s.parse().ok())?;

    let quote_age_ms = start.elapsed().as_millis() as u64;

    Some(RfqQuote {
        token_in: req.token_in.clone(),
        token_out: req.token_out.clone(),
        amount_in: req.amount_in,
        amount_out,
        provider: RfqProvider::Hashflow,
        quote_age_ms,
        valid_until_block: None,
    })
}

/// Bebop RFQ — aggregates across multiple MMs.
async fn fetch_bebop(client: &reqwest::Client, req: &RfqRequest) -> Option<RfqQuote> {
    let endpoint = std::env::var("BEBOP_RFQ_URL")
        .unwrap_or_else(|_| "https://api.bebop.xyz/pmm/arbitrum/v3/quote".to_string());

    let start = Instant::now();

    let resp = client
        .get(&endpoint)
        .query(&[
            ("sell_tokens", req.token_in.as_str()),
            ("buy_tokens", req.token_out.as_str()),
            ("sell_amounts", &req.amount_in.to_string()),
            ("taker_address", req.taker.as_str()),
        ])
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;

    let amount_out: u128 = resp["routes"]
        .as_array()?
        .first()?["quote"]["buyAmount"]
        .as_str()
        .and_then(|s| s.parse().ok())?;

    let quote_age_ms = start.elapsed().as_millis() as u64;

    Some(RfqQuote {
        token_in: req.token_in.clone(),
        token_out: req.token_out.clone(),
        amount_in: req.amount_in,
        amount_out,
        provider: RfqProvider::Bebop,
        quote_age_ms,
        valid_until_block: None,
    })
}

// ── Metrics ───────────────────────────────────────────────────────────────────

pub struct RfqMetrics {
    /// Total RFQ requests made
    pub total_requests: AtomicU64,
    /// Requests where at least one provider responded
    pub successful_requests: AtomicU64,
    /// Requests where no provider responded (all timed out or failed)
    pub failed_requests: AtomicU64,
    /// Times RFQ quote beat AMM price and was used
    pub rfq_won: AtomicU64,
}

impl RfqMetrics {
    fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            rfq_won: AtomicU64::new(0),
        }
    }
}

static RFQ_METRICS: OnceLock<RfqMetrics> = OnceLock::new();

pub fn rfq_metrics() -> &'static RfqMetrics {
    RFQ_METRICS.get_or_init(RfqMetrics::new)
}

fn record_request(providers_responded: usize) {
    let m = rfq_metrics();
    m.total_requests.fetch_add(1, Ordering::Relaxed);
    if providers_responded > 0 {
        m.successful_requests.fetch_add(1, Ordering::Relaxed);
    } else {
        m.failed_requests.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_rfq_won() {
    rfq_metrics().rfq_won.fetch_add(1, Ordering::Relaxed);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_req() -> RfqRequest {
        RfqRequest {
            token_in: "0xWETH".to_string(),
            token_out: "0xUSDC".to_string(),
            amount_in: 1_000_000_000_000_000_000, // 1 ETH
            chain_id: 42161,
            taker: "0x9008D19f58AAbD9eD0D60971565AA8510560ab41".to_string(),
        }
    }

    #[test]
    fn rfq_quote_effective_price() {
        let q = RfqQuote {
            token_in: "0xWETH".to_string(),
            token_out: "0xUSDC".to_string(),
            amount_in: 1_000_000_000_000_000_000,   // 1 WETH (18 dec)
            amount_out: 2_700_000_000,               // 2700 USDC (6 dec)
            provider: RfqProvider::Wintermute,
            quote_age_ms: 50,
            valid_until_block: None,
        };
        assert!(q.effective_price() > 0.0);
    }

    #[test]
    fn rfq_provider_as_str() {
        assert_eq!(RfqProvider::Wintermute.as_str(), "wintermute");
        assert_eq!(RfqProvider::Hashflow.as_str(), "hashflow");
        assert_eq!(RfqProvider::Bebop.as_str(), "bebop");
    }

    #[test]
    fn rfq_disabled_kill_switch() {
        // Verify kill switch parsing doesn't panic
        let _ = is_rfq_enabled();
        let _ = is_provider_enabled("wintermute");
    }

    #[tokio::test]
    async fn fetch_best_quote_returns_none_when_no_endpoints() {
        // With no env vars set for provider URLs, Wintermute returns None (missing URL),
        // Hashflow and Bebop try real endpoints and will fail/timeout in test env.
        // With RFQ_ENABLED=false the whole thing short-circuits.
        unsafe { std::env::set_var("RFQ_ENABLED", "false") };
        let req = make_req();
        let result = fetch_best_quote(&req).await;
        assert!(result.is_none());
        unsafe { std::env::remove_var("RFQ_ENABLED") };
    }

    #[test]
    fn metrics_record_request() {
        let m = rfq_metrics();
        let before = m.total_requests.load(Ordering::Relaxed);
        record_request(1);
        assert_eq!(m.total_requests.load(Ordering::Relaxed), before + 1);
        assert!(m.successful_requests.load(Ordering::Relaxed) >= 1);
    }

    #[test]
    fn best_quote_selected_by_effective_price() {
        // Simulate two quotes — higher effective price should win
        let q1 = RfqQuote {
            token_in: "0xA".to_string(),
            token_out: "0xB".to_string(),
            amount_in: 1_000,
            amount_out: 2_000, // price = 2.0
            provider: RfqProvider::Hashflow,
            quote_age_ms: 100,
            valid_until_block: None,
        };
        let q2 = RfqQuote {
            token_in: "0xA".to_string(),
            token_out: "0xB".to_string(),
            amount_in: 1_000,
            amount_out: 2_500, // price = 2.5 — better
            provider: RfqProvider::Bebop,
            quote_age_ms: 80,
            valid_until_block: None,
        };
        let best = [q1, q2]
            .into_iter()
            .max_by(|a, b| a.effective_price().partial_cmp(&b.effective_price()).unwrap());
        assert_eq!(best.unwrap().provider, RfqProvider::Bebop);
    }
}
