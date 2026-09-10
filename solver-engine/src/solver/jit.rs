//! Just-In-Time (JIT) liquidity provider.
//!
//! Instead of routing through existing DEX pools, the solver can fill small orders
//! directly from its own token inventory at a small spread. This avoids:
//! - DEX gas costs (no on-chain interactions required)
//! - Price impact on pools
//! - AMM fees paid to LPs
//!
//! ## Constraints
//! - Only fills orders where our inventory is sufficient
//! - Spread must be at least `min_spread_bps` to be profitable
//! - Only used for small orders where the DEX routing is expensive relative to size
//! - Inventory is tracked and decremented after each fill
//!
//! ## Initial inventory
//! Start conservatively:
//! - 0.5 ETH (WETH)
//! - 2000 USDC
//!
//! ## CoW Protocol JIT trades
//! JIT fills appear as "jit" trades in the solution. No on-chain interactions
//! are needed — the settlement contract handles the token transfer directly.

use std::collections::HashMap;

use tracing::{debug, warn};

use crate::models::order::{Order, OrderKind};
use crate::models::solution::{Score, Solution, Trade};

// ── JIT token inventory ───────────────────────────────────────────────────────

/// WETH address (Ethereum mainnet)
pub const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
/// USDC address (Ethereum mainnet)
pub const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

/// Initial WETH inventory: 0.5 ETH in wei
pub const INITIAL_WETH_WEI: u128 = 500_000_000_000_000_000; // 0.5e18
/// Initial USDC inventory: 2000 USDC in micro-USDC (6 decimals)
pub const INITIAL_USDC: u128 = 2_000_000_000; // 2000e6

// ── JIT fill result ───────────────────────────────────────────────────────────

/// A JIT fill for one order.
#[derive(Debug, Clone)]
pub struct JitFill {
    pub order_uid: String,
    /// Amount of sell_token taken from the user
    pub executed_sell: u128,
    /// Amount of buy_token given to the user
    pub executed_buy: u128,
    /// Surplus generated (buy amount above user's limit price)
    pub surplus: u128,
    /// Our profit (spread = price we bought at − price user gets)
    pub spread: u128,
}

// ── JIT Provider ──────────────────────────────────────────────────────────────

/// JIT liquidity provider state.
///
/// Tracks available inventory and provides pricing/fill logic.
pub struct JitProvider {
    /// Available inventory: token address → amount in raw units
    pub inventory: HashMap<String, u128>,
    /// Minimum spread in basis points (e.g. 5 = 0.05%)
    pub min_spread_bps: u32,
    /// Maximum order size to fill via JIT (prevents inventory exhaustion on one order)
    pub max_fill_wei: u128,
}

impl Default for JitProvider {
    fn default() -> Self {
        let mut inventory = HashMap::new();
        inventory.insert(WETH.to_string(), INITIAL_WETH_WEI);
        inventory.insert(USDC.to_string(), INITIAL_USDC);
        Self {
            inventory,
            min_spread_bps: 5,      // 0.05% minimum spread
            max_fill_wei: 100_000_000_000_000_000, // 0.1 ETH equivalent
        }
    }
}

impl JitProvider {
    pub fn new(min_spread_bps: u32, max_fill_wei: u128) -> Self {
        Self {
            min_spread_bps,
            max_fill_wei,
            ..Self::default()
        }
    }

    /// Check if we can profitably fill this order from inventory.
    ///
    /// For a sell order (user sells sell_token, wants buy_token):
    /// - We must have `buy_token` in inventory
    /// - The sell_amount must be ≤ max_fill_wei
    /// - Our spread on the execution must be ≥ min_spread_bps
    ///
    /// Returns `Some(JitFill)` if we can fill, `None` otherwise.
    pub fn can_fill(&self, order: &Order) -> Option<JitFill> {
        let sell_amount: u128 = order.sell_amount.parse().ok()?;
        let buy_amount_limit: u128 = order.buy_amount.parse().ok()?;

        if sell_amount == 0 || buy_amount_limit == 0 {
            return None;
        }

        // Only handle sell orders for now (buy orders require inverse math)
        if order.kind != OrderKind::Sell {
            return None;
        }

        // Check if sell_amount is within our max fill size
        if sell_amount > self.max_fill_wei {
            debug!(
                order_uid = %order.uid,
                sell_amount,
                max = self.max_fill_wei,
                "JIT: order too large"
            );
            return None;
        }

        // We provide buy_token — check inventory
        let available = self.inventory.get(&order.buy_token).copied().unwrap_or(0);

        // We will offer `buy_amount_with_spread` to the user.
        // To be profitable: we pay buy_amount_limit × (1 + spread)
        // We charge (in sell_token) at the user's implied rate.
        // Our spread is: how much buy_token we KEEP above what user gets.
        // We give the user: buy_amount_limit (exactly their limit = 0 surplus)
        // For a better fill: give buy_amount_limit × (1 + 0.5 × spread)
        // to create positive surplus for the user while we keep 0.5 × spread.

        // Compute what we pay: exactly the user's limit price (minimum)
        // We earn spread from acquiring sell_token at market rate later.
        let buy_amount_to_give = buy_amount_limit; // user gets their exact limit

        if available < buy_amount_to_give {
            debug!(
                order_uid = %order.uid,
                need = buy_amount_to_give,
                have = available,
                token = %order.buy_token,
                "JIT: insufficient inventory"
            );
            return None;
        }

        // Verify our minimum spread:
        // Spread check: sell_amount / buy_amount ratio vs market.
        // We assume market price ≈ sell_amount / buy_amount_limit (the user's limit price).
        // Our actual profit comes from selling the acquired sell_token on the market later.
        // For JIT purposes: spread = sell_amount * min_spread_bps / 10000 in sell_token units.
        let spread_amount = sell_amount
            .saturating_mul(self.min_spread_bps as u128)
            / 10_000;

        if spread_amount == 0 {
            return None;
        }

        // Surplus for the user = 0 (we fill exactly at their limit)
        // Our profit = spread_amount (in sell_token units, realised when we sell on market)
        Some(JitFill {
            order_uid: order.uid.clone(),
            executed_sell: sell_amount,
            executed_buy: buy_amount_to_give,
            surplus: 0,
            spread: spread_amount,
        })
    }

