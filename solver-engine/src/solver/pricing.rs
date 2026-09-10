use std::collections::HashMap;

use crate::models::order::{Order, OrderKind};

// ── UDCP types ────────────────────────────────────────────────────────────────

/// Clearing prices for a batch.
///
/// The CoW settlement contract checks that each filled order satisfies:
///   executed_sell_amount * prices[sell_token] >= executed_buy_amount * prices[buy_token]
///
/// We express prices as:
///   prices[sell_token] = total_buy_out
///   prices[buy_token]  = total_sell_in
///
/// This ensures the ratio is always consistent with the executed fills.
#[derive(Debug, Clone)]
pub struct ClearingPrices {
    /// token address → price (no fixed scale; ratio matters)
    pub prices: HashMap<String, u128>,
}

impl ClearingPrices {
    pub fn new(prices: HashMap<String, u128>) -> Self {
        Self { prices }
    }

    pub fn get(&self, token: &str) -> Option<u128> {
        self.prices.get(token).copied()
    }

    /// Serialize for the CoW settlement contract (token → decimal string).
    pub fn to_string_map(&self) -> HashMap<String, String> {
        self.prices
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect()
    }
}

// ── UDCP computation ──────────────────────────────────────────────────────────

/// Compute Uniform Directional Clearing Prices (UDCP).
///
/// All orders trading the same pair in the same direction MUST receive the same
/// price.  The price is derived from the aggregate fill:
///
///   prices[sell_token] = total_buy_out
///   prices[buy_token]  = total_sell_in
///
/// Returns None if inputs are empty, mismatched in length, or overflow occurs.
pub fn compute_udcp(
    sell_token: &str,
    buy_token: &str,
    amounts_in: &[u128],
    amounts_out: &[u128],
) -> Option<ClearingPrices> {
    if amounts_in.is_empty() || amounts_in.len() != amounts_out.len() {
        return None;
    }

    let total_in: u128 = amounts_in
        .iter()
        .try_fold(0u128, |acc, &x| acc.checked_add(x))?;
    let total_out: u128 = amounts_out
        .iter()
        .try_fold(0u128, |acc, &x| acc.checked_add(x))?;

    if total_in == 0 || total_out == 0 {
        return None;
    }

    let mut prices = HashMap::new();
    prices.insert(sell_token.to_string(), total_out);
    prices.insert(buy_token.to_string(), total_in);

    Some(ClearingPrices::new(prices))
}

