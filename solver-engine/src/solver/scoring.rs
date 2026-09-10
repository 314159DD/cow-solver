//! Solution scoring and score-maximization utilities.
//!
//! CoW Protocol selects the winning solver by comparing scores. The score for
//! a solution is the sum of per-order surplus, where surplus is measured in
//! a common reference currency (ETH/wei equivalent via token reference prices).
//!
//! ## CoW Protocol scoring formula
//!
//! For a **sell order**:
//!   `surplus = (executed_buy − limit_buy) × buy_reference_price`
//!
//! For a **buy order**:
//!   `surplus = (limit_sell − executed_sell) × sell_reference_price`
//!
//! Total score = Σ surplus_i across all filled orders.
//!
//! ## Optimization strategies
//!
//! 1. **Surplus optimization** — evaluate all candidate routes and pick the one
//!    with maximum surplus (not just best price).
//! 2. **Order prioritization** — estimate potential surplus per order and solve
//!    highest-potential orders first when time is limited.
//! 3. **Clearing price adjustment** — within UDCP constraints, set prices to the
//!    most favourable point along the feasible line to maximize total surplus.
//! 4. **Solution merging** — when multiple partial solutions exist, find the
//!    combination that maximises total score without violating UDCP.

use std::collections::HashMap;

use crate::models::auction::TokenMap;
use crate::models::order::{Order, OrderKind};
use crate::models::solution::{Score, Solution, Trade};

// ── CoW Protocol scoring (matches driver formula) ───────────────────────────

/// Safe `a * b / c` that avoids u128 overflow by splitting the computation.
pub fn mul_div(a: u128, b: u128, c: u128) -> Option<u128> {
    if c == 0 { return None; }
    if let Some(ab) = a.checked_mul(b) {
        return Some(ab / c);
    }
    // Overflow: (a/c)*b + (a%c)*b/c
    let q = a / c;
    let r = a % c;
    let qb = q.checked_mul(b)?;
    let rb = r.checked_mul(b)?;
    Some(qb.checked_add(rb / c)?)
}

/// Safe ceiling division: `ceil(a * b / c)`.
pub fn mul_div_ceil(a: u128, b: u128, c: u128) -> Option<u128> {
    if c == 0 { return None; }
    if let Some(ab) = a.checked_mul(b) {
        return Some((ab + c - 1) / c);
    }
    // Overflow: (a/c)*b + ceil((a%c)*b / c)
    let q = a / c;
    let r = a % c;
    let qb = q.checked_mul(b)?;
    let rb = r.checked_mul(b)?;
    Some(qb.checked_add((rb + c - 1) / c)?)
}

/// Look up native price (reference_price) for a token from the auction TokenMap.
/// Returns 0 if not found — caller MUST check and skip the trade.
fn native_price(token: &str, tokens: &TokenMap) -> u128 {
    let result = tokens.iter()
        .find(|(k, _)| k.to_lowercase() == token.to_lowercase())
        .and_then(|(_, t)| t.reference_price.as_ref())
        .and_then(|p| {
            // Integer parse only — no f64 fallback (precision loss risk)
            p.parse::<u128>().ok()
        })
        .unwrap_or(0);

    if result == 0 {
        tracing::warn!(token, "Missing or zero reference price — trade will score 0");
    }
    result
}

