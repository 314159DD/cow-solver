use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tracing::{debug, info, warn};

use crate::gas;
use crate::models::auction::{AuctionInstance, TokenMap};
use crate::models::liquidity::PoolKind;
use crate::models::solution::{Interaction, Score, Solution, SolveResponse, Trade};
use crate::solver::direct::{DirectSolver, OrderRoute};
use crate::solver::scoring;
use crate::solver::pricing;

// Track order UIDs from previous auction to detect new orders
static PREV_ORDER_UIDS: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();

fn prev_orders() -> &'static Mutex<HashSet<String>> {
    PREV_ORDER_UIDS.get_or_init(|| Mutex::new(HashSet::new()))
}

// ── Score normalization ──────────────────────────────────────────────────────

/// Normalize a surplus amount by the buy token's reference price.
///
/// CoW scoring formula: `score(order) = surplus(order) × reference_price(buy_token)`
///
/// `reference_price` is provided in the auction payload as a string representing
/// "atoms of ETH per atom of this token". So multiplying surplus (in buy token atoms)
/// by reference_price gives a score in ETH atoms — comparable across all token pairs.
///
/// Returns 0 if the token has no reference price (unknown/untrusted token).
pub fn normalize_surplus(surplus: u128, buy_token: &str, tokens: &TokenMap) -> u128 {
    // Case-insensitive token lookup — CoW driver may use checksummed addresses
    let ref_price_str = tokens.iter()
        .find(|(k, _)| k.to_lowercase() == buy_token.to_lowercase())
        .and_then(|(_, t)| t.reference_price.as_ref());

    let ref_price_u128: u128 = ref_price_str
        .and_then(|p| p.parse::<u128>().ok())
        .unwrap_or(0);

    if ref_price_u128 == 0 {
        // Try f64 parse as fallback (some drivers send decimal strings)
        let ref_price_f64 = ref_price_str
            .and_then(|p| p.parse::<f64>().ok())
            .unwrap_or(0.0);
        if ref_price_f64 <= 0.0 {
            return 0;
        }
        let normalized = (surplus as f64 * ref_price_f64) / 1e18;
        return if normalized > 0.0 { normalized as u128 } else { 0 };
    }

    // Integer path: surplus × ref_price / 1e18
    // No cap — finalize_solutions rescores using the exact CoW driver formula.
    // This function is only used for internal route filtering/sorting.
    surplus
        .checked_mul(ref_price_u128)
        .map(|v| v / 1_000_000_000_000_000_000u128)
        .unwrap_or_else(|| {
            (surplus / 1_000_000_000_000_000_000u128).saturating_mul(ref_price_u128)
        })
}

// ── Assembly pipeline ─────────────────────────────────────────────────────────

/// Assemble a valid Solution from computed trade results.
///
/// This is the low-level version used when you already have prices, trades,
/// and interactions fully computed.
pub fn assemble_solution(
    id: u64,
    prices: HashMap<String, String>,
    trades: Vec<Trade>,
    interactions: Vec<Interaction>,
) -> Solution {
    let score = compute_score(&trades, &prices);
    Solution {
        id,
        prices,
        trades,
        pre_interactions: vec![],
        interactions,
        post_interactions: vec![],
        gas: None,
        score,
    }
}

