//! Opportunity Filter / Auction Triage (B.5)
//!
//! Fast classifier that bins incoming auctions before committing full solver
//! resources. Must complete within 30ms. Decides pipeline depth per auction.
//!
//! ## Triage Classes
//!
//! | Class | Action |
//! |-------|--------|
//! | `profitable` | Full pipeline + RFQ + simulation |
//! | `marginal` | Internal strategies only, skip RFQ |
//! | `unwinnable` | Fast direct-only, minimal compute |
//! | `skip` | Return empty immediately |

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use tracing::debug;

use crate::models::auction::AuctionInstance;

// ── Triage Result ───────────────────────────────────────────────────────────

/// The triage classification for an auction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageClass {
    /// Full pipeline: all strategies + RFQ + simulation
    Profitable,
    /// Internal strategies only, skip RFQ
    Marginal,
    /// Fast direct-only routing, minimal compute
    Unwinnable,
    /// Return empty immediately — not worth any compute
    Skip,
}

impl TriageClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            TriageClass::Profitable => "profitable",
            TriageClass::Marginal => "marginal",
            TriageClass::Unwinnable => "unwinnable",
            TriageClass::Skip => "skip",
        }
    }
}

impl std::fmt::Display for TriageClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Triage Metrics ──────────────────────────────────────────────────────────

pub struct TriageMetrics {
    pub profitable: AtomicU64,
    pub marginal: AtomicU64,
    pub unwinnable: AtomicU64,
    pub skip: AtomicU64,
}

impl TriageMetrics {
    fn new() -> Self {
        Self {
            profitable: AtomicU64::new(0),
            marginal: AtomicU64::new(0),
            unwinnable: AtomicU64::new(0),
            skip: AtomicU64::new(0),
        }
    }
}

static TRIAGE_METRICS: OnceLock<TriageMetrics> = OnceLock::new();

pub fn triage_metrics() -> &'static TriageMetrics {
    TRIAGE_METRICS.get_or_init(TriageMetrics::new)
}

fn record_triage(class: TriageClass) {
    let m = triage_metrics();
    match class {
        TriageClass::Profitable => m.profitable.fetch_add(1, Ordering::Relaxed),
        TriageClass::Marginal => m.marginal.fetch_add(1, Ordering::Relaxed),
        TriageClass::Unwinnable => m.unwinnable.fetch_add(1, Ordering::Relaxed),
        TriageClass::Skip => m.skip.fetch_add(1, Ordering::Relaxed),
    };
}

// ── Configuration ───────────────────────────────────────────────────────────

/// Minimum sell amount in wei to avoid dust orders (roughly $0.10 worth of ETH).
const DUST_THRESHOLD_WEI: u128 = 50_000_000_000_000; // 0.00005 ETH

/// Minimum number of liquidity sources to consider an order worth routing.
const MIN_LIQUIDITY_SOURCES: usize = 1;

/// Orders above this sell amount (in wei) are always considered profitable.
/// ~0.1 ETH worth — large enough to generate meaningful surplus.
const LARGE_ORDER_THRESHOLD_WEI: u128 = 100_000_000_000_000_000;

// ── Well-known tokens (Arbitrum One) ────────────────────────────────────────

/// Tokens we know have deep liquidity on Arbitrum.
const WELL_KNOWN_TOKENS: &[&str] = &[
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1", // WETH
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831", // USDC (native)
    "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8", // USDC.e (bridged)
    "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9", // USDT
    "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1", // DAI
    "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f", // WBTC
    "0x912ce59144191c1204e64559fe8253a0e49e6548", // ARB
    "0xfc5a1a6eb076a2c7ad06ed22c90d7e710e35ad0a", // GMX
];

// Tokens where aggregator-backed solvers (OKX, 1inch) have the strongest edge.
// Pairs between these 3 tokens have maximum competition — thin margins.
const HIGH_COMPETITION_TOKENS: &[&str] = &[
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1", // WETH
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831", // USDC (native)
    "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8", // USDC.e (bridged)
    "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9", // USDT
];

/// Check if a token pair has maximum competition from aggregator-backed solvers.
///
/// Returns true for WETH↔USDC, WETH↔USDT, USDC↔USDT (both directions).
/// These pairs are dominated by solvers with OKX/1inch APIs — we should
/// deprioritize them and focus on exotic pairs where we have an edge.
pub fn is_high_competition_pair(sell_token: &str, buy_token: &str) -> bool {
    let sell_lc = sell_token.to_lowercase();
    let buy_lc = buy_token.to_lowercase();
    let sell_hc = HIGH_COMPETITION_TOKENS.iter().any(|t| *t == sell_lc);
    let buy_hc = HIGH_COMPETITION_TOKENS.iter().any(|t| *t == buy_lc);
    sell_hc && buy_hc
}