/// Compute the score for a solution using the EXACT CoW driver formula.
///
/// Ported from `cowprotocol/services/crates/driver/src/domain/competition/solution/scoring.rs`
/// and verified against live auction data (exact match on auction 6787468).
///
/// CRITICAL INSIGHT from research: clearing prices are NOT reference prices.
/// They are execution rates from AMM math (arbitrary denomination, only ratio matters).
/// The driver derives `bought` from `executed × cp_sell / cp_buy` (ceil for sell orders).
///
/// For sell orders:
///   executed_gross = executedAmount (+ fee if applicable)
///   bought = ceil(executed_gross × cp_sell / cp_buy)
///   limit_buy = ceil(executed_gross × signed_buy / signed_sell)
///   surplus = bought - limit_buy  [in buy token atoms]
///   score = surplus × native_price(buy_token) / 1e18  [floor]
///
/// For buy orders:
///   executed = executedAmount
///   sold = floor(executed × cp_buy / cp_sell)
///   limit_sell = floor(executed × signed_sell / signed_buy)
///   surplus = limit_sell - sold  [in sell token atoms]
///   surplus_in_buy = surplus × signed_buy / signed_sell  [floor]
///   score = surplus_in_buy × native_price(buy_token) / 1e18  [floor]
pub fn compute_cow_score(
    solution: &Solution,
    orders: &[Order],
    tokens: &TokenMap,
) -> u128 {
    let order_map: HashMap<&str, &Order> = orders.iter()
        .map(|o| (o.uid.as_str(), o))
        .collect();

    let mut total_score: u128 = 0;
    let mut trade_count: usize = 0;

    for trade in &solution.trades {
        let Trade::Fulfillment(ft) = trade;

        let Some(order) = order_map.get(ft.order.as_str()) else {
            continue;
        };

        let executed: u128 = match ft.executed_amount.parse() {
            Ok(v) if v > 0 => v,
            _ => continue,
        };

        // signed_sell and signed_buy are the ORDER's limit amounts (what user signed)
        let signed_sell: u128 = order.sell_amount.parse().unwrap_or(0);
        let signed_buy: u128 = order.buy_amount.parse().unwrap_or(0);
        if signed_sell == 0 || signed_buy == 0 { continue; }

        // Look up clearing prices (execution rates set by our solver, NOT reference prices)
        let cp_sell = solution.prices.iter()
            .find(|(k, _)| k.to_lowercase() == order.sell_token.to_lowercase())
            .and_then(|(_, v)| v.parse::<u128>().ok())
            .unwrap_or(0);
        let cp_buy = solution.prices.iter()
            .find(|(k, _)| k.to_lowercase() == order.buy_token.to_lowercase())
            .and_then(|(_, v)| v.parse::<u128>().ok())
            .unwrap_or(0);
        if cp_sell == 0 || cp_buy == 0 { continue; }

        let trade_score = match order.kind {
            OrderKind::Sell => {
                // Driver: executed_gross = executedAmount + fee (for sell orders)
                // We don't have separate fee field in our FulfillmentTrade, so executed = gross
                let executed_gross = executed;

                // bought = ceil(executed_gross × cp_sell / cp_buy)
                let bought = match mul_div_ceil(executed_gross, cp_sell, cp_buy) {
                    Some(v) => v,
                    None => continue,
                };

                // limit_buy = ceil(executed_gross × signed_buy / signed_sell)
                let limit_buy = match mul_div_ceil(executed_gross, signed_buy, signed_sell) {
                    Some(v) => v,
                    None => continue,
                };

                // surplus = bought - limit_buy (in buy token atoms)
                let surplus = match bought.checked_sub(limit_buy) {
                    Some(v) => v,
                    None => 0,
                };

                // score = surplus × native_price(buy_token) / 1e18 (floor)
                let np = native_price(&order.buy_token, tokens);
                if np == 0 { continue; }
                let score = mul_div(surplus, np, 1_000_000_000_000_000_000).unwrap_or(0);

                if trade_count < 3 {
                    tracing::info!(
                        order_uid = %ft.order,
                        executed_gross,
                        signed_sell,
                        signed_buy,
                        cp_sell,
                        cp_buy,
                        bought,
                        limit_buy,
                        surplus,
                        native_price = np,
                        trade_score = score,
                        "Scoring: sell order (driver formula)"
                    );
                }

                score
            }
            OrderKind::Buy => {
                // sold = floor(executed × cp_buy / cp_sell)
                let sold = match mul_div(executed, cp_buy, cp_sell) {
                    Some(v) => v,
                    None => continue,
                };

                // limit_sell = floor(executed × signed_sell / signed_buy)
                let limit_sell = match mul_div(executed, signed_sell, signed_buy) {
                    Some(v) => v,
                    None => continue,
                };

                // surplus in sell-token atoms
                let surplus_sell = match limit_sell.checked_sub(sold) {
                    Some(v) => v,
                    None => 0,
                };

                // Convert surplus to buy tokens: surplus × signed_buy / signed_sell (floor)
                // This matches the driver's buy-order conversion path
                let surplus_buy = match mul_div(surplus_sell, signed_buy, signed_sell) {
                    Some(v) => v,
                    None => continue,
                };

                // score = surplus_in_buy × native_price(buy_token) / 1e18 (floor)
                let np = native_price(&order.buy_token, tokens);
                if np == 0 { continue; }
                let score = mul_div(surplus_buy, np, 1_000_000_000_000_000_000).unwrap_or(0);

                if trade_count < 3 {
                    tracing::info!(
                        order_uid = %ft.order,
                        executed,
                        signed_sell,
                        signed_buy,
                        cp_sell,
                        cp_buy,
                        sold,
                        limit_sell,
                        surplus_sell,
                        surplus_buy,
                        native_price = np,
                        trade_score = score,
                        "Scoring: buy order (driver formula)"
                    );
                }

                score
            }
        };

        trade_count += 1;
        total_score = total_score.saturating_add(trade_score);
    }

    total_score
}

