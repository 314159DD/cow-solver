//! Aggregator-powered solver: queries external DEX aggregators (Odos, 1inch,
//! ParaSwap, Bebop) for best execution, then formats the response as a CoW solution.
//!
//! This is the v2 architecture: we don't compute our own AMM math. External
//! aggregators handle all routing, pool math, split routing, and RFQ quotes.
//! We just format their output as a CoW Protocol solution.
//!
//! Pattern is identical to how OKX and BitGet solvers work in cowprotocol/services.

use std::collections::HashMap;
use std::time::Instant;

use tracing::{debug, info, warn};

use crate::liquidity::aggregator::{self, Aggregator, AggregatorQuote};
use crate::models::auction::AuctionInstance;
use crate::models::order::{Order, OrderKind};
use crate::models::solution::{
    CustomInteraction, Interaction, Score, Solution, SolveResponse, Trade,
};

/// Maximum orders to quote per auction (rate limit budget).
const DEFAULT_MAX_ORDERS: usize = 5;

/// Minimum sell value in ETH-equivalent wei to bother quoting.
/// Roughly $5 at $2000/ETH.
const MIN_SELL_VALUE_WEI: u128 = 2_500_000_000_000_000;

/// Solve an auction using external aggregator quotes.
///
/// For each selected order:
/// 1. Query all configured aggregators in parallel
/// 2. Pick the best quote (highest buy_amount)
/// 3. Build a CoW solution with execution-rate clearing prices
///
/// Returns None if no profitable quotes are found.
pub async fn solve(auction: &AuctionInstance) -> Option<Solution> {
    // Kill switch
    if std::env::var("AGG_ENABLED").unwrap_or_else(|_| "true".into()) == "false" {
        debug!("Aggregator solver disabled via AGG_ENABLED=false");
        return None;
    }

    let aggregators = aggregator::build_from_env();
    if aggregators.is_empty() {
        debug!("No aggregators configured — skipping aggregator solver");
        return None;
    }

    let chain_id = auction.chain_id.unwrap_or(42161);
    let max_orders: usize = std::env::var("AGG_MAX_ORDERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_ORDERS);

    let t0 = Instant::now();

    // Select top orders by value (sell_amount × reference_price)
    let mut valued_orders: Vec<(&Order, u128)> = auction.orders.iter()
        .filter_map(|o| {
            let sell_amount: u128 = o.sell_amount.parse().ok()?;
            if sell_amount == 0 { return None; }

            // Estimate ETH value using reference price
            let ref_price: u128 = auction.tokens.get(&o.sell_token)
                .or_else(|| auction.tokens.iter()
                    .find(|(k, _)| k.to_lowercase() == o.sell_token.to_lowercase())
                    .map(|(_, v)| v))
                .and_then(|t| t.reference_price.as_ref())
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);

            let value = crate::solver::scoring::mul_div(sell_amount, ref_price, 1_000_000_000_000_000_000)
                .unwrap_or(0);

            if value < MIN_SELL_VALUE_WEI {
                return None; // Too small to bother
            }

            Some((o, value))
        })
        .collect();

    valued_orders.sort_by(|a, b| b.1.cmp(&a.1));
    valued_orders.truncate(max_orders);

    if valued_orders.is_empty() {
        debug!(auction_id = auction.id, "No orders above minimum value for aggregator");
        return None;
    }

    info!(
        auction_id = auction.id,
        orders_selected = valued_orders.len(),
        aggregators = aggregators.len(),
        agg_names = ?aggregators.iter().map(|a| a.name()).collect::<Vec<_>>(),
        "Aggregator solver: quoting top orders"
    );

    // Quote each order through all aggregators
    let mut trades = Vec::new();
    let mut interactions = Vec::new();
    let mut all_prices: HashMap<String, String> = HashMap::new();
    let mut total_surplus_wei: u128 = 0;

    for (order, _value) in &valued_orders {
        // Check time budget
        if t0.elapsed().as_secs() > 20 {
            debug!(auction_id = auction.id, "Aggregator solver: time budget exceeded");
            break;
        }

        let sell_amount: u128 = order.sell_amount.parse().unwrap_or(0);
        let buy_amount_limit: u128 = order.buy_amount.parse().unwrap_or(0);
        if sell_amount == 0 || buy_amount_limit == 0 { continue; }

        // Query all aggregators for this order
        let best = aggregator::best_quote(
            &aggregators,
            &order.sell_token,
            &order.buy_token,
            &sell_amount.to_string(),
            chain_id,
        ).await;

        let (agg_name, quote) = match best {
            Some(q) => q,
            None => {
                debug!(order_uid = %order.uid.chars().take(20).collect::<String>(), "No aggregator quote");
                continue;
            }
        };

        let buy_amount_quoted: u128 = quote.buy_amount_u128();

        // Check: does the aggregator quote meet the order's limit?
        if buy_amount_quoted < buy_amount_limit {
            debug!(
                order_uid = %order.uid.chars().take(20).collect::<String>(),
                quoted = buy_amount_quoted,
                limit = buy_amount_limit,
                aggregator = %agg_name,
                "Aggregator quote below limit — skipping"
            );
            continue;
        }

        let surplus = buy_amount_quoted.saturating_sub(buy_amount_limit);

        // Normalize surplus to ETH for logging
        let np: u128 = auction.tokens.iter()
            .find(|(k, _)| k.to_lowercase() == order.buy_token.to_lowercase())
            .and_then(|(_, t)| t.reference_price.as_ref())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let surplus_wei = crate::solver::scoring::mul_div(surplus, np, 1_000_000_000_000_000_000)
            .unwrap_or(0);

        info!(
            auction_id = auction.id,
            order_uid = %order.uid.chars().take(20).collect::<String>(),
            aggregator = %agg_name,
            sell_amount,
            buy_quoted = buy_amount_quoted,
            buy_limit = buy_amount_limit,
            surplus,
            surplus_gwei = surplus_wei / 1_000_000_000,
            gas = quote.gas_estimate,
            "Aggregator quote accepted"
        );

        // Build clearing prices from execution amounts.
        // prices[sell_token] = buy_amount (what you get for selling)
        // prices[buy_token] = sell_amount (what you pay to buy)
        // Only ratio matters — these encode the actual execution rate.
        let sell_lc = order.sell_token.to_lowercase();
        let buy_lc = order.buy_token.to_lowercase();

        // Handle shared tokens: if token already has a price, normalize
        if let Some(existing) = all_prices.get(&sell_lc) {
            let existing_val: u128 = existing.parse().unwrap_or(0);
            if existing_val > 0 && buy_amount_quoted != existing_val {
                // Scale buy_token price to maintain consistency
                let scaled = crate::solver::scoring::mul_div(
                    sell_amount, existing_val, buy_amount_quoted
                ).unwrap_or(sell_amount);
                all_prices.insert(buy_lc.clone(), scaled.to_string());
            }
        } else if let Some(existing) = all_prices.get(&buy_lc) {
            let existing_val: u128 = existing.parse().unwrap_or(0);
            if existing_val > 0 && sell_amount != existing_val {
                let scaled = crate::solver::scoring::mul_div(
                    buy_amount_quoted, existing_val, sell_amount
                ).unwrap_or(buy_amount_quoted);
                all_prices.insert(sell_lc.clone(), scaled.to_string());
            }
        } else {
            all_prices.insert(sell_lc.clone(), buy_amount_quoted.to_string());
            all_prices.insert(buy_lc.clone(), sell_amount.to_string());
        }

        // Build trade
        let executed_amount = match order.kind {
            OrderKind::Sell => sell_amount.to_string(),
            OrderKind::Buy => order.buy_amount.clone(),
        };
        trades.push(Trade::fulfillment(&order.uid, executed_amount));

        // Build interaction from aggregator calldata
        if quote.calldata.len() > 2 { // "0x" = empty
            interactions.push(Interaction::Custom(CustomInteraction {
                internalize: false,
                target: quote.to.clone(),
                call_data: quote.calldata.clone(),
                value: quote.value.clone(),
            }));
        }

        total_surplus_wei += surplus_wei;
    }

    if trades.is_empty() {
        info!(
            auction_id = auction.id,
            elapsed_ms = t0.elapsed().as_millis(),
            "Aggregator solver: no viable trades"
        );
        return None;
    }

    // Score placeholder — rescore_solutions will compute the real score
    // from clearing prices using the driver's exact formula.
    let score = Some(Score::Solver {
        score: "0".to_string(),
    });

    info!(
        auction_id = auction.id,
        trades = trades.len(),
        interactions = interactions.len(),
        total_surplus_gwei = total_surplus_wei / 1_000_000_000,
        elapsed_ms = t0.elapsed().as_millis(),
        "Aggregator solver: solution assembled"
    );

    Some(Solution {
        id: 0,
        prices: all_prices,
        trades,
        pre_interactions: vec![],
        interactions,
        post_interactions: vec![],
        gas: None,
        score,
    })
}