// ── Classifier ──────────────────────────────────────────────────────────────

/// Classify an auction for pipeline depth selection.
///
/// Returns the triage class and a brief reason string for logging.
/// This function MUST complete within 30ms — it does zero I/O.
pub fn classify(auction: &AuctionInstance) -> (TriageClass, &'static str) {
    // Rule 1: No orders → skip
    if auction.orders.is_empty() {
        let class = TriageClass::Skip;
        record_triage(class);
        debug!(class = %class, reason = "no_orders", "Auction triaged");
        return (class, "no_orders");
    }

    // Rule 2: No liquidity at all → skip (but check our own pool indexer too)
    if auction.liquidity.is_empty() && crate::pool_indexer::pool_count() == 0 {
        let class = TriageClass::Skip;
        record_triage(class);
        debug!(class = %class, reason = "no_liquidity", "Auction triaged");
        return (class, "no_liquidity");
    }

    let order_count = auction.orders.len();
    let liquidity_count = auction.liquidity.len() + crate::pool_indexer::pool_count();

    // Rule 3: Check if any order is large enough to be worth full compute
    let has_large_order = auction.orders.iter().any(|o| {
        parse_amount(&o.sell_amount) >= LARGE_ORDER_THRESHOLD_WEI
    });

    // Rule 4: Check if all orders are dust
    let all_dust = auction.orders.iter().all(|o| {
        parse_amount(&o.sell_amount) < DUST_THRESHOLD_WEI
    });

    if all_dust {
        let class = TriageClass::Skip;
        record_triage(class);
        debug!(
            class = %class,
            reason = "all_dust",
            order_count,
            "Auction triaged"
        );
        return (class, "all_dust");
    }

    // Rule 5: Check token familiarity
    let known_token_orders = auction.orders.iter().filter(|o| {
        let sell_lower = o.sell_token.to_lowercase();
        let buy_lower = o.buy_token.to_lowercase();
        WELL_KNOWN_TOKENS.iter().any(|t| *t == sell_lower)
            && WELL_KNOWN_TOKENS.iter().any(|t| *t == buy_lower)
    }).count();

    let unknown_token_ratio = if order_count > 0 {
        1.0 - (known_token_orders as f64 / order_count as f64)
    } else {
        1.0
    };

    // Rule 6: Classify
    if has_large_order && known_token_orders > 0 && liquidity_count >= MIN_LIQUIDITY_SOURCES {
        let class = TriageClass::Profitable;
        record_triage(class);
        debug!(
            class = %class,
            reason = "large_order_with_liquidity",
            order_count,
            liquidity_count,
            "Auction triaged"
        );
        return (class, "large_order_with_liquidity");
    }

    if known_token_orders > 0 && liquidity_count >= MIN_LIQUIDITY_SOURCES {
        let class = TriageClass::Profitable;
        record_triage(class);
        debug!(
            class = %class,
            reason = "known_tokens_with_liquidity",
            order_count,
            known_token_orders,
            liquidity_count,
            "Auction triaged"
        );
        return (class, "known_tokens_with_liquidity");
    }

    // Unknown tokens but we have some liquidity → marginal
    if liquidity_count >= MIN_LIQUIDITY_SOURCES && unknown_token_ratio < 1.0 {
        let class = TriageClass::Marginal;
        record_triage(class);
        debug!(
            class = %class,
            reason = "mixed_tokens",
            unknown_ratio = unknown_token_ratio,
            "Auction triaged"
        );
        return (class, "mixed_tokens");
    }

    // Very little liquidity or all unknown tokens → unwinnable
    if unknown_token_ratio >= 1.0 || liquidity_count < MIN_LIQUIDITY_SOURCES {
        let class = TriageClass::Unwinnable;
        record_triage(class);
        debug!(
            class = %class,
            reason = "unknown_tokens_or_no_liquidity",
            "Auction triaged"
        );
        return (class, "unknown_tokens_or_no_liquidity");
    }

    // Default: marginal (conservative — run internal strategies)
    let class = TriageClass::Marginal;
    record_triage(class);
    debug!(class = %class, reason = "default", "Auction triaged");
    (class, "default")
}