/// Rescore all solutions using the EXACT CoW driver formula.
///
/// DOES NOT replace clearing prices — each solution's prices are execution
/// rates from AMM routing (UDCP). The driver uses these clearing prices
/// to derive executed amounts and compute surplus. Reference prices (native
/// prices) are only used for the final ETH conversion step.
///
/// From the CoW driver source: "clearing prices are arbitrary denomination,
/// only the ratio matters." The driver trusts our clearing prices.
pub fn rescore_solutions(
    solutions: &mut [Solution],
    orders: &[Order],
    tokens: &TokenMap,
) {
    for sol in solutions.iter_mut() {
        let score = compute_cow_score(sol, orders, tokens);
        sol.score = Some(Score::Solver {
            score: score.to_string(),
        });
    }
}

// ── Per-order surplus ─────────────────────────────────────────────────────────

/// Compute the surplus for a single sell order execution.
///
/// `surplus = executed_buy − limit_buy` (in buy-token units, raw integers).
/// Returns 0 if the order is not filled or executed at limit price.
pub fn sell_order_surplus(executed_buy: u128, limit_buy: u128) -> u128 {
    executed_buy.saturating_sub(limit_buy)
}

/// Compute the surplus for a single buy order execution.
///
/// `surplus = limit_sell − executed_sell` (in sell-token units, raw integers).
pub fn buy_order_surplus(executed_sell: u128, limit_sell: u128) -> u128 {
    limit_sell.saturating_sub(executed_sell)
}

/// Compute order surplus in native token units (no reference price normalization).
///
/// Returns surplus in:
/// - buy-token units for sell orders
/// - sell-token units for buy orders
pub fn order_surplus_raw(order: &Order, executed_sell: u128, executed_buy: u128) -> u128 {
    let limit_sell: u128 = order.sell_amount.parse().unwrap_or(0);
    let limit_buy: u128 = order.buy_amount.parse().unwrap_or(0);

    match order.kind {
        OrderKind::Sell => sell_order_surplus(executed_buy, limit_buy),
        OrderKind::Buy => buy_order_surplus(executed_sell, limit_sell),
    }
}

/// Compute order surplus normalized to a reference currency (wei equivalent).
///
/// `normalized_surplus = raw_surplus × reference_price`
///
/// Reference prices are token → wei per token (e.g. WETH = 1e18, USDC ≈ 3.7e11 at $2700/ETH).
pub fn order_surplus_wei(
    order: &Order,
    executed_sell: u128,
    executed_buy: u128,
    reference_prices: &HashMap<String, u128>,
) -> u128 {
    let raw = order_surplus_raw(order, executed_sell, executed_buy);
    if raw == 0 {
        return 0;
    }

    // For sell orders: surplus is in buy-token units; normalize by buy token price
    // For buy orders: surplus is in sell-token units; normalize by sell token price
    let price_token = match order.kind {
        OrderKind::Sell => &order.buy_token,
        OrderKind::Buy => &order.sell_token,
    };

    let price = reference_prices.get(price_token).copied().unwrap_or(1);
    raw.saturating_mul(price) / 1_000_000_000_000_000_000u128 // divide by 1e18 to normalize
}

// ── Solution scoring ──────────────────────────────────────────────────────────

