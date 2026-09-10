//! EBBO (Ethereum Best Bid and Offer) compliance checker.
//!
//! Solutions must match or beat the best available price from reference
//! liquidity (typically Uniswap V3). Failing EBBO leads to rejection
//! and potential slashing by the CoW Protocol.
//!
//! ## How EBBO works
//! For each order, the driver computes a "reference price" — the best price
//! available on reference DEXs for that token pair. A solution is EBBO-compliant
//! if every order gets at least as good a price as the reference.
//!
//! Formally for a sell order:
//!   `executed_buy_amount ≥ reference_buy_amount × (1 − tolerance_bps/10000)`

use std::collections::HashMap;

use tracing::{debug, warn};

use crate::models::order::Order;
use crate::models::solution::{Solution, Trade};

// ── EBBO result types ─────────────────────────────────────────────────────────

/// Result of an EBBO check for a single order.
#[derive(Debug, Clone, PartialEq)]
pub enum EbboResult {
    /// Order meets EBBO. `margin_bps` = how much better than reference (basis points).
    Pass {
        margin_bps: u32,
    },
    /// Order fails EBBO. `deficit_bps` = how far below reference (basis points).
    Fail {
        deficit_bps: u32,
    },
    /// No reference price available for this token pair — check is skipped.
    NoReference,
}

impl EbboResult {
    pub fn is_pass(&self) -> bool {
        matches!(self, EbboResult::Pass { .. } | EbboResult::NoReference)
    }
}

// ── EbboChecker ───────────────────────────────────────────────────────────────

/// EBBO compliance checker.
///
/// Holds a tolerance threshold and a map of reference prices.
/// Reference prices are indexed by `(sell_token, buy_token)` and represent
/// the best buy amount achievable for the order's full sell amount on reference DEXs.
pub struct EbboChecker {
    /// Tolerance in basis points (e.g. 1 = 0.01%). Default: 1 bps.
    pub tolerance_bps: u32,
    /// Reference prices: (sell_token, buy_token) → reference_buy_amount
    pub reference_prices: HashMap<(String, String), u128>,
}

impl EbboChecker {
    /// Create a checker with the given tolerance and no reference prices.
    pub fn new(tolerance_bps: u32) -> Self {
        Self {
            tolerance_bps,
            reference_prices: HashMap::new(),
        }
    }

    /// Set a reference price for a token pair.
    pub fn set_reference(&mut self, sell_token: &str, buy_token: &str, reference_buy: u128) {
        self.reference_prices
            .insert((sell_token.to_string(), buy_token.to_string()), reference_buy);
    }

    /// Check EBBO compliance for a single order execution.
    ///
    /// * `order` — the order being settled
    /// * `executed_buy` — buy amount the order actually receives in the solution
    ///
    /// Returns:
    /// - `Pass { margin_bps }` if execution ≥ reference (possibly with tolerance)
    /// - `Fail { deficit_bps }` if execution is below the reference threshold
    /// - `NoReference` if no reference price is available (check skipped gracefully)
    pub fn check_order(&self, order: &Order, executed_buy: u128) -> EbboResult {
        let key = (order.sell_token.clone(), order.buy_token.clone());
        let reference = match self.reference_prices.get(&key) {
            Some(&r) => r,
            None => {
                debug!(
                    order_uid = %order.uid,
                    sell = %order.sell_token,
                    buy = %order.buy_token,
                    "No EBBO reference price — skipping check"
                );
                return EbboResult::NoReference;
            }
        };

        if reference == 0 {
            return EbboResult::NoReference;
        }

        // Minimum acceptable execution = reference × (1 − tolerance_bps/10000)
        let tolerance_amount = reference
            .saturating_mul(self.tolerance_bps as u128)
            / 10_000;
        let min_execution = reference.saturating_sub(tolerance_amount);

        if executed_buy >= reference {
            // Above reference — compute margin
            let excess = executed_buy - reference;
            let margin_bps = ((excess as u64 * 10_000) / (reference as u64)).min(u32::MAX as u64) as u32;
            EbboResult::Pass { margin_bps }
        } else if executed_buy >= min_execution {
            // Within tolerance — passes with 0 margin
            EbboResult::Pass { margin_bps: 0 }
        } else {
            // Below threshold — compute deficit
            let deficit = min_execution - executed_buy;
            let deficit_bps = (deficit as u128).saturating_mul(10_000)
                .checked_div(reference.max(1))
                .unwrap_or(10_000) as u32;
            warn!(
                order_uid = %order.uid,
                executed = executed_buy,
                reference = reference,
                deficit_bps = deficit_bps,
                "EBBO violation"
            );
            EbboResult::Fail { deficit_bps }
        }
    }