/// Parse a string amount to u128, returning 0 on failure.
fn parse_amount(s: &str) -> u128 {
    s.parse::<u128>().unwrap_or(0)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::auction::AuctionInstance;
    use crate::models::order::{Order, OrderKind, OrderClass};
    use std::collections::HashMap;

    fn make_order(sell_token: &str, buy_token: &str, sell_amount: &str) -> Order {
        Order {
            uid: "test-uid".to_string(),
            sell_token: sell_token.to_string(),
            buy_token: buy_token.to_string(),
            sell_amount: sell_amount.to_string(),
            buy_amount: "1".to_string(),
            fee_amount: "0".to_string(),
            kind: OrderKind::Sell,
            partially_fillable: false,
            class: OrderClass::Market,
            sell_token_balance: None,
            buy_token_balance: None,
            signing_scheme: None,
            signature: None,
            receiver: None,
            app_data: None,
            valid_to: None,
        }
    }

    fn make_auction(orders: Vec<Order>, liquidity_count: usize) -> AuctionInstance {
        use crate::models::liquidity::{Liquidity, ConstantProductPool, LiquidityTokenBalance, LiquidityTokenMap};

        let liquidity: Vec<Liquidity> = (0..liquidity_count).map(|i| {
            let mut tokens: LiquidityTokenMap = HashMap::new();
            tokens.insert(
                "0x82af49447d8a07e3bd95bd0d56f35241523fbab1".to_string(),
                LiquidityTokenBalance { balance: "1000000000000000000".to_string() },
            );
            tokens.insert(
                "0xaf88d065e77c8cc2239327c5edb3a432268e5831".to_string(),
                LiquidityTokenBalance { balance: "2700000000".to_string() },
            );
            Liquidity::ConstantProduct(ConstantProductPool {
                tokens,
                fee: "0.003".to_string(),
                id: format!("pool_{i}"),
                address: format!("0x{i:040x}"),
                router: None,
                gas_estimate: String::new(),
            })
        }).collect();

        AuctionInstance {
            id: 1,
            tokens: HashMap::new(),
            orders,
            liquidity,
            effective_gas_price: "100000000".to_string(),
            deadline: None,
            chain_id: Some(42161),
            block: None,
        }
    }

    #[test]
    fn empty_orders_is_skip() {
        let auction = make_auction(vec![], 5);
        let (class, _) = classify(&auction);
        assert_eq!(class, TriageClass::Skip);
    }

    #[test]
    fn no_liquidity_is_skip() {
        let order = make_order(
            "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
            "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
            "1000000000000000000",
        );
        let auction = make_auction(vec![order], 0);
        // The pool indexer cache is process-global, so a sibling test that
        // registered pools would make "no liquidity" untrue for this auction.
        if crate::pool_indexer::pool_count() > 0 {
            eprintln!("pool indexer cache is populated by another test; skipping no_liquidity_is_skip");
            return;
        }
        let (class, _) = classify(&auction);
        assert_eq!(class, TriageClass::Skip);
    }

    #[test]
    fn dust_orders_are_skipped() {
        let order = make_order(
            "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
            "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
            "100", // way below dust threshold
        );
        let auction = make_auction(vec![order], 3);
        let (class, _) = classify(&auction);
        assert_eq!(class, TriageClass::Skip);
    }

    #[test]
    fn large_known_order_is_profitable() {
        let order = make_order(
            "0x82af49447d8a07e3bd95bd0d56f35241523fbab1", // WETH
            "0xaf88d065e77c8cc2239327c5edb3a432268e5831", // USDC
            "500000000000000000", // 0.5 ETH
        );
        let auction = make_auction(vec![order], 3);
        let (class, _) = classify(&auction);
        assert_eq!(class, TriageClass::Profitable);
    }

    #[test]
    fn small_known_order_is_profitable() {
        let order = make_order(
            "0x82af49447d8a07e3bd95bd0d56f35241523fbab1", // WETH
            "0xaf88d065e77c8cc2239327c5edb3a432268e5831", // USDC
            "1000000000000000", // 0.001 ETH — above dust, below large
        );
        let auction = make_auction(vec![order], 3);
        let (class, _) = classify(&auction);
        assert_eq!(class, TriageClass::Profitable);
    }

    #[test]
    fn unknown_tokens_with_liquidity_is_unwinnable() {
        let order = make_order(
            "0x1111111111111111111111111111111111111111", // unknown
            "0x2222222222222222222222222222222222222222", // unknown
            "1000000000000000000", // 1 ETH — large but unknown tokens
        );
        let auction = make_auction(vec![order], 3);
        let (class, _) = classify(&auction);
        assert_eq!(class, TriageClass::Unwinnable);
    }

    #[test]
    fn triage_class_display() {
        assert_eq!(TriageClass::Profitable.as_str(), "profitable");
        assert_eq!(TriageClass::Skip.as_str(), "skip");
    }
}