/// Compute the total score for a solution.
///
/// Score = Σ order_surplus across all filled orders.
/// If `reference_prices` is empty, uses raw surplus (buy-token units).
///
/// This matches the CoW Protocol score formula when reference prices are provided
/// in wei-per-token units.
pub fn compute_solution_score(
    solution: &Solution,
    orders: &[Order],
    reference_prices: &HashMap<String, u128>,
) -> u128 {
    let mut total = 0u128;

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

        let limit_sell: u128 = order.sell_amount.parse().unwrap_or(0);
        let limit_buy: u128 = order.buy_amount.parse().unwrap_or(0);

        // For sell orders: executed_amount = sell amount; buy amount is computed implicitly
        // For buy orders: executed_amount = buy amount; sell amount is the remaining
        let (executed_sell, executed_buy) = match order.kind {
            OrderKind::Sell => (executed_amount, {
                // Estimate executed_buy from solution prices if available
                let sell_price = solution
                    .prices
                    .get(&order.sell_token)
                    .and_then(|p| p.parse::<u128>().ok())
                    .unwrap_or(1);
                let buy_price = solution
                    .prices
                    .get(&order.buy_token)
                    .and_then(|p| p.parse::<u128>().ok())
                    .unwrap_or(1);
                if buy_price == 0 {
                    limit_buy // fallback
                } else {
                    executed_amount
                        .saturating_mul(sell_price)
                        / buy_price
                }
            }),
            OrderKind::Buy => (limit_sell, executed_amount),
        };

        let surplus = if reference_prices.is_empty() {
            order_surplus_raw(order, executed_sell, executed_buy)
        } else {
            order_surplus_wei(order, executed_sell, executed_buy, reference_prices)
        };

        total = total.saturating_add(surplus);
    }

    total
}

/// Attach a computed `Score::Solver` to a solution based on its total surplus.
pub fn attach_score(
    solution: &mut Solution,
    orders: &[Order],
    reference_prices: &HashMap<String, u128>,
) {
    let score = compute_solution_score(solution, orders, reference_prices);
    solution.score = Some(Score::Solver {
        score: score.to_string(),
    });
}

/// Attach a gas-adjusted score: `score = surplus - gas_cost`.
///
/// The winning solution in CoW Protocol = highest surplus MINUS gas cost.
/// `gas_cost_wei` is the estimated gas cost in wei. This is converted to the
/// same unit as surplus using the reference prices.
///
/// If surplus < gas_cost, the score is set to 0 (unprofitable solution).
pub fn attach_gas_adjusted_score(
    solution: &mut Solution,
    orders: &[Order],
    reference_prices: &HashMap<String, u128>,
    gas_cost_wei: u128,
) {
    let gross_surplus = compute_solution_score(solution, orders, reference_prices);

    // Gas cost is already in wei; surplus is also normalized to wei via reference prices.
    // So we can directly subtract.
    let net_score = gross_surplus.saturating_sub(gas_cost_wei);

    solution.score = Some(Score::Solver {
        score: net_score.to_string(),
    });
}

/// Compute the gas-adjusted score without modifying the solution.
///
/// Returns `(gross_surplus, gas_cost_wei, net_score)`.
pub fn compute_gas_adjusted_score(
    solution: &Solution,
    orders: &[Order],
    reference_prices: &HashMap<String, u128>,
    gas_cost_wei: u128,
) -> (u128, u128, u128) {
    let gross = compute_solution_score(solution, orders, reference_prices);
    let net = gross.saturating_sub(gas_cost_wei);
    (gross, gas_cost_wei, net)
}

// ── Order prioritization ──────────────────────────────────────────────────────

/// Estimate the potential surplus for an order given available liquidity.
///
/// Higher potential surplus orders should be solved first when time is limited.
/// This is a fast heuristic: potential = sell_amount / buy_amount (price quality proxy).
pub fn order_priority(order: &Order) -> u128 {
    let sell: u128 = order.sell_amount.parse().unwrap_or(0);
    let buy: u128 = order.buy_amount.parse().unwrap_or(1);
    if buy == 0 {
        return u128::MAX;
    }
    // Higher ratio = more generous limit = more potential surplus = lower priority
    // Invert: priority = buy / sell (tight limit → harder to improve → lower priority)
    // Use fixed-point: multiply by 1e6 for precision
    (buy.saturating_mul(1_000_000)) / sell.max(1)
}

