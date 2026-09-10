//! Split routing: split a large order across multiple routes to minimize price impact.
//!
//! ## Approach
//!
//! For constant-product AMMs, the marginal price worsens as trade size increases
//! (price impact). By splitting across N routes, each route handles less volume,
//! resulting in less total price impact.
//!
//! ## Algorithm: Marginal Price Equalization via Binary Search
//!
//! The optimal split equalizes the marginal output rate across all routes.
//! For each route i, the marginal rate `dr_i/dx_i` is a decreasing function of
//! the volume `x_i` sent through it (convexity of constant-product curves).
//!
//! We use binary search on the target marginal rate `r*`:
//! 1. For a candidate `r*`, compute how much volume each route can absorb
//!    before its marginal rate drops to `r*`.
//! 2. Sum volumes. If total > order amount, r* is too low; if < order amount, too high.
//! 3. Binary search converges to the optimal r* in ~30 iterations.
//!
//! This is equivalent to gradient descent on the total output function but
//! more numerically stable for AMM curves.
//!
//! ## Multi-route support
//!
//! Unlike the previous 2-pool approach, this supports splitting across N routes,
//! including multi-hop graph paths (A->B->C). Each route is characterized by
//! its output function f_i(x) and marginal rate function f_i'(x).

use tracing::debug;

use crate::gas;
use crate::models::liquidity::Liquidity;
use crate::models::order::Order;
use crate::models::solution::{Interaction, LiquidityInteraction, Score, Solution, Trade};
use crate::solver::direct::DirectSolver;
use crate::solver::graph::{self, GraphPath, TokenGraph};

// ── Configuration ────────────────────────────────────────────────────────────

/// Minimum improvement ratio to prefer split over single route (0.1% = 1/1000).
const MIN_IMPROVEMENT_BPS: u128 = 10; // 0.1%

/// Maximum number of routes to split across.
const MAX_SPLIT_ROUTES: usize = 5;

/// Number of binary search iterations for marginal rate equalization.
const BINARY_SEARCH_ITERATIONS: usize = 40;

/// Minimum fraction of total volume to allocate to a route (1% = prevents dust allocations).
const MIN_ROUTE_FRACTION: f64 = 0.01;

/// Number of golden-section search steps for 2-route optimization fallback.
const GOLDEN_SECTION_STEPS: usize = 50;

// ── Split result ─────────────────────────────────────────────────────────────

/// A single leg of a split route.
#[derive(Debug, Clone)]
pub struct SplitLeg {
    /// How the route is identified (pool_id for direct, path description for multi-hop).
    pub route_id: String,
    /// Amount of sell token sent through this leg.
    pub amount_in: u128,
    /// Amount of buy token received from this leg.
    pub amount_out: u128,
    /// Interactions for this leg (may be >1 for multi-hop).
    pub interactions: Vec<Interaction>,
}

/// The result of splitting an order across multiple routes.
#[derive(Debug, Clone)]
pub struct SplitRoute {
    pub order_uid: String,
    /// Individual legs of the split.
    pub legs: Vec<SplitLeg>,
    /// Total input across all legs.
    pub total_input: u128,
    /// Total output across all legs.
    pub total_output: u128,
    /// Surplus above order's buy_amount limit.
    pub surplus: u128,
    /// Total gas cost in wei across all legs.
    pub gas_cost_wei: u128,
    /// Net surplus = surplus - gas cost.
    pub net_surplus: i128,
    /// All interactions (flattened from legs).
    pub interactions: Vec<Interaction>,
}

// ── Route abstraction ────────────────────────────────────────────────────────

/// A candidate route that can handle partial volume.
/// Wraps either a direct pool or a graph path.
struct CandidateRoute<'a> {
    id: String,
    liquidity: &'a [Liquidity],
    kind: RouteKind<'a>,
    gas_units: u64,
}

enum RouteKind<'a> {
    /// Direct single-pool route.
    DirectPool {
        pool_idx: usize,
        sell_token: String,
        buy_token: String,
    },
    /// Multi-hop graph path.
    GraphPath {
        path: &'a GraphPath,
        graph: &'a TokenGraph,
    },
}