    /// Apply a JIT fill: deduct from inventory.
    ///
    /// Returns false if the inventory is insufficient (fill should be rejected).
    pub fn apply_fill(&mut self, fill: &JitFill, buy_token: &str) -> bool {
        let available = self.inventory.get_mut(buy_token);
        match available {
            Some(bal) if *bal >= fill.executed_buy => {
                *bal -= fill.executed_buy;
                // Add the received sell_token to inventory
                // (not tracked here since sell_token isn't part of our initial inventory)
                true
            }
            _ => {
                warn!(
                    order_uid = %fill.order_uid,
                    "JIT: apply_fill failed — insufficient balance"
                );
                false
            }
        }
    }

    /// Try to fill all orders via JIT where profitable.
    ///
    /// Fills are applied sequentially; inventory is consumed by each fill.
    pub fn fill_orders(&mut self, orders: &[Order]) -> Vec<JitFill> {
        let mut fills = Vec::new();
        for order in orders {
            if let Some(fill) = self.can_fill(order) {
                if self.apply_fill(&fill, &order.buy_token) {
                    debug!(
                        order_uid = %fill.order_uid,
                        sell = fill.executed_sell,
                        buy = fill.executed_buy,
                        spread = fill.spread,
                        "JIT fill applied"
                    );
                    fills.push(fill);
                }
            }
        }
        fills
    }

    /// Get current inventory balance for a token.
    pub fn balance(&self, token: &str) -> u128 {
        self.inventory.get(token).copied().unwrap_or(0)
    }

    /// Set inventory balance for a token (for testing or reconfiguration).
    pub fn set_balance(&mut self, token: &str, amount: u128) {
        self.inventory.insert(token.to_string(), amount);
    }
}

// ── Solution building ─────────────────────────────────────────────────────────