/// Verify that the UDCP constraint is satisfied for all provided orders.
///
/// For each order:
/// - Sell order: sell_amount * prices[sell] >= buy_amount * prices[buy]
/// - Buy order:  buy_amount  * prices[buy]  <= sell_amount * prices[sell]
///
/// Returns Ok(()) if all orders pass; Err with description if any fail.
pub fn verify_udcp(orders: &[&Order], prices: &ClearingPrices) -> Result<(), String> {
    for order in orders {
        let sell_price = prices
            .get(&order.sell_token)
            .ok_or_else(|| format!("Missing price for sell_token {}", order.sell_token))?;
        let buy_price = prices
            .get(&order.buy_token)
            .ok_or_else(|| format!("Missing price for buy_token {}", order.buy_token))?;

        if sell_price == 0 || buy_price == 0 {
            return Err(format!("Zero price for order {}", order.uid));
        }

        let sell_amount = order
            .sell_amount
            .parse::<u128>()
            .map_err(|e| format!("Invalid sell_amount for {}: {e}", order.uid))?;
        let buy_amount = order
            .buy_amount
            .parse::<u128>()
            .map_err(|e| format!("Invalid buy_amount for {}: {e}", order.uid))?;

        match order.kind {
            OrderKind::Sell => {
                // sell_amount * sell_price >= buy_amount * buy_price
                let lhs = sell_amount.checked_mul(sell_price);
                let rhs = buy_amount.checked_mul(buy_price);
                match (lhs, rhs) {
                    (Some(l), Some(r)) if l >= r => {}
                    _ => {
                        return Err(format!(
                            "UDCP violated for sell order {}: clearing price too low",
                            order.uid
                        ));
                    }
                }
            }
            OrderKind::Buy => {
                // buy_amount * buy_price <= sell_amount * sell_price
                let lhs = buy_amount.checked_mul(buy_price);
                let rhs = sell_amount.checked_mul(sell_price);
                match (lhs, rhs) {
                    (Some(l), Some(r)) if l <= r => {}
                    _ => {
                        return Err(format!(
                            "UDCP violated for buy order {}: clearing cost too high",
                            order.uid
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Convenience: compute UDCP from a slice of (sell_in, buy_out) route pairs.
pub fn enforce_udcp(
    sell_token: &str,
    buy_token: &str,
    routes: &[(u128, u128)],
) -> Option<ClearingPrices> {
    let amounts_in: Vec<u128> = routes.iter().map(|(a, _)| *a).collect();
    let amounts_out: Vec<u128> = routes.iter().map(|(_, b)| *b).collect();
    compute_udcp(sell_token, buy_token, &amounts_in, &amounts_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::order::{Order, OrderClass, OrderKind};

    fn make_order(
        uid: &str,
        sell: &str,
        buy: &str,
        sell_amt: u128,
        buy_amt: u128,
        kind: OrderKind,
    ) -> Order {
        Order {
            uid: uid.to_string(),
            sell_token: sell.to_string(),
            buy_token: buy.to_string(),
            sell_amount: sell_amt.to_string(),
            buy_amount: buy_amt.to_string(),
            fee_amount: "0".to_string(),
            kind,
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

    #[test]
    fn compute_udcp_single_trade() {
        let prices = compute_udcp("0xweth", "0xusdc", &[1_000], &[2_000]).unwrap();
        assert_eq!(prices.get("0xweth"), Some(2_000)); // total_out
        assert_eq!(prices.get("0xusdc"), Some(1_000)); // total_in
    }

    #[test]
    fn compute_udcp_multiple_trades_sums() {
        let prices =
            compute_udcp("0xweth", "0xusdc", &[1_000, 2_000], &[2_000, 4_000]).unwrap();
        assert_eq!(prices.get("0xweth"), Some(6_000));
        assert_eq!(prices.get("0xusdc"), Some(3_000));
    }

    #[test]
    fn compute_udcp_empty_returns_none() {
        assert!(compute_udcp("0xa", "0xb", &[], &[]).is_none());
    }

    #[test]
    fn compute_udcp_mismatched_returns_none() {
        assert!(compute_udcp("0xa", "0xb", &[1, 2], &[3]).is_none());
    }

    #[test]
    fn verify_udcp_passes_for_valid_sell_order() {
        let prices = compute_udcp("0xweth", "0xusdc", &[1_000], &[2_000]).unwrap();
        // 1000 WETH → want ≥1500 USDC; price gives 2000 → satisfied
        let order = make_order("u1", "0xweth", "0xusdc", 1_000, 1_500, OrderKind::Sell);
        assert!(verify_udcp(&[&order], &prices).is_ok());
    }

    #[test]
    fn verify_udcp_fails_when_insufficient() {
        // prices: sell=500, buy=1000 → only 0.5 USDC per WETH
        let prices = compute_udcp("0xweth", "0xusdc", &[1_000], &[500]).unwrap();
        let order = make_order("u2", "0xweth", "0xusdc", 1_000, 2_000, OrderKind::Sell);
        assert!(verify_udcp(&[&order], &prices).is_err());
    }

    #[test]
    fn enforce_udcp_returns_prices() {
        let prices = enforce_udcp("0xa", "0xb", &[(100, 200), (150, 300)]).unwrap();
        assert_eq!(prices.get("0xa"), Some(500)); // total_out
        assert_eq!(prices.get("0xb"), Some(250)); // total_in
    }

    #[test]
    fn to_string_map_converts_correctly() {
        let prices = compute_udcp("0xweth", "0xusdc", &[1000], &[2000]).unwrap();
        let map = prices.to_string_map();
        assert_eq!(map.get("0xweth"), Some(&"2000".to_string()));
        assert_eq!(map.get("0xusdc"), Some(&"1000".to_string()));
    }
}