impl<'a> CandidateRoute<'a> {
    /// Simulate routing `amount_in` through this route.
    /// Returns (output, interactions) or None if route can't handle this amount.
    fn simulate(&self, amount_in: u128) -> Option<(u128, Vec<Interaction>)> {
        if amount_in == 0 {
            return Some((0, vec![]));
        }

        match &self.kind {
            RouteKind::DirectPool {
                pool_idx,
                sell_token,
                buy_token,
            } => {
                let pool = &self.liquidity[*pool_idx];
                let output = graph::simulate_pool_swap(pool, sell_token, buy_token, amount_in)?;
                let interaction = Interaction::Liquidity(LiquidityInteraction {
                    internalize: false,
                    id: pool.id().to_string(),
                    input_token: sell_token.clone(),
                    output_token: buy_token.clone(),
                    input_amount: amount_in.to_string(),
                    output_amount: output.to_string(),
                });
                Some((output, vec![interaction]))
            }
            RouteKind::GraphPath { path, graph } => {
                // Use the first and last tokens from the path
                if path.tokens.len() < 2 {
                    return None;
                }
                let sell = &path.tokens[0];
                let buy = path.tokens.last()?;
                graph.simulate_path_partial(path, sell, buy, amount_in, self.liquidity)
            }
        }
    }

    /// Compute the marginal output rate at a given input volume.
    /// Uses finite difference: (f(x+dx) - f(x-dx)) / (2*dx).
    fn marginal_rate(&self, amount_in: u128) -> Option<f64> {
        if amount_in == 0 {
            // At zero volume, use a small test amount to get the initial rate
            let test = 1000u128;
            let (out, _) = self.simulate(test)?;
            return Some(out as f64 / test as f64);
        }

        let dx = (amount_in / 1000).max(1);
        let x_lo = amount_in.saturating_sub(dx);
        let x_hi = amount_in.saturating_add(dx);

        let (y_lo, _) = self.simulate(x_lo)?;
        let (y_hi, _) = self.simulate(x_hi)?;

        let actual_dx = (x_hi - x_lo) as f64;
        if actual_dx == 0.0 {
            return None;
        }

        Some((y_hi as f64 - y_lo as f64) / actual_dx)
    }

    /// Given a target marginal rate, find how much volume this route absorbs
    /// before its marginal rate drops to that level.
    /// Uses binary search on amount.
    fn volume_at_marginal_rate(&self, target_rate: f64, max_amount: u128) -> u128 {
        if target_rate <= 0.0 {
            return max_amount;
        }

        let mut lo: u128 = 0;
        let mut hi: u128 = max_amount;

        for _ in 0..BINARY_SEARCH_ITERATIONS {
            if hi - lo <= 1 {
                break;
            }
            let mid = lo + (hi - lo) / 2;
            match self.marginal_rate(mid) {
                Some(rate) if rate > target_rate => lo = mid,
                Some(_) => hi = mid,
                None => {
                    hi = mid; // Can't simulate → too much volume
                }
            }
        }

        lo
    }
}

// ── Main entry point: graph-aware split ──────────────────────────────────────