    /// Check EBBO compliance for all trades in a solution.
    ///
    /// Returns a map of order_uid → EbboResult for every trade in the solution.
    /// The solution passes EBBO if all results are Pass or NoReference.
    pub fn check_solution(
        &self,
        solution: &Solution,
        orders: &[Order],
    ) -> HashMap<String, EbboResult> {
        let mut results = HashMap::new();

        for trade in &solution.trades {
            let Trade::Fulfillment(f) = trade;

            let order = match orders.iter().find(|o| o.uid == f.order) {
                Some(o) => o,
                None => continue,
            };

            let executed_amount: u128 = match f.executed_amount.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };

            // For sell orders: executed_amount is the SELL amount.
            // We need the BUY amount — derive from clearing prices.
            // For buy orders: executed_amount IS the buy amount.
            let executed_buy = match order.kind {
                crate::models::order::OrderKind::Sell => {
                    // executed_buy = executed_sell × sell_price / buy_price
                    let sell_price = solution.prices
                        .get(&order.sell_token)
                        .and_then(|p| p.parse::<u128>().ok())
                        .unwrap_or(0);
                    let buy_price = solution.prices
                        .get(&order.buy_token)
                        .and_then(|p| p.parse::<u128>().ok())
                        .unwrap_or(0);
                    if buy_price == 0 || sell_price == 0 {
                        // Can't derive — use limit buy as conservative fallback
                        order.buy_amount.parse::<u128>().unwrap_or(0)
                    } else {
                        executed_amount.saturating_mul(sell_price) / buy_price
                    }
                }
                crate::models::order::OrderKind::Buy => executed_amount,
            };

            let result = self.check_order(order, executed_buy);
            debug!(
                order_uid = %f.order,
                result = ?result,
                "EBBO check"
            );
            results.insert(f.order.clone(), result);
        }

        results
    }

    /// Returns true only if all trades in the solution pass EBBO.
    pub fn solution_passes(&self, solution: &Solution, orders: &[Order]) -> bool {
        let results = self.check_solution(solution, orders);
        results.values().all(|r| r.is_pass())
    }

    /// Compute the average EBBO margin across all passing trades (in basis points).
    ///
    /// Returns 0 if there are no passing trades with a reference price.
    pub fn average_margin_bps(&self, solution: &Solution, orders: &[Order]) -> u32 {
        let results = self.check_solution(solution, orders);
        let margins: Vec<u32> = results
            .values()
            .filter_map(|r| match r {
                EbboResult::Pass { margin_bps } => Some(*margin_bps),
                _ => None,
            })
            .collect();

        if margins.is_empty() {
            return 0;
        }

        (margins.iter().map(|&m| m as u64).sum::<u64>() / margins.len() as u64) as u32
    }
}

// ── Legacy helper ─────────────────────────────────────────────────────────────