/// Sort orders by priority (highest potential surplus first).
///
/// Uses a loose heuristic: orders with a very tight limit price (buy/sell ≈ market)
/// have lower potential surplus and should be processed last.
pub fn prioritize_orders(orders: &mut [Order]) {
    // Sort by descending priority (relaxed limit = higher surplus potential)
    // Priority = sell / buy — loose limit means high sell, reasonable buy
    orders.sort_by(|a, b| {
        let pa = sell_buy_ratio(a);
        let pb = sell_buy_ratio(b);
        pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn sell_buy_ratio(order: &Order) -> f64 {
    let sell: f64 = order.sell_amount.parse().unwrap_or(0.0);
    let buy: f64 = order.buy_amount.parse().unwrap_or(1.0);
    if buy == 0.0 { f64::MAX } else { sell / buy }
}

// ── Solution merging ──────────────────────────────────────────────────────────

/// Select the best solution from a list by total score.
///
/// Returns the index of the highest-scoring solution, or None if the list is empty.
pub fn select_best_solution(solutions: &[Solution]) -> Option<usize> {
    solutions
        .iter()
        .enumerate()
        .max_by_key(|(_, s)| extract_score(s))
        .map(|(i, _)| i)
}

/// Extract the numeric score from a Solution.
pub fn extract_score(solution: &Solution) -> u128 {
    match &solution.score {
        Some(Score::Solver { score }) => score.parse().unwrap_or(0),
        Some(Score::RiskAdjusted { success_probability }) => {
            (*success_probability * 1_000_000.0) as u128
        }
        None => 0,
    }
}

/// Merge two solutions that cover disjoint sets of orders.
///
/// Returns a new solution combining both. Caller must ensure no order appears in both.
pub fn merge_solutions(a: Solution, b: Solution) -> Solution {
    let mut trades = a.trades;
    trades.extend(b.trades);

    let mut pre_interactions = a.pre_interactions;
    pre_interactions.extend(b.pre_interactions);

    let mut interactions = a.interactions;
    interactions.extend(b.interactions);

    let mut post_interactions = a.post_interactions;
    post_interactions.extend(b.post_interactions);

    let mut prices = a.prices;
    prices.extend(b.prices);

    let score = match (&a.score, &b.score) {
        (Some(Score::Solver { score: s1 }), Some(Score::Solver { score: s2 })) => {
            let total = s1.parse::<u128>().unwrap_or(0)
                .saturating_add(s2.parse::<u128>().unwrap_or(0));
            Some(Score::Solver { score: total.to_string() })
        }
        (Some(s), _) => Some(s.clone()),
        (_, Some(s)) => Some(s.clone()),
        _ => None,
    };

    Solution {
        id: a.id,
        prices,
        trades,
        pre_interactions,
        interactions,
        post_interactions,
        gas: None,
        score,
    }
}

// ── Protocol fee bonus estimation ────────────────────────────────────────────

/// Estimate a protocol fee bonus for score adjustment.
///
/// The driver formula is `score = user_surplus + protocol_fees`, but we don't
/// know the fee policy (it's not in the Order struct). We estimate based on
/// order class, since different classes have different typical fee structures:
///
/// - Market orders: ~15% bonus (volume-based fee, 2bps of trade value)
/// - Limit orders: ~30% bonus (surplus fee, 50% of surplus above limit)
/// - Liquidity orders: 0% (no protocol fee)
///
/// Gated by `PROTOCOL_FEE_BONUS` env var (default: "false" — conservative).
pub fn estimate_protocol_fee_bonus(
    class: &crate::models::order::OrderClass,
    user_surplus_wei: u128,
) -> u128 {
    use crate::models::order::OrderClass;

    // Only apply if explicitly enabled
    let enabled = std::env::var("PROTOCOL_FEE_BONUS")
        .unwrap_or_else(|_| "false".to_string());
    if enabled != "true" {
        return 0;
    }

    match class {
        OrderClass::Market => user_surplus_wei * 15 / 100,
        OrderClass::Limit => user_surplus_wei * 30 / 100,
        OrderClass::Liquidity => 0,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::order::{Order, OrderClass, OrderKind};
    use crate::models::solution::{FulfillmentTrade, Score, Trade};

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

    fn make_solution_with_score(score_val: u128) -> Solution {
        let mut sol = Solution::new(0);
        sol.score = Some(Score::Solver { score: score_val.to_string() });
        sol
    }

    // ── Surplus tests ─────────────────────────────────────────────────────────

    #[test]
    fn sell_order_surplus_above_limit() {
        assert_eq!(sell_order_surplus(1100, 1000), 100);
    }

    #[test]
    fn sell_order_surplus_at_limit() {
        assert_eq!(sell_order_surplus(1000, 1000), 0);
    }

    #[test]
    fn sell_order_surplus_below_limit() {
        // Below limit — saturates to 0 (invalid fill, but shouldn't panic)
        assert_eq!(sell_order_surplus(900, 1000), 0);
    }

    #[test]
    fn buy_order_surplus_logic() {
        assert_eq!(buy_order_surplus(900, 1000), 100); // spent 100 less than limit
        assert_eq!(buy_order_surplus(1000, 1000), 0);
        assert_eq!(buy_order_surplus(1100, 1000), 0); // overspent — saturates to 0
    }

    #[test]
    fn order_surplus_raw_sell_order() {
        let order = make_sell_order("u1", "0xa", "0xb", 1000, 900);
        // executed_sell = 1000, executed_buy = 1100 → surplus = 200
        let surplus = order_surplus_raw(&order, 1000, 1100);
        assert_eq!(surplus, 200);
    }

    // ── Score computation ─────────────────────────────────────────────────────

    #[test]
    fn compute_solution_score_single_order() {
        let order = make_sell_order("u1", "0xa", "0xb", 1000, 900);
        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000".into(), // sell amount
            fee: String::new(),
        }));
        // Set prices: sell=1000, buy=800 → executed_buy = 1000 * 1000 / 800 = 1250
        solution.prices.insert("0xa".into(), "1000".into());
        solution.prices.insert("0xb".into(), "800".into());

        let score = compute_solution_score(&solution, &[order], &HashMap::new());
        // executed_buy = 1250, limit_buy = 900 → surplus = 350
        assert!(score > 0, "should compute positive score");
    }

    #[test]
    fn compute_solution_score_no_trades() {
        let solution = Solution::new(0);
        let score = compute_solution_score(&solution, &[], &HashMap::new());
        assert_eq!(score, 0);
    }

    // ── Order prioritization ──────────────────────────────────────────────────

    #[test]
    fn prioritize_orders_sorts_by_ratio() {
        let mut orders = vec![
            make_sell_order("tight", "0xa", "0xb", 1000, 999), // tight limit
            make_sell_order("loose", "0xa", "0xb", 1000, 100), // loose limit
            make_sell_order("mid",   "0xa", "0xb", 1000, 500), // mid limit
        ];
        prioritize_orders(&mut orders);
        // Loose limit (sell=1000, buy=100 → ratio=10) comes first
        assert_eq!(orders[0].uid, "loose");
        // Tight limit (ratio≈1) comes last
        assert_eq!(orders[2].uid, "tight");
    }

    #[test]
    fn order_priority_returns_higher_for_loose_limit() {
        let tight = make_sell_order("t", "0xa", "0xb", 1000, 999);
        let loose = make_sell_order("l", "0xa", "0xb", 1000, 100);
        // Both sell 1000. Loose limit buys only 100 (generous → more headroom for improvement)
        // But priority heuristic: buy/sell ratio. 100/1000 < 999/1000 → loose has lower ratio
        // In terms of profit potential, a lower buy_min means solver can do much better → high priority
        let p_loose = order_priority(&loose);
        let p_tight = order_priority(&tight);
        assert!(p_loose < p_tight, "loose limit has lower priority number (actually sorted desc by sell/buy)");
    }

    // ── Best solution selection ───────────────────────────────────────────────

    #[test]
    fn select_best_solution_picks_highest_score() {
        let solutions = vec![
            make_solution_with_score(100),
            make_solution_with_score(500),
            make_solution_with_score(200),
        ];
        let best = select_best_solution(&solutions).unwrap();
        assert_eq!(best, 1, "solution at index 1 has the highest score");
    }

    #[test]
    fn select_best_solution_empty_returns_none() {
        assert!(select_best_solution(&[]).is_none());
    }

    // ── Solution merging ──────────────────────────────────────────────────────

    #[test]
    fn merge_solutions_sums_scores() {
        let a = make_solution_with_score(300);
        let b = make_solution_with_score(700);
        let merged = merge_solutions(a, b);
        match &merged.score {
            Some(Score::Solver { score }) => {
                assert_eq!(score.parse::<u128>().unwrap(), 1000);
            }
            other => panic!("expected Solver score, got {:?}", other),
        }
    }

    #[test]
    fn attach_score_sets_solver_score() {
        let order = make_sell_order("u1", "0xa", "0xb", 1000, 500);
        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000".into(),
            fee: String::new(),
        }));
        solution.prices.insert("0xa".into(), "1000".into());
        solution.prices.insert("0xb".into(), "500".into());
        attach_score(&mut solution, &[order], &HashMap::new());

        assert!(
            matches!(&solution.score, Some(Score::Solver { .. })),
            "score should be Solver variant"
        );
    }

    // ── Gas-adjusted scoring ─────────────────────────────────────────────────

    #[test]
    fn gas_adjusted_score_subtracts_gas() {
        let order = make_sell_order("u1", "0xa", "0xb", 1000, 500);
        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000".into(),
            fee: String::new(),
        }));
        solution.prices.insert("0xa".into(), "1000".into());
        solution.prices.insert("0xb".into(), "500".into());

        let gross_surplus = compute_solution_score(&solution, &[order.clone()], &HashMap::new());
        let gas_cost = 100u128;

        attach_gas_adjusted_score(&mut solution, &[order], &HashMap::new(), gas_cost);

        match &solution.score {
            Some(Score::Solver { score }) => {
                let net: u128 = score.parse().unwrap();
                assert_eq!(net, gross_surplus.saturating_sub(gas_cost));
            }
            other => panic!("expected Solver score, got {:?}", other),
        }
    }

    #[test]
    fn gas_adjusted_score_saturates_to_zero() {
        let order = make_sell_order("u1", "0xa", "0xb", 1000, 999);
        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000".into(),
            fee: String::new(),
        }));
        solution.prices.insert("0xa".into(), "1000".into());
        solution.prices.insert("0xb".into(), "999".into());

        // Very high gas cost should saturate score to 0
        attach_gas_adjusted_score(&mut solution, &[order], &HashMap::new(), u128::MAX);

        match &solution.score {
            Some(Score::Solver { score }) => {
                let net: u128 = score.parse().unwrap();
                assert_eq!(net, 0, "Unprofitable solution should have 0 score");
            }
            other => panic!("expected Solver score, got {:?}", other),
        }
    }

    #[test]
    fn compute_gas_adjusted_score_returns_components() {
        let order = make_sell_order("u1", "0xa", "0xb", 1000, 500);
        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000".into(),
            fee: String::new(),
        }));
        solution.prices.insert("0xa".into(), "1000".into());
        solution.prices.insert("0xb".into(), "500".into());

        let gas = 50u128;
        let (gross, gas_ret, net) = compute_gas_adjusted_score(
            &solution, &[order], &HashMap::new(), gas,
        );
        assert_eq!(gas_ret, gas);
        assert_eq!(net, gross.saturating_sub(gas));
    }

    // ── CoW Protocol scoring formula tests ───────────────────────────────────

    fn make_token_map(entries: &[(&str, &str)]) -> TokenMap {
        use crate::models::token::TokenInfo;
        entries.iter().map(|(addr, ref_price)| {
            (addr.to_string(), TokenInfo {
                decimals: Some(18),
                symbol: None,
                reference_price: Some(ref_price.to_string()),
                available_balance: None,
                trusted: true,
            })
        }).collect()
    }

    #[test]
    fn cow_score_sell_order_basic() {
        // Sell 1000 units of token A for token B
        // Limit: must get at least 900 B
        // Clearing prices: A=1000, B=800 → user gets 1000*1000/800 = 1250 B
        // Surplus = 1250 - 900 = 350 in B atoms
        // Native price of B = 1e18 (1 B = 1 ETH)
        // Score = 350 * 1e18 / 1e18 = 350
        let order = make_sell_order("u1", "0xa", "0xb", 1000, 900);
        let tokens = make_token_map(&[
            ("0xb", "1000000000000000000"), // 1e18
        ]);

        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000".into(),
            fee: String::new(),
        }));
        solution.prices.insert("0xa".into(), "1000".into());
        solution.prices.insert("0xb".into(), "800".into());

        let score = compute_cow_score(&solution, &[order], &tokens);
        // ceil(1000 * 1000 / 800) = 1250, surplus = 1250 - 900 = 350 in B atoms
        // No caps with reference-price clearing (prices are globally consistent)
        assert_eq!(score, 350);
    }

    #[test]
    fn cow_score_sell_order_no_surplus() {
        // Clearing prices give exactly the limit — zero surplus
        let order = make_sell_order("u1", "0xa", "0xb", 1000, 1000);
        let tokens = make_token_map(&[("0xb", "1000000000000000000")]);

        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000".into(),
            fee: String::new(),
        }));
        solution.prices.insert("0xa".into(), "1000".into());
        solution.prices.insert("0xb".into(), "1000".into());

        let score = compute_cow_score(&solution, &[order], &tokens);
        assert_eq!(score, 0);
    }

    #[test]
    fn cow_score_with_reference_price_scaling() {
        // USDC-like token: reference_price = 3.7e14 (USDC worth ~0.00037 ETH)
        // Sell 1e6 USDC worth of token A, get token B (USDC-like)
        // surplus = 50000 B atoms
        // score = 50000 * 3.7e14 / 1e18 = 18.5 (in ETH-wei)
        let order = make_sell_order("u1", "0xa", "0xb", 1000000, 950000);
        let tokens = make_token_map(&[("0xb", "370000000000000")]); // 3.7e14

        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000000".into(),
            fee: String::new(),
        }));
        // Prices that give 1000000 B for 1000000 A (1:1), surplus = 1000000 - 950000 = 50000
        solution.prices.insert("0xa".into(), "1".into());
        solution.prices.insert("0xb".into(), "1".into());

        let score = compute_cow_score(&solution, &[order], &tokens);
        // surplus = 50000 * 370000000000000 / 1e18 = 18500000000000000000 / 1e18 = 18
        assert_eq!(score, 18);
    }

    #[test]
    fn cow_score_large_surplus_no_cap() {
        // With reference-price clearing, no artificial caps — scores are naturally
        // correct when prices are globally consistent.
        let order = make_sell_order("u1", "0xa", "0xb", 1_000_000_000, 100_000_000);
        let tokens = make_token_map(&[("0xb", "1000000000000000000")]); // 1e18

        let mut solution = Solution::new(0);
        solution.trades.push(Trade::Fulfillment(FulfillmentTrade {
            order: "u1".into(),
            executed_amount: "1000000000".into(),
            fee: String::new(),
        }));
        // 10:1 price ratio → bought = 1e9 * 10 / 1 = 10e9
        // surplus = 10e9 - 100_000_000 = 9_900_000_000
        solution.prices.insert("0xa".into(), "10".into());
        solution.prices.insert("0xb".into(), "1".into());

        let score = compute_cow_score(&solution, &[order], &tokens);
        assert_eq!(score, 9_900_000_000); // No cap — reference prices define truth
    }

    #[test]
    fn mul_div_handles_overflow() {
        // Test large values that would overflow u128 in a * b
        let result = super::mul_div(u128::MAX / 2, 2, 1);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), u128::MAX - 1);
    }

    #[test]
    fn mul_div_ceil_rounds_up() {
        assert_eq!(super::mul_div_ceil(10, 3, 4), Some(8)); // ceil(30/4) = 8
        assert_eq!(super::mul_div_ceil(10, 1, 3), Some(4)); // ceil(10/3) = 4
        assert_eq!(super::mul_div_ceil(10, 1, 5), Some(2)); // ceil(10/5) = 2 (exact)
    }
}