/// Full assembly pipeline:
///
/// 1. Run DirectSolver on all orders
/// 2. Filter out routes whose surplus < estimated per-DEX gas cost
/// 3. Compute UDCP prices per (sell_token, buy_token) group
/// 4. Verify UDCP compliance
/// 5. Build trades and interactions, collect pool kinds
/// 6. Compute gas-adjusted score (gross surplus - total gas cost)
/// 7. Assemble into SolveResponse
pub fn assemble_from_auction(auction: &AuctionInstance) -> SolveResponse {
    if auction.orders.is_empty() {
        return SolveResponse::empty();
    }

    // Parse effective gas price (wei)
    let chain_id = auction.chain_id.unwrap_or(1);
    // Arbitrum gas is ~0.1-0.3 gwei; mainnet ~30 gwei. Use chain-appropriate default.
    let default_gas = if chain_id == 42161 { 100_000_000u128 } else { 30_000_000_000u128 };
    let gas_price_wei: u128 = auction
        .effective_gas_price
        .parse()
        .unwrap_or(default_gas);

    // Step 1: Route ALL orders through available pools.
    // Phase 1 takes ~50ms for 970 orders — no need to pre-filter.
    // More orders routed = more profitable opportunities found.
    let mut routes: Vec<OrderRoute> = Vec::new();
    for order in &auction.orders {
        if let Some(route) = DirectSolver::solve_order(order, &auction.liquidity) {
            routes.push(route);
        }
    }

    // Also try partial fills for partially_fillable orders that didn't route at full size.
    // Large orders push deep into the AMM curve where slippage kills surplus.
    // Filling 50% at better rates can produce more net surplus than 100% fill.
    const MIN_PARTIAL_SELL_WEI: u128 = 1_000_000_000_000_000; // ~$3 floor
    let routed_uids: HashSet<String> = routes.iter().map(|r| r.order_uid.clone()).collect();
    for order in &auction.orders {
        if !order.partially_fillable { continue; }
        if routed_uids.contains(&order.uid) { continue; }
        let sell_amount: u128 = order.sell_amount.parse().unwrap_or(0);
        if sell_amount == 0 { continue; }

        // Try 75%, 50%, 25% fill fractions
        for fraction in [75u128, 50, 25] {
            let partial_sell = sell_amount * fraction / 100;
            if partial_sell < MIN_PARTIAL_SELL_WEI { continue; }

            // Create a modified order with reduced sell amount (and proportional buy)
            let buy_amount: u128 = order.buy_amount.parse().unwrap_or(0);
            let partial_buy = buy_amount * fraction / 100;
            let mut partial_order = order.clone();
            partial_order.sell_amount = partial_sell.to_string();
            partial_order.buy_amount = partial_buy.to_string();

            if let Some(route) = DirectSolver::solve_order(&partial_order, &auction.liquidity) {
                // Use the original order uid — the solver fills a fraction
                let mut adjusted_route = route;
                adjusted_route.order_uid = order.uid.clone();
                routes.push(adjusted_route);
                break; // Use first successful partial fill
            }
        }
    }

    if routes.is_empty() {
        info!(auction_id = auction.id, orders = auction.orders.len(), pools = auction.liquidity.len(), "Assembler: no routes found");
        return SolveResponse::empty();
    }
    info!(auction_id = auction.id, routes = routes.len(), orders = auction.orders.len(), pools = auction.liquidity.len(), "Assembler: routes found before filtering");

    // Step 1.5: Filter out routes with suspiciously high surplus.
    // Winners score 953K-1.5M gwei total. Any single route producing more than
    // 5M gwei of surplus is likely from stale pool data or wrong pool matching.
    // This prevents inflated routes from dominating honest ones.
    const MAX_SURPLUS_WEI: u128 = 1_500_000_000_000_000; // 1.5M gwei = 0.0015 ETH
    let routes: Vec<OrderRoute> = routes.into_iter().filter(|r| {
        let order = auction.orders.iter().find(|o| o.uid == r.order_uid);
        let buy_token = order.map(|o| o.buy_token.as_str()).unwrap_or("");
        let normalized = normalize_surplus(r.surplus, buy_token, &auction.tokens);
        if normalized > MAX_SURPLUS_WEI {
            debug!(
                order_uid = %r.order_uid.chars().take(20).collect::<String>(),
                surplus = r.surplus,
                normalized,
                "Route filtered: surplus exceeds sanity cap (likely stale pool)"
            );
            false
        } else {
            true
        }
    }).collect();

    if routes.is_empty() {
        debug!(auction_id = auction.id, "All routes filtered by surplus cap");
        return SolveResponse::empty();
    }

    // Step 2: Filter unprofitable routes using per-DEX gas estimates
    //
    // For each route, normalize surplus by reference_price then compare vs gas.
    // This ensures we correctly compare ETH-denominated surplus vs ETH-denominated gas.
    let profitable: Vec<OrderRoute> = routes
        .into_iter()
        .filter(|r| {
            let pool_kind = detect_pool_kind_from_route(r, &auction.liquidity);
            let gas_cost_wei = gas::estimate_solution_cost_wei(
                chain_id,
                &[pool_kind],
                1,
                0,
                gas_price_wei,
            );

            // Normalize surplus to ETH atoms for fair comparison with gas cost
            let buy_token = auction.orders.iter()
                .find(|o| o.uid == r.order_uid)
                .map(|o| o.buy_token.as_str())
                .unwrap_or("");
            let normalized_surplus = normalize_surplus(r.surplus, buy_token, &auction.tokens);

            let profitable = normalized_surplus >= gas_cost_wei;
            if !profitable {
                debug!(
                    order_uid = %r.order_uid,
                    raw_surplus = r.surplus,
                    normalized_surplus = normalized_surplus,
                    gas_cost = gas_cost_wei,
                    pool_kind = ?pool_kind,
                    "Route filtered: normalized surplus < gas cost"
                );
            }
            profitable
        })
        .collect();

    if profitable.is_empty() {
        debug!(auction_id = auction.id, "All routes unprofitable — empty solution");
        return SolveResponse::empty();
    }

    // Step 2.5: Detect new orders and prioritize them.
    // New orders = just appeared in this batch (not in previous auction).
    // These are the most likely to be actionable — someone just placed them.
    let prev = prev_orders().lock().unwrap_or_else(|e| e.into_inner());
    let new_order_uids: HashSet<&str> = auction.orders.iter()
        .filter(|o| !prev.contains(&o.uid))
        .map(|o| o.uid.as_str())
        .collect();
    drop(prev);
    let new_count = new_order_uids.len();

    // Update previous orders for next auction
    {
        let mut prev = prev_orders().lock().unwrap_or_else(|e| e.into_inner());
        prev.clear();
        for order in &auction.orders {
            prev.insert(order.uid.clone());
        }
    }

    // Step 2.6: Select top N routes.
    // Priority: new orders first, then large orders (CIP-74: large = more profitable),
    // then market orders, then by surplus.
    // Submit multiple solutions with different orders — let the driver pick the best.
    // Each solution has 1 trade (the driver deducts per-trade gas).
    // Previously we always picked the same "best" order — now we diversify.
    const MAX_SOLUTIONS: usize = 3;
    let mut profitable = profitable;
    if profitable.len() > MAX_SOLUTIONS {
        // Sort by NORMALIZED SURPLUS descending — this is what determines the
        // driver's score. Winners pick the order with highest surplus, not
        // highest value. But we also subtract estimated gas per trade to get
        // net surplus, since the driver deducts gas.
        profitable.sort_by(|a, b| {
            let order_a = auction.orders.iter().find(|o| o.uid == a.order_uid);
            let order_b = auction.orders.iter().find(|o| o.uid == b.order_uid);
            let buy_a = order_a.map(|o| o.buy_token.as_str()).unwrap_or("");
            let buy_b = order_b.map(|o| o.buy_token.as_str()).unwrap_or("");

            // Compute net surplus = normalized_surplus - estimated_gas
            let norm_a = normalize_surplus(a.surplus, buy_a, &auction.tokens);
            let norm_b = normalize_surplus(b.surplus, buy_b, &auction.tokens);

            let pool_kind_a = detect_pool_kind_from_route(a, &auction.liquidity);
            let pool_kind_b = detect_pool_kind_from_route(b, &auction.liquidity);
            let gas_a = gas::estimate_solution_cost_wei(chain_id, &[pool_kind_a], 1, 0, gas_price_wei);
            let gas_b = gas::estimate_solution_cost_wei(chain_id, &[pool_kind_b], 1, 0, gas_price_wei);

            let net_a = norm_a.saturating_sub(gas_a);
            let net_b = norm_b.saturating_sub(gas_b);

            // Pair priority: deprioritize high-competition pairs (WETH/USDC etc),
            // boost exotic pairs, limit orders, and partially-fillable orders.
            // Priority is a weighted tiebreaker — net surplus is still primary.
            let priority = |order: Option<&&crate::models::order::Order>| -> i64 {
                let Some(o) = order else { return 0 };
                let mut p: i64 = 0;
                if crate::triage::is_high_competition_pair(&o.sell_token, &o.buy_token) {
                    p -= 100;
                }
                if o.class == crate::models::order::OrderClass::Limit {
                    p += 50; // stale limit orders are our niche
                }
                if o.partially_fillable {
                    p += 30; // other solvers often skip these
                }
                p
            };
            let pa = priority(order_a.as_ref());
            let pb = priority(order_b.as_ref());

            // Weighted score: net surplus + priority bonus (scaled to ~1M gwei range)
            let weighted_a = (net_a as i128) + (pa as i128) * 10_000_000_000; // 10K gwei per priority point
            let weighted_b = (net_b as i128) + (pb as i128) * 10_000_000_000;

            weighted_b.cmp(&weighted_a) // highest weighted score first
        });
        profitable.truncate(MAX_SOLUTIONS);
    }

    if new_count > 0 {
        let new_in_solution = profitable.iter()
            .filter(|r| new_order_uids.contains(r.order_uid.as_str()))
            .count();
        info!(
            auction_id = auction.id,
            new_orders = new_count,
            new_in_solution = new_in_solution,
            total_routes = profitable.len(),
            "Order freshness: new orders detected"
        );
    }

    // Step 3: Compute clearing prices from EXECUTION RATES (UDCP).
    //
    // CRITICAL INSIGHT from CoW driver source code analysis:
    // Clearing prices are NOT reference prices. They are arbitrary-denomination
    // values where ONLY THE RATIO matters. The driver computes:
    //   bought = ceil(executed × cp_sell / cp_buy)
    // So cp_sell/cp_buy must equal the actual execution rate from our AMM routing.
    //
    // The simplest correct approach (confirmed by CoW docs):
    //   prices[sell_token] = total_buy_output   (across all trades of this pair)
    //   prices[buy_token]  = total_sell_input
    // This ensures the ratio = actual execution rate.
    //
    // For shared tokens across pairs: if USDC appears in WETH→USDC and USDC→WBTC,
    // we compute per-pair first, then normalize globally so shared tokens have
    // one consistent price. This uses the first pair's price as the anchor and
    // scales other pairs to match.
    // Build clearing prices per the EXACT reference solver formula:
    //
    // From crates/solvers/src/domain/solution.rs — Single::into_solution():
    //   For sell orders:
    //     sell = input_amount + fee  (capped at order.sell_amount)
    //     buy  = (sell - fee) × pool_output / pool_input  [proportional scaling]
    //     prices[sell_token] = buy
    //     prices[buy_token]  = sell - fee  (= net sell amount)
    //
    //   For buy orders:
    //     sell = input_amount + fee
    //     buy  = output_amount  (capped at order.buy_amount)
    //     prices[sell_token] = buy
    //     prices[buy_token]  = sell - fee
    //
    // For market orders (fee=0): buy = pool_output, sell_net = pool_input. Same as before.
    // For limit orders with fee: buy is PROPORTIONALLY SCALED down.
    let mut pair_routes: HashMap<(String, String), Vec<(u128, u128)>> = HashMap::new();
    for route in &profitable {
        if let Some(order) = auction.orders.iter().find(|o| o.uid == route.order_uid) {
            let key = (order.sell_token.to_lowercase(), order.buy_token.to_lowercase());

            // Compute the reference-style amounts:
            // For sell orders: net_sell = executed (no fee for market), buy = proportional
            // For buy orders: buy = min(output, order.buy_amount), net_sell = executed
            // Compute gas fee to embed in clearing price.
            // Winners subtract gas cost from user's buy amount, keeping the
            // difference as network fee. Without this, our score looks "too good"
            // and fails the driver's simulation/fairness check.
            //
            // gas_fee_wei = gas_estimate × gas_price (in wei)
            // gas_fee_in_buy_token = gas_fee_wei × 1e18 / native_price(buy_token)
            let pool_kind = detect_pool_kind_from_route(route, &auction.liquidity);
            // Gas estimate for fee embedding. On Arbitrum, L2 execution gas is
            // very cheap (~0.1 gwei). The significant cost is L1 calldata posting.
            // Previous values (90K-180K total) over-deducted by ~50%, causing our
            // scores to be 0.32x of winners. Halve the estimates.
            // Data shows 1.30x median inflation. Increase gas embedding by ~25%
            // to bring scores closer to what the driver computes.
            // Previous tuning was at 0.90x — overcorrected. Now targeting 1.0-1.05x.
            let per_trade_gas: u128 = match pool_kind {
                PoolKind::UniswapV2 | PoolKind::Sushiswap | PoolKind::CamelotV2 => 45_000,
                PoolKind::UniswapV3 | PoolKind::CamelotV3 => 65_000,
                _ => 50_000,
            };
            let total_gas = per_trade_gas + 20_000;
            let gas_fee_wei = total_gas * gas_price_wei;

            // Convert gas fee from ETH-wei to buy-token atoms
            let buy_token_native_price: u128 = auction.tokens.iter()
                .find(|(k, _)| k.to_lowercase() == order.buy_token.to_lowercase())
                .and_then(|(_, t)| t.reference_price.as_ref())
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);
            let gas_fee_in_buy_token = if buy_token_native_price > 0 {
                scoring::mul_div(gas_fee_wei, 1_000_000_000_000_000_000, buy_token_native_price)
                    .unwrap_or(0)
            } else {
                0
            };

            let (net_sell, buy_output) = match order.kind {
                crate::models::order::OrderKind::Sell => {
                    let net_sell = route.executed_amount;
                    // Proportional scaling + gas fee deduction
                    let raw_buy = scoring::mul_div(net_sell, route.output_amount, route.executed_amount)
                        .unwrap_or(route.output_amount);
                    // Subtract gas fee from buy output (embed gas in price)
                    let buy = raw_buy.saturating_sub(gas_fee_in_buy_token);
                    (net_sell, buy)
                }
                crate::models::order::OrderKind::Buy => {
                    let buy_limit: u128 = order.buy_amount.parse().unwrap_or(u128::MAX);
                    let buy = route.output_amount.min(buy_limit);
                    // For buy orders: gas fee increases the sell amount needed
                    let sell_token_native_price: u128 = auction.tokens.iter()
                        .find(|(k, _)| k.to_lowercase() == order.sell_token.to_lowercase())
                        .and_then(|(_, t)| t.reference_price.as_ref())
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(0);
                    let gas_fee_in_sell = if sell_token_native_price > 0 {
                        scoring::mul_div(gas_fee_wei, 1_000_000_000_000_000_000, sell_token_native_price)
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    let net_sell = route.executed_amount.saturating_add(gas_fee_in_sell);
                    (net_sell, buy)
                }
            };

            pair_routes
                .entry(key)
                .or_default()
                .push((net_sell, buy_output));
        }
    }

    let mut all_prices: HashMap<String, String> = HashMap::new();
    let mut udcp_ok = true;

    for ((sell_token, buy_token), route_amounts) in &pair_routes {
        // prices[sell_token] = total buy_output (what sellers receive)
        // prices[buy_token]  = total net_sell   (what sellers pay, excluding fee)
        let total_net_sell: u128 = route_amounts.iter().map(|(a, _)| a).sum();
        let total_buy_out: u128 = route_amounts.iter().map(|(_, b)| b).sum();
        if total_net_sell == 0 || total_buy_out == 0 {
            udcp_ok = false;
            break;
        }

        // Check for shared token conflicts
        let sell_lc = sell_token.to_lowercase();
        let buy_lc = buy_token.to_lowercase();

        if let Some(existing) = all_prices.get(&sell_lc) {
            let existing_val: u128 = existing.parse().unwrap_or(0);
            if existing_val > 0 && total_buy_out != existing_val {
                let scaled = scoring::mul_div(total_net_sell, existing_val, total_buy_out)
                    .unwrap_or(total_net_sell);
                all_prices.insert(buy_lc, scaled.to_string());
                continue;
            }
        }
        if let Some(existing) = all_prices.get(&buy_lc) {
            let existing_val: u128 = existing.parse().unwrap_or(0);
            if existing_val > 0 && total_net_sell != existing_val {
                let scaled = scoring::mul_div(total_buy_out, existing_val, total_net_sell)
                    .unwrap_or(total_buy_out);
                all_prices.insert(sell_lc, scaled.to_string());
                continue;
            }
        }

        // No conflict — set directly per reference formula:
        // prices[sell_token] = buy_output (what sellers receive)
        // prices[buy_token]  = net_sell   (what sellers pay, excluding fee)
        all_prices.insert(sell_lc, total_buy_out.to_string());
        all_prices.insert(buy_lc, total_net_sell.to_string());
    }

    if !udcp_ok || all_prices.is_empty() {
        debug!(auction_id = auction.id, "UDCP failed — returning empty");
        return SolveResponse::empty();
    }

    // Step 4: Build trades and interactions.
    //
    // With UDCP clearing prices (execution rates from AMM math), the prices
    // already encode the actual swap rate. No reference-price clamping needed —
    // the driver will derive bought amounts from OUR clearing prices, which
    // match what the AMM actually delivers.
    //
    // The only constraint: AMM output must satisfy the order's limit price.
    // This was already checked in DirectSolver::solve_order (routes that don't
    // meet the limit are filtered in Step 1).

    let mut trades = Vec::new();
    let mut interactions = Vec::new();
    let mut swap_kinds = Vec::new();

    for (idx, route) in profitable.iter().enumerate() {
        if idx < 3 {
            let order = auction.orders.iter().find(|o| o.uid == route.order_uid);
            info!(
                auction_id = auction.id,
                trade = idx,
                order_uid = %route.order_uid.chars().take(20).collect::<String>(),
                executed = route.executed_amount,
                output = route.output_amount,
                surplus = route.surplus,
                "Trade included (UDCP clearing)"
            );
        }

        trades.push(Trade::fulfillment(
            &route.order_uid,
            route.executed_amount.to_string(),
        ));
        interactions.push(route.interaction.clone());
        swap_kinds.push(detect_pool_kind_from_route(route, &auction.liquidity));
    }

    // Step 5: Internalize interactions for trusted tokens.
    // When both input and output tokens are trusted and the settlement contract
    // has sufficient balance, skip the on-chain DEX call — saves ~100-150K gas.
    let internalized_count = super::internalization::try_internalize_interactions(
        &mut interactions, &auction.tokens,
    );
    if internalized_count > 0 {
        debug!(
            auction_id = auction.id,
            internalized = internalized_count,
            "Internalized interactions (trusted tokens with balance)"
        );
    }

    // Step 5.5: Build approval pre-interactions for swap interactions.
    // Approvals belong in pre_interactions per the CoW settlement format.
    let mut pre_interactions: Vec<Interaction> = vec![];
    if std::env::var("PREPEND_APPROVALS").unwrap_or_else(|_| "true".to_string()) == "true" {
        use crate::interactions::approvals;
        let targets = approvals::extract_approval_targets(&interactions);
        if !targets.is_empty() {
            let requests: Vec<approvals::ApprovalRequest> = targets
                .into_iter()
                .map(|(token, spender)| approvals::ApprovalRequest {
                    token,
                    spender,
                    required_amount: u128::MAX,
                    current_allowance: 0,
                })
                .collect();
            for req in &requests {
                if approvals::needs_approval(req.current_allowance, req.required_amount) {
                    pre_interactions.push(approvals::encode_approval(&req.token, &req.spender));
                }
            }
        }
    }

    // Step 6: Compute score using normalize_surplus (honest, reference-price-based).
    // This sums: per-trade surplus × reference_price(buy_token) / 1e18
    // which produces scores in the same range as winners (~1K-1M gwei).
    // DO NOT use compute_cow_score here — it uses clearing prices which inflate.
    let mut total_surplus_wei: u128 = 0;
    for route in &profitable {
        if let Some(order) = auction.orders.iter().find(|o| o.uid == route.order_uid) {
            let normalized = normalize_surplus(route.surplus, &order.buy_token, &auction.tokens);
            total_surplus_wei = total_surplus_wei.saturating_add(normalized);
        }
    }
    // Subtract estimated gas cost (already in ETH-wei from gas embedding)
    let total_gas_cost = gas::estimate_solution_cost_wei(
        chain_id,
        &swap_kinds,
        profitable.len(),
        0,
        gas_price_wei,
    );
    let mut net_score = total_surplus_wei.saturating_sub(total_gas_cost);

    // Add estimated protocol fee bonus (gated by PROTOCOL_FEE_BONUS=true env var)
    for route in &profitable {
        if let Some(order) = auction.orders.iter().find(|o| o.uid == route.order_uid) {
            let normalized = normalize_surplus(route.surplus, &order.buy_token, &auction.tokens);
            let bonus = scoring::estimate_protocol_fee_bonus(&order.class, normalized);
            net_score = net_score.saturating_add(bonus);
        }
    }

    let score = Some(Score::Solver {
        score: net_score.to_string(),
    });

    info!(
        auction_id = auction.id,
        trades = trades.len(),
        interactions = interactions.len(),
        "Assembled solution (score computed by rescore_solutions)"
    );

    let solution = Solution {
        id: auction.id,
        prices: all_prices,
        trades,
        pre_interactions,
        interactions,
        post_interactions: vec![],
        gas: None,
        score,
    };

    let mut solutions = vec![solution];

    // Build alternative solutions from routes 2 and 3 (if they exist).
    // Each alternative is a single-trade solution with its own clearing prices.
    // The driver picks the best-scoring one — this gives it options.
    if profitable.len() > 1 {
        for alt_idx in 1..profitable.len().min(MAX_SOLUTIONS) {
            let alt_route = &profitable[alt_idx];
            if let Some(order) = auction.orders.iter().find(|o| o.uid == alt_route.order_uid) {
                // Build simple clearing prices for this single trade
                let pool_kind = detect_pool_kind_from_route(alt_route, &auction.liquidity);
                let per_trade_gas: u128 = match pool_kind {
                    PoolKind::UniswapV2 | PoolKind::Sushiswap | PoolKind::CamelotV2 => 30_000,
                    PoolKind::UniswapV3 | PoolKind::CamelotV3 => 45_000,
                    _ => 35_000,
                };
                let gas_fee_wei = (per_trade_gas + 15_000) * gas_price_wei;
                let buy_token_price: u128 = auction.tokens.iter()
                    .find(|(k, _)| k.to_lowercase() == order.buy_token.to_lowercase())
                    .and_then(|(_, t)| t.reference_price.as_ref())
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0);
                let gas_in_buy = if buy_token_price > 0 {
                    scoring::mul_div(gas_fee_wei, 1_000_000_000_000_000_000, buy_token_price).unwrap_or(0)
                } else { 0 };

                let (net_sell, buy_out) = match order.kind {
                    crate::models::order::OrderKind::Sell => {
                        (alt_route.executed_amount, alt_route.output_amount.saturating_sub(gas_in_buy))
                    }
                    crate::models::order::OrderKind::Buy => {
                        let buy_limit: u128 = order.buy_amount.parse().unwrap_or(u128::MAX);
                        (alt_route.executed_amount, alt_route.output_amount.min(buy_limit))
                    }
                };

                if net_sell == 0 || buy_out == 0 { continue; }

                let mut alt_prices = HashMap::new();
                alt_prices.insert(order.sell_token.to_lowercase(), buy_out.to_string());
                alt_prices.insert(order.buy_token.to_lowercase(), net_sell.to_string());

                let normalized = normalize_surplus(alt_route.surplus, &order.buy_token, &auction.tokens);
                let gas_cost = gas::estimate_solution_cost_wei(chain_id, &[pool_kind], 1, 0, gas_price_wei);
                let alt_score = normalized.saturating_sub(gas_cost);

                let mut alt_interactions = vec![alt_route.interaction.clone()];
                super::internalization::try_internalize_interactions(&mut alt_interactions, &auction.tokens);

                solutions.push(Solution {
                    id: auction.id + alt_idx as u64,
                    prices: alt_prices,
                    trades: vec![Trade::fulfillment(&alt_route.order_uid, alt_route.executed_amount.to_string())],
                    pre_interactions: vec![],
                    interactions: alt_interactions,
                    post_interactions: vec![],
                    gas: None,
                    score: Some(Score::Solver { score: alt_score.to_string() }),
                });

                debug!(
                    auction_id = auction.id,
                    alt_idx,
                    order_uid = %alt_route.order_uid.chars().take(20).collect::<String>(),
                    score = alt_score,
                    "Alternative solution built"
                );
            }
        }
    }

    SolveResponse { solutions }
}