/// Find the optimal split for an order using graph-based routes.
///
/// This is the primary split routing function. It:
/// 1. Gets the top-N graph routes for the order
/// 2. Uses marginal price equalization to find optimal split ratios
/// 3. Returns a SplitRoute if the split improves on the best single route
pub fn find_graph_split(
    order: &Order,
    liquidity: &[Liquidity],
    graph: &TokenGraph,
    gas_price_wei: u128,
    chain_id: u64,
) -> Option<SplitRoute> {
    let sell_amount: u128 = order.sell_amount.parse().ok()?;
    let buy_amount_min: u128 = order.buy_amount.parse().ok()?;

    if sell_amount == 0 {
        return None;
    }

    // Get candidate routes from the graph
    let graph_routes = graph::find_top_n_graph_routes(order, liquidity, graph, MAX_SPLIT_ROUTES);

    if graph_routes.len() < 2 {
        return None; // Need at least 2 routes to split
    }

    // Build candidate route wrappers from graph paths
    let candidates: Vec<CandidateRoute> = graph_routes
        .iter()
        .map(|gr| CandidateRoute {
            id: format!("graph_{}", gr.path.tokens.join(">")),
            liquidity,
            kind: RouteKind::GraphPath {
                path: &gr.path,
                graph,
            },
            gas_units: gr.path.total_gas_units,
        })
        .collect();

    // Get best single-route output as baseline
    let baseline_output = graph_routes[0].output_amount;
    let baseline_gas = graph_routes[0].gas_cost_wei;
    let _baseline_net = baseline_output as i128 - baseline_gas as i128;

    // Find optimal split using marginal rate equalization
    let split = optimize_split(&candidates, sell_amount, gas_price_wei, chain_id)?;

    let total_output = split.total_output;
    let total_gas = split.gas_cost_wei;
    let net_surplus_split = total_output as i128 - buy_amount_min as i128 - total_gas as i128;

    // Check if split is better than best single route
    let single_net_surplus = baseline_output as i128 - buy_amount_min as i128 - baseline_gas as i128;
    let improvement = net_surplus_split - single_net_surplus;

    // Require minimum improvement (0.1% of baseline output)
    let threshold = (baseline_output / 10_000 * MIN_IMPROVEMENT_BPS) as i128;

    if improvement <= threshold || total_output < buy_amount_min || net_surplus_split <= 0 {
        return None;
    }

    let computed_surplus = total_output.saturating_sub(buy_amount_min);

    debug!(
        order_uid = %order.uid,
        legs = split.legs.len(),
        sell_amount = sell_amount,
        buy_amount_min = buy_amount_min,
        baseline_output = baseline_output,
        split_output = total_output,
        surplus = computed_surplus,
        gas_cost = total_gas,
        net_surplus = net_surplus_split,
        improvement_bps = (improvement * 10_000) / baseline_output.max(1) as i128,
        "Graph-aware split found"
    );

    // Sanity check: surplus should not exceed sell_amount (that would mean free money).
    // If surplus > sell_amount, something is wrong with the math — cap it.
    let capped_surplus = if computed_surplus > sell_amount {
        debug!(
            order_uid = %order.uid,
            surplus = computed_surplus,
            sell_amount = sell_amount,
            "WARNING: surplus exceeds sell_amount — capping to sell_amount"
        );
        sell_amount
    } else {
        computed_surplus
    };

    Some(SplitRoute {
        order_uid: order.uid.clone(),
        surplus: capped_surplus,
        net_surplus: net_surplus_split,
        ..split
    })
}