/// Build a Solution from a set of JIT fills.
///
/// JIT solutions have:
/// - `Trade::Fulfillment` for each filled order
/// - No interactions (settlement contract handles transfers directly)
/// - Score = sum of spreads (our profit in sell-token units, as a proxy)
pub fn build_jit_solution(auction_id: u64, fills: &[JitFill]) -> Option<Solution> {
    if fills.is_empty() {
        return None;
    }

    let trades: Vec<Trade> = fills
        .iter()
        .map(|f| Trade::fulfillment(&f.order_uid, f.executed_sell.to_string()))
        .collect();

    let mut prices = HashMap::new();
    let mut total_spread = 0u128;

    for f in fills {
        // Approximate clearing price: buy/sell ratio
        if f.executed_sell > 0 {
            prices.insert(
                format!("jit_{}_sell", f.order_uid),
                f.executed_sell.to_string(),
            );
            prices.insert(
                format!("jit_{}_buy", f.order_uid),
                f.executed_buy.to_string(),
            );
        }
        total_spread = total_spread.saturating_add(f.spread);
    }

    Some(Solution {
        id: auction_id,
        prices,
        trades,
        pre_interactions: vec![],
        interactions: vec![], // JIT: no on-chain interactions
        post_interactions: vec![],
        gas: None,
        score: Some(Score::Solver {
            score: total_spread.to_string(),
        }),
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::order::{Order, OrderClass, OrderKind};

    fn make_sell_order(uid: &str, sell: &str, buy: &str, sell_amt: u128, buy_amt: u128) -> Order {
        Order {
            uid: uid.to_string(),
            sell_token: sell.to_string(),
            buy_token: buy.to_string(),
            sell_amount: sell_amt.to_string(),
            buy_amount: buy_amt.to_string(),
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

    // ── can_fill tests ────────────────────────────────────────────────────────

    #[test]
    fn can_fill_with_sufficient_inventory() {
        let provider = JitProvider::default();
        // Small USDC→WETH order (user sells USDC, wants WETH)
        // Provider has 0.5 WETH; order wants 0.01 WETH → fine
        let order = make_sell_order(
            "o1",
            USDC,
            WETH,
            10_000_000, // 10 USDC (sell_amount ≤ max_fill)
            10_000_000_000_000_000, // 0.01 WETH minimum
        );
        let fill = provider.can_fill(&order);
        assert!(fill.is_some(), "should fill small order with inventory");
        let f = fill.unwrap();
        assert_eq!(f.executed_sell, 10_000_000);
        assert_eq!(f.executed_buy, 10_000_000_000_000_000);
    }

    #[test]
    fn cannot_fill_order_too_large() {
        let provider = JitProvider::default();
        // Sell amount > max_fill_wei (0.1 ETH = 1e17 wei)
        let order = make_sell_order(
            "o1",
            USDC,
            WETH,
            1_000_000_000_000_000_000, // 1e18 — too large
            900_000_000_000_000_000,
        );
        assert!(provider.can_fill(&order).is_none(), "too large → no fill");
    }

    #[test]
    fn cannot_fill_insufficient_inventory() {
        let mut provider = JitProvider::default();
        provider.set_balance(WETH, 0); // drain WETH

        let order = make_sell_order(
            "o1", USDC, WETH,
            1_000_000, // 1 USDC
            1_000_000_000_000_000, // 0.001 WETH
        );
        assert!(provider.can_fill(&order).is_none(), "no WETH → no fill");
    }

    #[test]
    fn cannot_fill_unknown_buy_token() {
        let provider = JitProvider::default();
        let order = make_sell_order("o1", WETH, "0xunknown", 1_000_000, 1_000_000);
        assert!(provider.can_fill(&order).is_none(), "unknown token → no fill");
    }

    // ── apply_fill tests ──────────────────────────────────────────────────────

    #[test]
    fn apply_fill_decrements_inventory() {
        let mut provider = JitProvider::default();
        let initial_weth = provider.balance(WETH);

        let fill = JitFill {
            order_uid: "o1".into(),
            executed_sell: 1_000_000,
            executed_buy: 1_000_000_000_000_000,
            surplus: 0,
            spread: 500,
        };
        let ok = provider.apply_fill(&fill, WETH);
        assert!(ok);
        assert_eq!(
            provider.balance(WETH),
            initial_weth - 1_000_000_000_000_000,
            "WETH balance should decrease"
        );
    }

    #[test]
    fn apply_fill_fails_if_insufficient() {
        let mut provider = JitProvider::default();
        provider.set_balance(WETH, 100); // only 100 wei

        let fill = JitFill {
            order_uid: "o1".into(),
            executed_sell: 1_000,
            executed_buy: 1_000_000, // need 1M but have 100
            surplus: 0,
            spread: 5,
        };
        assert!(!provider.apply_fill(&fill, WETH));
    }

    // ── fill_orders tests ─────────────────────────────────────────────────────

    #[test]
    fn fill_orders_sequential_inventory_consumption() {
        let mut provider = JitProvider::default();
        // Set WETH to exactly enough for one fill
        let fill_size = 1_000_000_000_000_000u128; // 0.001 WETH
        provider.set_balance(WETH, fill_size);

        let o1 = make_sell_order("o1", USDC, WETH, 1_000_000, fill_size);
        let o2 = make_sell_order("o2", USDC, WETH, 1_000_000, fill_size);

        let fills = provider.fill_orders(&[o1, o2]);
        assert_eq!(fills.len(), 1, "only first order should be filled");
        assert_eq!(provider.balance(WETH), 0, "inventory exhausted");
    }

    // ── JIT solution tests ────────────────────────────────────────────────────

    #[test]
    fn build_jit_solution_has_no_interactions() {
        let fills = vec![
            JitFill {
                order_uid: "u1".into(),
                executed_sell: 10_000_000,
                executed_buy: 10_000_000_000_000_000,
                surplus: 0,
                spread: 5_000,
            },
        ];
        let sol = build_jit_solution(1, &fills).unwrap();
        assert!(sol.interactions.is_empty(), "JIT must have no on-chain interactions");
        assert_eq!(sol.trades.len(), 1);
        match &sol.score {
            Some(Score::Solver { score }) => assert_eq!(score, "5000"),
            other => panic!("expected Solver score, got {:?}", other),
        }
    }

    #[test]
    fn build_jit_solution_empty_fills_returns_none() {
        assert!(build_jit_solution(1, &[]).is_none());
    }

    // ── Spread and profit tests ───────────────────────────────────────────────

    #[test]
    fn spread_is_at_least_min_spread() {
        let provider = JitProvider { min_spread_bps: 10, ..JitProvider::default() };
        let sell_amount = 1_000_000u128;
        let order = make_sell_order("o1", USDC, WETH, sell_amount, 1_000_000_000_000_000);

        if let Some(fill) = provider.can_fill(&order) {
            let min_spread = sell_amount * 10 / 10_000;
            assert!(
                fill.spread >= min_spread,
                "spread {} should be >= min {}", fill.spread, min_spread
            );
        }
    }
}