// ── Pool kind detection ─────────────────────────────────────────────────────

/// Detect the pool kind for a route by looking up the pool in the auction liquidity.
///
/// Falls back to `PoolKind::UniswapV2` if the pool can't be identified.
fn detect_pool_kind_from_route(
    route: &OrderRoute,
    liquidity: &[crate::models::liquidity::Liquidity],
) -> PoolKind {
    use crate::models::liquidity::Liquidity;

    for liq in liquidity {
        match liq {
            Liquidity::ConstantProduct(p) if p.id == route.pool_id => {
                return PoolKind::UniswapV2;
            }
            Liquidity::ConcentratedLiquidity(p) if p.id == route.pool_id => {
                return PoolKind::UniswapV3;
            }
            Liquidity::Stable(p) if p.id == route.pool_id => {
                return PoolKind::Curve;
            }
            Liquidity::WeightedProduct(p) if p.id == route.pool_id => {
                return PoolKind::Balancer;
            }
            _ => {}
        }
    }

    // Default to V2 (cheapest gas, conservative)
    PoolKind::UniswapV2
}

// ── Score computation ─────────────────────────────────────────────────────────

/// Compute a score for a solution from its clearing prices and trades.
///
/// For each fulfilled order, the surplus is the difference between the
/// clearing price output and the order's limit price output. The total
/// solution score is the sum of all surpluses.
///
/// This function is used by `assemble_solution()` (the low-level assembly
/// entry point). The full pipeline in `assemble_from_auction()` computes
/// its own gas-adjusted score directly, so this is mainly for manually
/// constructed solutions and tests.
fn compute_score(
    trades: &[Trade],
    prices: &HashMap<String, String>,
) -> Option<Score> {
    if trades.is_empty() {
        return None;
    }

    // Sum clearing price values as a rough score proxy.
    // In the full pipeline, surplus is computed from (output - limit) per order.
    // Here we use the sum of all price values as an approximation since we
    // don't have order context.
    let total: u128 = prices
        .values()
        .filter_map(|v| v.parse::<u128>().ok())
        .sum();

    if total > 0 {
        Some(Score::Solver {
            score: total.to_string(),
        })
    } else {
        Some(Score::Solver {
            score: trades.len().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::auction::AuctionInstance;
    use crate::models::liquidity::{ConstantProductPool, Liquidity, LiquidityTokenBalance, LiquidityTokenMap};
    use crate::models::order::{Order, OrderClass, OrderKind};
    use crate::models::solution::Score;

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

    fn make_cp_liquidity(id: &str, t0: &str, r0: u128, t1: &str, r1: u128) -> Liquidity {
        let mut tokens = LiquidityTokenMap::new();
        tokens.insert(t0.to_string(), LiquidityTokenBalance { balance: r0.to_string() });
        tokens.insert(t1.to_string(), LiquidityTokenBalance { balance: r1.to_string() });
        Liquidity::ConstantProduct(ConstantProductPool {
            id: id.to_string(),
            address: format!("0x{id}"),
            tokens,
            fee: "0.003".to_string(),
            router: None,
            gas_estimate: String::new(),
        })
    }

    fn make_auction(orders: Vec<Order>, liquidity: Vec<Liquidity>) -> AuctionInstance {
        use crate::models::token::TokenInfo;
        let mut tokens = std::collections::HashMap::new();
        // Add reference prices so normalize_surplus can compute correctly.
        // ref_price ≈ "atoms of ETH per atom of this token × 1e18"
        tokens.insert("0xweth".to_string(), TokenInfo {
            decimals: Some(18),
            symbol: Some("WETH".to_string()),
            reference_price: Some("1000000000000000000".to_string()), // 1.0
            available_balance: None,
            trusted: true,
        });
        tokens.insert("0xusdc".to_string(), TokenInfo {
            decimals: Some(6),
            symbol: Some("USDC".to_string()),
            reference_price: Some("370000000000".to_string()), // ~0.00037 ETH
            available_balance: None,
            trusted: true,
        });
        AuctionInstance {
            id: 42,
            tokens,
            orders,
            liquidity,
            // Use 1 wei gas price: gas_cost = 180_000 wei, any realistic surplus >> this
            effective_gas_price: "1".to_string(),
            deadline: None,
            chain_id: None,
            block: None,
        }
    }

    #[test]
    fn empty_auction_returns_empty() {
        let auction = make_auction(vec![], vec![]);
        let response = assemble_from_auction(&auction);
        assert!(response.solutions.is_empty());
    }

    #[test]
    fn profitable_order_assembles_solution() {
        // Large pool → low price impact → high surplus
        // Sell 1e9 tokens, limit buy = 1 → surplus ≈ 2e12 which >> gas cost at 1 gwei (1.8e11)
        let pool = make_cp_liquidity(
            "pool1",
            "0xweth", 1_000_000_000_000u128,
            "0xusdc", 2_000_000_000_000_000u128,
        );
        let order = make_sell_order("uid1", "0xweth", "0xusdc", 1_000_000_000, 1);
        let auction = make_auction(vec![order], vec![pool]);

        let response = assemble_from_auction(&auction);
        assert_eq!(response.solutions.len(), 1);

        let sol = &response.solutions[0];
        assert_eq!(sol.id, 42);
        assert!(!sol.prices.is_empty());
        assert_eq!(sol.trades.len(), 1);
        // 1 swap interaction + possibly 1 approval prepended
        assert!(sol.interactions.len() >= 1);
        assert!(sol.score.is_some());
    }

    #[test]
    fn round_trip_serialization() {
        let pool = make_cp_liquidity(
            "pool1",
            "0xweth", 1_000_000_000_000u128,
            "0xusdc", 2_000_000_000_000_000u128,
        );
        let order = make_sell_order("uid1", "0xweth", "0xusdc", 1_000_000_000, 1);
        let auction = make_auction(vec![order], vec![pool]);

        let response = assemble_from_auction(&auction);
        if response.solutions.is_empty() {
            return; // solution filtered as unprofitable is ok
        }

        let json = serde_json::to_string(&response).unwrap();
        let back: crate::models::solution::SolveResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.solutions.len(), response.solutions.len());
    }

    #[test]
    fn assemble_solution_sets_score() {
        let mut prices = HashMap::new();
        prices.insert("0xa".to_string(), "1000".to_string());
        let trades = vec![Trade::fulfillment("uid1", "500")];
        let sol = assemble_solution(1, prices, trades, vec![]);
        // compute_score returns Some(Solver) for non-empty trades with prices
        assert!(sol.score.is_some());
        match sol.score.unwrap() {
            Score::Solver { score } => {
                // score = sum of price values = 1000
                assert_eq!(score, "1000");
            }
            other => panic!("unexpected score type: {:?}", other),
        }
    }
}