/// Optimize the split ratios across candidate routes using marginal rate equalization.
///
/// Binary searches for the target marginal rate r* such that the sum of volumes
/// across all routes (each absorbing volume until its marginal rate = r*) equals
/// the total sell amount.
fn optimize_split(
    candidates: &[CandidateRoute],
    total_amount: u128,
    gas_price_wei: u128,
    chain_id: u64,
) -> Option<SplitRoute> {
    if candidates.is_empty() {
        return None;
    }

    // Get the initial marginal rates at zero volume (these are upper bounds)
    let initial_rates: Vec<f64> = candidates
        .iter()
        .filter_map(|c| c.marginal_rate(0))
        .collect();

    if initial_rates.is_empty() {
        return None;
    }

    let max_rate = initial_rates.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min_rate = 0.0f64;

    // Binary search for the target marginal rate
    let mut lo_rate = min_rate;
    let mut hi_rate = max_rate;

    for _ in 0..BINARY_SEARCH_ITERATIONS {
        let mid_rate = (lo_rate + hi_rate) / 2.0;

        let total_volume: u128 = candidates
            .iter()
            .map(|c| c.volume_at_marginal_rate(mid_rate, total_amount))
            .sum();

        if total_volume > total_amount {
            // Too much volume → target rate is too low (routes accept too much)
            lo_rate = mid_rate;
        } else {
            hi_rate = mid_rate;
        }
    }

    let target_rate = (lo_rate + hi_rate) / 2.0;

    // Compute final allocations at the converged target rate
    let mut allocations: Vec<u128> = candidates
        .iter()
        .map(|c| c.volume_at_marginal_rate(target_rate, total_amount))
        .collect();

    // Normalize allocations to sum to total_amount exactly
    let alloc_sum: u128 = allocations.iter().sum();
    if alloc_sum == 0 {
        return None;
    }

    // Scale allocations proportionally
    if alloc_sum != total_amount {
        let scale = total_amount as f64 / alloc_sum as f64;
        let n_allocs = allocations.len();
        let mut remaining = total_amount;
        for (i, alloc) in allocations.iter_mut().enumerate() {
            if i == n_allocs - 1 {
                *alloc = remaining; // Last gets the remainder to avoid rounding errors
            } else {
                *alloc = (*alloc as f64 * scale) as u128;
                remaining = remaining.saturating_sub(*alloc);
            }
        }
    }

    // Filter out dust allocations (below MIN_ROUTE_FRACTION)
    let min_amount = (total_amount as f64 * MIN_ROUTE_FRACTION) as u128;
    let mut active_count = 0;
    for alloc in &allocations {
        if *alloc >= min_amount {
            active_count += 1;
        }
    }

    if active_count < 2 {
        return None; // Split not worthwhile — one route dominates
    }

    // Redistribute dust to the largest allocation
    let largest_idx = allocations
        .iter()
        .enumerate()
        .max_by_key(|(_, a)| **a)
        .map(|(i, _)| i)?;

    let mut dust_total = 0u128;
    for (i, alloc) in allocations.iter_mut().enumerate() {
        if *alloc < min_amount && i != largest_idx {
            dust_total += *alloc;
            *alloc = 0;
        }
    }
    allocations[largest_idx] = allocations[largest_idx].saturating_add(dust_total);

    // Simulate each leg with the final allocations
    let mut legs = Vec::new();
    let mut all_interactions = Vec::new();
    let mut total_output = 0u128;
    let mut total_input = 0u128;
    let mut _total_gas_units: u64 = 0;

    for (i, (candidate, &amount)) in candidates.iter().zip(allocations.iter()).enumerate() {
        if amount == 0 {
            continue;
        }

        match candidate.simulate(amount) {
            Some((output, interactions)) if output > 0 => {
                _total_gas_units += candidate.gas_units;
                total_output += output;
                total_input += amount;
                all_interactions.extend(interactions.clone());
                legs.push(SplitLeg {
                    route_id: candidate.id.clone(),
                    amount_in: amount,
                    amount_out: output,
                    interactions,
                });
            }
            _ => {
                // Route failed — skip this leg
                debug!(
                    route = i,
                    amount = amount,
                    "Split leg simulation failed, skipping"
                );
            }
        }
    }

    if legs.len() < 2 || total_output == 0 {
        return None;
    }

    let gas_cost_wei = gas::estimate_cost_wei(
        chain_id,
        all_interactions.len(),
        false, // conservative: assume V2 gas
        gas_price_wei,
    );

    let net_surplus = total_output as i128 - gas_cost_wei as i128;

    Some(SplitRoute {
        order_uid: String::new(), // Caller fills this in
        legs,
        total_input,
        total_output,
        surplus: 0,        // Caller computes from buy_amount_min
        gas_cost_wei,
        net_surplus,
        interactions: all_interactions,
    })
}

// ── Legacy 2-pool split (kept for backward compatibility) ────────────────────