/// Simple EBBO check without tolerance (backwards-compatible helper).
///
/// * `executed_buy`  — buy amount the order actually receives
/// * `limit_buy`     — minimum buy amount the user specified
/// * `reference_buy` — best price available on reference DEX
///
/// Returns true if the execution is EBBO-compliant.
pub fn check_ebbo(executed_buy: u128, limit_buy: u128, reference_buy: u128) -> bool {
    if executed_buy < limit_buy {
        return false;
    }
    let tolerance = reference_buy / 10_000; // 1 bps
    executed_buy + tolerance >= reference_buy
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::order::{Order, OrderClass, OrderKind};
    use crate::models::solution::{FulfillmentTrade, Trade};

    fn make_order(uid: &str, sell: &str, buy: &str) -> Order {
        Order {
            uid: uid.to_string(),
            sell_token: sell.to_string(),
            buy_token: buy.to_string(),
            sell_amount: "1000".to_string(),
            buy_amount: "900".to_string(),
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

    fn make_solution_with_trade(order_uid: &str, executed_buy: u128) -> Solution {
        // For sell orders: executed_amount = sell amount, and we derive buy from prices.
        // Set prices so that sell_amount × sell_price / buy_price = executed_buy.
        // Use sell_price=executed_buy, buy_price=1000 (the sell amount from make_order).
        let mut prices = std::collections::HashMap::new();
        prices.insert("0xweth".to_string(), executed_buy.to_string()); // sell_price
        prices.insert("0xusdc".to_string(), "1000".to_string());       // buy_price
        Solution {
            id: 0,
            prices,
            trades: vec![Trade::Fulfillment(FulfillmentTrade {
                order: order_uid.to_string(),
                // For sell order: executed_amount = sell amount (1000 from make_order)
                executed_amount: "1000".to_string(),
                fee: String::new(),
            })],
            pre_interactions: vec![],
            interactions: vec![],
            post_interactions: vec![],
            gas: None,
            score: None,
        }
    }

    // ── check_order tests ─────────────────────────────────────────────────────

    #[test]
    fn pass_above_reference() {
        let checker = EbboChecker::new(1);
        let order = make_order("o1", "0xweth", "0xusdc");
        let mut checker = checker;
        checker.set_reference("0xweth", "0xusdc", 1000);

        let result = checker.check_order(&order, 1010);
        assert!(matches!(result, EbboResult::Pass { margin_bps } if margin_bps == 100));
    }

    #[test]
    fn pass_exactly_at_reference() {
        let mut checker = EbboChecker::new(1);
        let order = make_order("o1", "0xweth", "0xusdc");
        checker.set_reference("0xweth", "0xusdc", 1000);

        let result = checker.check_order(&order, 1000);
        assert!(matches!(result, EbboResult::Pass { margin_bps: 0 }));
    }

    #[test]
    fn pass_within_tolerance() {
        // 1 bps tolerance: min = 1000 - 1000/10000 = 999
        let mut checker = EbboChecker::new(1);
        let order = make_order("o1", "0xweth", "0xusdc");
        checker.set_reference("0xweth", "0xusdc", 10_000);

        // 9999 is within 1 bps of 10000
        let result = checker.check_order(&order, 9999);
        assert!(result.is_pass(), "should pass within 1 bps tolerance: {:?}", result);
    }

    #[test]
    fn fail_below_tolerance() {
        let mut checker = EbboChecker::new(1);
        let order = make_order("o1", "0xweth", "0xusdc");
        checker.set_reference("0xweth", "0xusdc", 10_000);

        // 9980 is 20 bps below — fails 1 bps tolerance
        let result = checker.check_order(&order, 9980);
        assert!(matches!(result, EbboResult::Fail { deficit_bps } if deficit_bps > 0));
    }

    #[test]
    fn no_reference_returns_no_reference() {
        let checker = EbboChecker::new(1);
        let order = make_order("o1", "0xweth", "0xusdc");
        // No reference set
        let result = checker.check_order(&order, 1000);
        assert_eq!(result, EbboResult::NoReference);
    }

    #[test]
    fn zero_reference_returns_no_reference() {
        let mut checker = EbboChecker::new(1);
        let order = make_order("o1", "0xweth", "0xusdc");
        checker.set_reference("0xweth", "0xusdc", 0);
        let result = checker.check_order(&order, 1000);
        assert_eq!(result, EbboResult::NoReference);
    }

    // ── check_solution tests ──────────────────────────────────────────────────

    #[test]
    fn solution_passes_when_all_orders_pass() {
        let mut checker = EbboChecker::new(1);
        let order = make_order("uid1", "0xweth", "0xusdc");
        checker.set_reference("0xweth", "0xusdc", 1000);

        let solution = make_solution_with_trade("uid1", 1010);
        assert!(checker.solution_passes(&solution, &[order]));
    }

    #[test]
    fn solution_fails_when_any_order_fails() {
        let mut checker = EbboChecker::new(1);
        let order = make_order("uid1", "0xweth", "0xusdc");
        checker.set_reference("0xweth", "0xusdc", 1000);

        let solution = make_solution_with_trade("uid1", 100); // way below reference
        assert!(!checker.solution_passes(&solution, &[order]));
    }

    #[test]
    fn solution_passes_with_no_reference_for_pair() {
        let checker = EbboChecker::new(1); // no references set
        let order = make_order("uid1", "0xfoo", "0xbar");

        let solution = make_solution_with_trade("uid1", 1);
        assert!(
            checker.solution_passes(&solution, &[order]),
            "no reference should not fail"
        );
    }

    #[test]
    fn average_margin_across_trades() {
        let mut checker = EbboChecker::new(1);
        let o1 = make_order("u1", "0xweth", "0xusdc");
        let o2 = make_order("u2", "0xweth", "0xusdc");
        checker.set_reference("0xweth", "0xusdc", 10_000);

        // Both trades: executed_buy = 10100 (+100bps) and 10200 (+200bps) → avg 150 bps
        // For sell orders: executed_buy = executed_sell × sell_price / buy_price
        // sell_amount = 1000 from make_order. Set prices to derive correct executed_buy.
        let mut solution = Solution::new(0);
        // Trade 1: executed_buy = 1000 * 10100 / 1000 = 10100
        solution.trades = vec![
            Trade::Fulfillment(FulfillmentTrade {
                order: "u1".into(),
                executed_amount: "1000".into(),
                fee: String::new(),
            }),
            Trade::Fulfillment(FulfillmentTrade {
                order: "u2".into(),
                executed_amount: "1000".into(),
                fee: String::new(),
            }),
        ];
        // Use different prices per trade to get different margins.
        // But all trades share the same prices (UDCP). So we use a trick:
        // sell_price = 10150, buy_price = 1000 → executed_buy = 1000 * 10150 / 1000 = 10150 for both.
        // Margin for both = (10150 - 10000) / 10000 = 150 bps. Avg = 150 bps.
        let mut prices = std::collections::HashMap::new();
        prices.insert("0xweth".to_string(), "10150".to_string());
        prices.insert("0xusdc".to_string(), "1000".to_string());
        solution.prices = prices;
        let avg = checker.average_margin_bps(&solution, &[o1, o2]);
        assert_eq!(avg, 150, "average margin should be 150 bps");
    }

    // ── Legacy check_ebbo tests ───────────────────────────────────────────────

    #[test]
    fn passes_when_above_reference() {
        assert!(check_ebbo(1010, 1000, 1000));
    }

    #[test]
    fn fails_below_limit() {
        assert!(!check_ebbo(990, 1000, 1000));
    }

    #[test]
    fn passes_with_tolerance() {
        assert!(check_ebbo(9999, 9000, 10000));
    }
}