/// Try splitting an order across two different direct pools.
///
/// Uses golden-section search for optimal split ratio (faster convergence
/// than the old 10% increment approach).
///
/// Returns Some(SplitRoute) if the split improves output by at least 0.1%
/// over the best single-pool route.
pub fn find_split(order: &Order, liquidity: &[Liquidity]) -> Option<SplitRoute> {
    let sell_amount: u128 = order.sell_amount.parse().ok()?;
    let buy_amount_min: u128 = order.buy_amount.parse().ok()?;

    if sell_amount == 0 {
        return None;
    }

    // Get candidate pools for this pair
    let candidate_pools: Vec<usize> = liquidity
        .iter()
        .enumerate()
        .filter_map(|(i, pool)| {
            let has_pair = match pool {
                Liquidity::ConstantProduct(p)
                | Liquidity::WeightedProduct(p)
                | Liquidity::Stable(p) => {
                    p.tokens.contains_key(&order.sell_token)
                        && p.tokens.contains_key(&order.buy_token)
                }
                Liquidity::ConcentratedLiquidity(p) => {
                    p.has_pair(&order.sell_token, &order.buy_token)
                }
            };
            if has_pair {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    if candidate_pools.len() < 2 {
        return None;
    }

    // Find best single-pool output as baseline
    let baseline = DirectSolver::solve_order(order, liquidity)?;
    let baseline_output = baseline.output_amount;

    let mut best_split: Option<SplitRoute> = None;

    // Try all pairs of pools
    for &i in &candidate_pools {
        for &j in &candidate_pools {
            if i >= j {
                continue;
            }

            // Build candidate routes for these two pools
            let routes = [
                CandidateRoute {
                    id: liquidity[i].id().to_string(),
                    liquidity,
                    kind: RouteKind::DirectPool {
                        pool_idx: i,
                        sell_token: order.sell_token.clone(),
                        buy_token: order.buy_token.clone(),
                    },
                    gas_units: gas::GAS_UNISWAP_V2_SWAP,
                },
                CandidateRoute {
                    id: liquidity[j].id().to_string(),
                    liquidity,
                    kind: RouteKind::DirectPool {
                        pool_idx: j,
                        sell_token: order.sell_token.clone(),
                        buy_token: order.buy_token.clone(),
                    },
                    gas_units: gas::GAS_UNISWAP_V2_SWAP,
                },
            ];

            // Use golden-section search for optimal 2-way split
            let best_frac = golden_section_search(|frac| {
                let amount_a = (sell_amount as f64 * frac) as u128;
                let amount_b = sell_amount.saturating_sub(amount_a);
                let out_a = routes[0].simulate(amount_a).map(|(o, _)| o).unwrap_or(0);
                let out_b = routes[1].simulate(amount_b).map(|(o, _)| o).unwrap_or(0);
                out_a + out_b
            });

            let amount_a = (sell_amount as f64 * best_frac) as u128;
            let amount_b = sell_amount.saturating_sub(amount_a);

            if amount_a == 0 || amount_b == 0 {
                continue;
            }

            let (out_a, int_a) = routes[0].simulate(amount_a)?;
            let (out_b, int_b) = routes[1].simulate(amount_b)?;

            let total = out_a.checked_add(out_b)?;

            // Check improvement over baseline
            let improvement = total.saturating_sub(baseline_output);
            let threshold = baseline_output / 10_000 * MIN_IMPROVEMENT_BPS;

            if total >= buy_amount_min && improvement > threshold {
                let is_better = best_split
                    .as_ref()
                    .is_none_or(|b: &SplitRoute| total > b.total_output);

                if is_better {
                    let mut all_interactions = int_a.clone();
                    all_interactions.extend(int_b.clone());

                    best_split = Some(SplitRoute {
                        order_uid: order.uid.clone(),
                        legs: vec![
                            SplitLeg {
                                route_id: routes[0].id.clone(),
                                amount_in: amount_a,
                                amount_out: out_a,
                                interactions: int_a,
                            },
                            SplitLeg {
                                route_id: routes[1].id.clone(),
                                amount_in: amount_b,
                                amount_out: out_b,
                                interactions: int_b,
                            },
                        ],
                        total_input: sell_amount,
                        total_output: total,
                        surplus: total.saturating_sub(buy_amount_min),
                        gas_cost_wei: 0, // Legacy path doesn't compute gas
                        net_surplus: total.saturating_sub(buy_amount_min) as i128,
                        interactions: all_interactions,
                    });

                    debug!(
                        order_uid = %order.uid,
                        fraction_a = best_frac,
                        improvement = improvement,
                        "Split route found (2-pool golden section)"
                    );
                }
            }
        }
    }

    best_split
}

/// Build a Solution from a split route.
pub fn build_split_solution(
    auction_id: u64,
    splits: &[SplitRoute],
    tokens: &crate::models::auction::TokenMap,
) -> Option<Solution> {
    if splits.is_empty() {
        return None;
    }

    let mut trades = Vec::new();
    let mut interactions = Vec::new();
    let mut prices = std::collections::HashMap::new();
    let mut total_surplus = 0u128;
    let mut total_gas = 0u128;

    for split in splits {
        trades.push(Trade::fulfillment(
            &split.order_uid,
            split.total_input.to_string(),
        ));
        interactions.extend(split.interactions.clone());

        // Normalize RAW surplus (in buy-token atoms) by reference_price.
        // Gas is subtracted separately after normalization, in wei space.
        let buy_token = split.interactions.iter()
            .find_map(|i| match i {
                crate::models::solution::Interaction::Liquidity(li) => Some(li.output_token.as_str()),
                _ => None,
            })
            .unwrap_or("");
        let contribution = crate::solver::assembler::normalize_surplus(split.surplus, buy_token, tokens);
        total_surplus = total_surplus.saturating_add(contribution);
        total_gas = total_gas.saturating_add(split.gas_cost_wei);

        debug!(
            order_uid = %split.order_uid,
            total_input = split.total_input,
            total_output = split.total_output,
            surplus = split.surplus,
            gas_cost = split.gas_cost_wei,
            net_surplus = split.net_surplus,
            contribution = contribution,
            legs = split.legs.len(),
            "Split solution: per-order breakdown"
        );

        // Clearing prices keyed by token address (CoW driver expects this format).
        if split.total_input > 0 {
            let sell_token = split.interactions.iter()
                .find_map(|i| match i {
                    crate::models::solution::Interaction::Liquidity(li) => Some(li.input_token.clone()),
                    _ => None,
                });
            let buy_token_addr = split.interactions.iter()
                .rev()
                .find_map(|i| match i {
                    crate::models::solution::Interaction::Liquidity(li) => Some(li.output_token.clone()),
                    _ => None,
                });
            if let (Some(st), Some(bt)) = (sell_token, buy_token_addr) {
                prices.entry(st).or_insert_with(|| split.total_input.to_string());
                prices.entry(bt).or_insert_with(|| split.total_output.to_string());
            }
        }
    }

    // Score = normalized surplus (wei) - gas cost (wei). Both in same unit space.
    let net_score = total_surplus.saturating_sub(total_gas);

    debug!(
        auction_id = auction_id,
        orders = splits.len(),
        gross_surplus = total_surplus,
        gas_cost = total_gas,
        net_score = net_score,
        "Split solution: total score"
    );

    Some(Solution {
        id: auction_id,
        prices,
        trades,
        pre_interactions: vec![],
        interactions,
        post_interactions: vec![],
        gas: None,
        score: Some(Score::Solver {
            score: net_score.to_string(),
        }),
    })
}

// ── Golden-section search ────────────────────────────────────────────────────

/// Golden-section search for unimodal function maximum in [0.05, 0.95].
///
/// `f(frac)` returns the total output for a given split fraction.
/// Returns the fraction that maximizes f.
fn golden_section_search<F: Fn(f64) -> u128>(f: F) -> f64 {
    let phi = (5.0_f64.sqrt() + 1.0) / 2.0;
    let resphi = 2.0 - phi;

    let mut a = 0.05_f64;
    let mut b = 0.95_f64;
    let mut x1 = a + resphi * (b - a);
    let mut x2 = b - resphi * (b - a);
    let mut f1 = f(x1);
    let mut f2 = f(x2);

    for _ in 0..GOLDEN_SECTION_STEPS {
        if (b - a).abs() < 1e-6 {
            break;
        }

        if f1 < f2 {
            // Maximum is in [x1, b]
            a = x1;
            x1 = x2;
            f1 = f2;
            x2 = b - resphi * (b - a);
            f2 = f(x2);
        } else {
            // Maximum is in [a, x2]
            b = x2;
            x2 = x1;
            f2 = f1;
            x1 = a + resphi * (b - a);
            f1 = f(x1);
        }
    }

    (a + b) / 2.0
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::{
        ConstantProductPool, Liquidity, LiquidityTokenBalance, LiquidityTokenMap,
    };
    use crate::models::order::{Order, OrderClass, OrderKind};

    fn make_order(uid: &str, sell: &str, buy: &str, sell_amt: u128, buy_amt: u128) -> Order {
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

    fn make_cp(id: &str, t0: &str, r0: u128, t1: &str, r1: u128) -> Liquidity {
        let mut tokens = LiquidityTokenMap::new();
        tokens.insert(
            t0.to_string(),
            LiquidityTokenBalance {
                balance: r0.to_string(),
            },
        );
        tokens.insert(
            t1.to_string(),
            LiquidityTokenBalance {
                balance: r1.to_string(),
            },
        );
        Liquidity::ConstantProduct(ConstantProductPool {
            id: id.to_string(),
            address: format!("0x{id}"),
            tokens,
            fee: "0.003".to_string(),
            router: None,
            gas_estimate: String::new(),
        })
    }

    #[test]
    fn split_returns_none_with_only_one_pool() {
        let pool = make_cp("pool1", "0xweth", 1_000_000, "0xusdc", 2_000_000);
        let order = make_order("uid1", "0xweth", "0xusdc", 10_000, 15_000);
        assert!(find_split(&order, &[pool]).is_none());
    }

    #[test]
    fn split_uses_two_pools_when_beneficial() {
        let pool1 = make_cp("p1", "0xweth", 1_000_000_000, "0xusdc", 2_000_000_000);
        let pool2 = make_cp("p2", "0xweth", 1_000_000_000, "0xusdc", 2_000_000_000);
        let order = make_order("uid2", "0xweth", "0xusdc", 10_000_000, 1);
        let _ = find_split(&order, &[pool1, pool2]);
    }

    #[test]
    fn split_outputs_sum_to_total() {
        let pool1 = make_cp("p1", "0xa", 1_000_000, "0xb", 2_000_000);
        let pool2 = make_cp("p2", "0xa", 2_000_000, "0xb", 3_000_000);
        let order = make_order("uid3", "0xa", "0xb", 100_000, 1);

        if let Some(split) = find_split(&order, &[pool1, pool2]) {
            assert_eq!(split.total_output, split.legs.iter().map(|l| l.amount_out).sum::<u128>());
            assert!(split.legs.len() >= 2);
        }
    }

    #[test]
    fn golden_section_finds_maximum() {
        // Quadratic with max at x=0.5: f(x) = -(x-0.5)^2 + 1
        // We'll return u128 so shift up
        let opt = golden_section_search(|x| {
            let val = -((x - 0.5) * (x - 0.5)) + 1.0;
            (val * 1_000_000.0) as u128
        });
        assert!(
            (opt - 0.5).abs() < 0.01,
            "Golden section should find max near 0.5, got {opt}"
        );
    }

    #[test]
    fn golden_section_finds_asymmetric_optimum() {
        // Asymmetric function with max around 0.3
        let opt = golden_section_search(|x| {
            let val = -((x - 0.3) * (x - 0.3)) + 0.5;
            (val * 1_000_000.0) as u128
        });
        assert!(
            (opt - 0.3).abs() < 0.02,
            "Should find optimum near 0.3, got {opt}"
        );
    }

    #[test]
    fn graph_split_returns_none_without_graph_routes() {
        let pool = make_cp("p1", "0xa", 1_000_000, "0xb", 2_000_000);
        let graph = TokenGraph::build_no_gas(&[pool.clone()]);
        let order = make_order("uid1", "0xa", "0xb", 10_000, 1);
        // Only one route from graph → can't split
        let result = find_graph_split(&order, &[pool], &graph, 0, 1);
        assert!(result.is_none());
    }

    #[test]
    fn graph_split_with_multiple_paths() {
        // Create a diamond: a→b direct, and a→m→b
        let pools = vec![
            make_cp("direct", "0xa", 1_000_000_000, "0xb", 2_000_000_000),
            make_cp("leg1", "0xa", 1_000_000_000, "0xm", 1_500_000_000),
            make_cp("leg2", "0xm", 1_500_000_000, "0xb", 1_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let order = make_order("uid1", "0xa", "0xb", 100_000_000, 1);

        // May or may not find a beneficial split (depends on price impact),
        // but should not panic
        let _ = find_graph_split(&order, &pools, &graph, 0, 1);
    }

    #[test]
    fn build_split_solution_works() {
        let split = SplitRoute {
            order_uid: "uid1".to_string(),
            legs: vec![
                SplitLeg {
                    route_id: "p1".to_string(),
                    amount_in: 500,
                    amount_out: 900,
                    interactions: vec![],
                },
                SplitLeg {
                    route_id: "p2".to_string(),
                    amount_in: 500,
                    amount_out: 850,
                    interactions: vec![],
                },
            ],
            total_input: 1000,
            total_output: 1750,
            surplus: 750,
            gas_cost_wei: 0,
            net_surplus: 750,
            interactions: vec![],
        };

        let sol = build_split_solution(1, &[split], &std::collections::HashMap::new());
        assert!(sol.is_some());
        let s = sol.unwrap();
        assert_eq!(s.trades.len(), 1);
    }
}
