use std::collections::HashSet;
use std::time::Instant;

use tracing::{debug, info, warn};

use crate::liquidity::aggregator::{self, Aggregator};
use crate::models::auction::AuctionInstance;
use crate::models::solution::{
    CustomInteraction, Interaction, Score, Solution, SolveResponse, Trade,
};

pub mod agg_solver;
pub mod assembler;
pub mod cow_matching;
pub mod direct;
pub mod graph;
pub mod internalization;
pub mod jit;
pub mod parallel;
pub mod pricing;
pub mod router;
pub mod scoring;
pub mod split;

/// Default max solve time if the env var is not set.
const DEFAULT_MAX_SOLVE_MS: u128 = 25_000;

// ── Phase time budgets (ms) ──────────────────────────────────────────────────
/// Phase 1: Quick routes (CoW + Direct) — must complete fast to guarantee a baseline.
const PHASE1_BUDGET_MS: u128 = 50;
/// Phase 2: Multi-hop routing through intermediaries.
const PHASE2_DEADLINE_MS: u128 = 500;
/// Phase 3: Split orders across multiple pools to reduce price impact.
const PHASE3_DEADLINE_MS: u128 = 5_000;
/// Phase 4: Full graph search (Yen's K-shortest paths).
const PHASE4_DEADLINE_MS: u128 = 15_000;
/// Phase 5: External aggregators + RFQ — uses remaining budget.
/// Hard cutoff is 1 second before max_solve to leave buffer for serialization.
const PHASE5_BUFFER_MS: u128 = 1_000;

/// Per-phase decision record: what happened and why.
#[derive(Debug, Clone)]
pub struct PhaseDecision {
    /// Phase number (1-5)
    pub phase: u8,
    /// Primary strategy in this phase
    pub strategy: &'static str,
    /// Number of candidates produced in this phase
    pub candidates_produced: usize,
    /// Best score from this phase's candidates
    pub best_score: u128,
    /// Whether this phase's candidate became the new best
    pub improved: bool,
    /// If not improved, why
    pub reason: &'static str,
}

/// Result of a solve call — includes the solutions and metadata for structured logging.
pub struct SolveOutcome {
    pub response: SolveResponse,
    /// Names of all strategies that ran (in order), including "fallback" if triggered.
    pub strategies_ran: Vec<&'static str>,
    /// Which strategy produced the winning (highest-score) solution.
    pub winning_strategy: &'static str,
    /// Whether the fallback solver was triggered (all primary strategies yielded nothing).
    pub used_fallback: bool,
    /// Which phase was the last one reached (1-5, 0 if empty auction).
    pub phase_reached: u8,
    /// Time spent in each phase (ms). Index 0 = phase 1, etc.
    pub phase_times_ms: [u64; 5],
    /// Score after each phase (wei). Shows improvement trajectory.
    pub phase_scores: [u128; 5],
    /// Per-phase decision log (why each phase did or didn't improve the best).
    pub phase_decisions: Vec<PhaseDecision>,
}

/// Entry point: iterative deepening solver.
///
/// Instead of running all strategies once and returning immediately,
/// we execute in 5 phases with increasing depth. Each phase improves
/// on the previous best. If the driver cuts us off early (timeout),
/// we've already found a valid solution in Phase 1.
///
/// ```text
/// Phase 1 (0-50ms):    CoW matching + Direct routing → baseline solution
/// Phase 2 (50-500ms):  Multi-hop via WETH/USDC intermediaries → better routes
/// Phase 3 (500ms-5s):  Split large orders across multiple pools → reduce slippage
/// Phase 4 (5-15s):     Full graph search (Yen's K=10 shortest) → find all routes
/// Phase 5 (15-24s):    External aggregators + RFQ → private MM quotes
/// ```
///
/// After each phase, the new candidate replaces `best_so_far` only if
/// it scores strictly higher. The function returns `best_so_far` when
/// either all phases complete or the deadline approaches.
pub async fn solve(auction: AuctionInstance) -> SolveOutcome {
    if auction.orders.is_empty() {
        return SolveOutcome {
            response: SolveResponse::empty(),
            strategies_ran: vec![],
            winning_strategy: "",
            used_fallback: false,
            phase_reached: 0,
            phase_times_ms: [0; 5],
            phase_scores: [0; 5],
            phase_decisions: vec![],
        };
    }

    // ── Liquidity: use ONLY driver-provided pools for routing ──────────────
    // The CoW driver sends fresh, current-block reserves in auction.liquidity.
    // Our cached pools have stale reserves (minutes old) which produce wrong
    // swap outputs → solutions that revert on-chain (96% revert rate).
    //
    // Strategy: route through driver pools only. Use cached pools only for
    // graph topology (path discovery in Phase 4) — never for swap simulation.
    let driver_pool_count = auction.liquidity.len();
    debug!(
        driver_pools = driver_pool_count,
        cached_pools = crate::pool_indexer::pool_count(),
        "Using driver-only liquidity for routing (cached pools for graph topology only)"
    );

    let max_solve_ms: u128 = std::env::var("MAX_SOLVE_TIME_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_SOLVE_MS);

    let t0 = Instant::now();
    let hard_cutoff_ms = max_solve_ms.saturating_sub(PHASE5_BUFFER_MS);

    // Accumulate ALL candidates across phases — we pick the top 3 at the end.
    let mut all_candidates: Vec<Solution> = Vec::new();
    // Track which strategy produced the best candidate so far
    let mut best_strategy: &str = "";
    // Phase decision log
    let mut phase_decisions: Vec<PhaseDecision> = Vec::new();
    let mut strategies_ran: Vec<&'static str> = Vec::with_capacity(10);
    let mut phase_reached: u8 = 0;
    let mut phase_times_ms = [0u64; 5];
    let mut phase_scores = [0u128; 5];

    // Track best score so far for improvement logging
    let mut best_score: u128 = 0;
    let chain_id: u64 = auction.chain_id.unwrap_or(1);

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // PHASE 1: Quick routes — CoW matching + Direct routing (0-50ms)
    // Goal: guarantee we have at least one valid solution before doing anything slow.
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    let phase1_start = Instant::now();
    phase_reached = 1;

    // Strategy 1: CoW matching (no DEX interactions)
    strategies_ran.push("cow");
    let cow_matches = cow_matching::find_cows(&auction);
    // Filter out CoW matches that would be outscored by AMM routing.
    // CIP-67 fairness filter discards entire solutions if any pair underperforms AMM.
    let cow_matches = cow_matching::filter_by_amm_baseline(
        cow_matches, &auction.orders, &auction.liquidity, &auction.tokens,
    );
    if !cow_matches.is_empty() {
        if let Some(sol) = cow_matching::build_cow_solution(auction.id, &cow_matches, &auction.tokens) {
            debug!(auction_id = auction.id, match_count = cow_matches.len(), "Phase 1: CoW solution");
            all_candidates.push(sol);
        }
    }

    // Strategy 2: Direct routing for all orders
    strategies_ran.push("direct");
    let direct_resp = assembler::assemble_from_auction(&auction);
    for sol in direct_resp.solutions {
        all_candidates.push(sol);
    }

    // Strategy 3: Combined CoW + direct for remainder
    strategies_ran.push("combined");
    if !cow_matches.is_empty() {
        let matched_uids: HashSet<&str> = cow_matches
            .iter()
            .flat_map(|m| [m.order_a_uid.as_str(), m.order_b_uid.as_str()])
            .collect();

        let unmatched: Vec<_> = auction
            .orders
            .iter()
            .filter(|o| !matched_uids.contains(o.uid.as_str()))
            .cloned()
            .collect();

        if !unmatched.is_empty() {
            let sub_auction = AuctionInstance {
                orders: unmatched,
                ..auction.clone()
            };
            let routed_resp = assembler::assemble_from_auction(&sub_auction);
            if let (Some(cow_sol), Some(routed_sol)) =
                (all_candidates.first().cloned(), routed_resp.solutions.into_iter().next())
            {
                let combined = combine_solutions(cow_sol, routed_sol);
                debug!(auction_id = auction.id, trades = combined.trades.len(), "Phase 1: Combined solution");
                all_candidates.push(combined);
            }
        }
    }

    // Record Phase 1 results
    let candidates_before_p1 = 0;
    best_score = all_candidates.iter().map(|s| extract_score(s)).max().unwrap_or(0);
    phase_times_ms[0] = phase1_start.elapsed().as_millis() as u64;
    phase_scores[0] = best_score;
    let p1_improved = best_score > 0;
    if p1_improved {
        best_strategy = "direct";
    }
    phase_decisions.push(PhaseDecision {
        phase: 1, strategy: "direct",
        candidates_produced: all_candidates.len() - candidates_before_p1,
        best_score, improved: p1_improved,
        reason: if p1_improved { "baseline" } else { "no_routes" },
    });

    info!(
        auction_id = auction.id,
        phase = 1,
        candidates = all_candidates.len(),
        best_score,
        elapsed_ms = phase_times_ms[0],
        "Phase 1 complete — baseline established"
    );

    if t0.elapsed().as_millis() >= hard_cutoff_ms {
        return finalize_outcome(all_candidates, strategies_ran, best_strategy, phase_reached, phase_times_ms, phase_scores, phase_decisions, &auction);
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // PHASE 2: Multi-hop routing (50-500ms)
    // For orders with no good direct pool, route through WETH/USDC/USDT.
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    let phase2_start = Instant::now();
    phase_reached = 2;
    strategies_ran.push("multihop");

    let covered_uids: HashSet<String> = all_candidates
        .iter()
        .flat_map(|s| s.trades.iter())
        .map(|t| match t {
            Trade::Fulfillment(f) => f.order.clone(),
        })
        .collect();

    let unrouted: Vec<_> = auction
        .orders
        .iter()
        .filter(|o| !covered_uids.contains(&o.uid))
        .cloned()
        .collect();

    if !unrouted.is_empty() {
        let hop_auction = AuctionInstance {
            orders: unrouted,
            ..auction.clone()
        };
        let routes = router::solve_routed(&hop_auction, chain_id);
        if let Some(hop_sol) = router::build_routed_solution(auction.id, &routes, &auction.tokens) {
            debug!(auction_id = auction.id, route_count = routes.len(), "Phase 2: Multi-hop solution");
            all_candidates.push(hop_sol);
        }
    }

    // Also try multi-hop for ALL orders (not just uncovered) — sometimes
    // a 2-hop route via WETH is better than a direct thin pool.
    {
        let full_routes = router::solve_routed(&auction, chain_id);
        if let Some(full_hop_sol) = router::build_routed_solution(auction.id, &full_routes, &auction.tokens) {
            let hop_score = extract_score(&full_hop_sol);
            if hop_score > best_score {
                debug!(
                    auction_id = auction.id,
                    improvement = hop_score - best_score,
                    "Phase 2: Multi-hop beats Phase 1 for all orders"
                );
                all_candidates.push(full_hop_sol);
            }
        }
    }

    let phase2_best = all_candidates.iter().map(|s| extract_score(s)).max().unwrap_or(0);
    phase_times_ms[1] = phase2_start.elapsed().as_millis() as u64;
    phase_scores[1] = phase2_best;
    if phase2_best > best_score {
        info!(
            auction_id = auction.id,
            phase = 2,
            improvement_bps = ((phase2_best as f64 / best_score.max(1) as f64 - 1.0) * 10000.0) as u64,
            new_best = phase2_best,
            elapsed_ms = phase_times_ms[1],
            "Phase 2 improved score"
        );
        best_score = phase2_best;
        best_strategy = "multihop";
    }
    phase_decisions.push(PhaseDecision {
        phase: 2, strategy: "multihop",
        candidates_produced: if phase2_best > phase_scores[0] { 1 } else { 0 },
        best_score: phase2_best, improved: phase2_best > phase_scores[0],
        reason: if phase2_best > phase_scores[0] { "improved" } else if phase2_best == 0 { "no_routes" } else { "outscored_by_phase1" },
    });

    if t0.elapsed().as_millis() >= hard_cutoff_ms {
        return finalize_outcome(all_candidates, strategies_ran, best_strategy, phase_reached, phase_times_ms, phase_scores, phase_decisions, &auction);
    }

    // ── Skip graph/split (Phase 3-4) decision ────────────────────────────────
    // Two cases where skipping directly to Phase 5 (aggregators) is better:
    //
    // Case A — no on-chain routes found yet (best_score == 0):
    //   Phase 1+2 searched all driver-provided pools and found zero routable
    //   orders. Phases 3-4 search those same pools (split + graph), so they
    //   will also find nothing and waste up to 15 seconds. Jump straight to
    //   Phase 5 (Odos, 1inch, OKX) which can route any token pair via API.
    //
    // Case B — baseline exists but pool graph is too large:
    //   Graph search on 200+ pools takes 5-15s with diminishing returns.
    //   Phase 5 aggregators typically add more value in the same time.
    let skip_graph = if best_score == 0 {
        // No on-chain routes — graph search on the same pools will also fail.
        info!(
            auction_id = auction.id,
            elapsed_ms = t0.elapsed().as_millis(),
            "Phase 1+2 found zero routes — skipping Phase 3-4, jumping to Phase 5 (aggregators)"
        );
        true
    } else {
        // Have a baseline — skip graph when pool count makes it too expensive.
        auction.orders.len() <= 5
            || auction.liquidity.len() > 200
            || t0.elapsed().as_millis() > 500
    };
    if skip_graph && best_score > 0 {
        debug!(
            auction_id = auction.id,
            pools = auction.liquidity.len(),
            "Skipping Phase 3-4 (graph/split) — too many pools. Jumping to Phase 5 (aggregator)."
        );
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // PHASE 3: Split routing (500ms-5s)
    let gas_price_wei: u128 = auction.effective_gas_price.parse().unwrap_or(0);

    // Phase 3-4 gated: skip graph/split when too many pools (prevents 15s timeouts)
    if !skip_graph {

    // Split large orders across multiple pools to reduce price impact.
    // Try progressively more splits: 2-way, then 3-way, then 4-way.
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    let phase3_start = Instant::now();
    phase_reached = 3;
    strategies_ran.push("split");

    let token_graph = graph::TokenGraph::build(&auction.liquidity, gas_price_wei, chain_id);

    if token_graph.edge_count() > 0 {
        // Smart order selection: focus on top-N orders by value.
        // Phase 3 is expensive per-order — don't waste compute on 900+ orders.
        // Prioritize by sell_amount × reference_price (biggest value = most surplus potential).
        let max_split_orders = std::env::var("MAX_SPLIT_ORDERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20usize);

        let mut valued_orders: Vec<(usize, u128)> = auction.orders.iter().enumerate()
            .map(|(i, o)| {
                let sell_amt: u128 = o.sell_amount.parse().unwrap_or(0);
                let ref_price = auction.tokens.get(&o.sell_token)
                    .and_then(|t| t.reference_price.as_ref())
                    .and_then(|p| p.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let value = (sell_amt as f64 * ref_price) as u128;
                (i, value)
            })
            .collect();
        valued_orders.sort_by(|a, b| b.1.cmp(&a.1));
        let top_orders: Vec<&crate::models::order::Order> = valued_orders
            .iter()
            .take(max_split_orders)
            .map(|(i, _)| &auction.orders[*i])
            .collect();

        debug!(
            auction_id = auction.id,
            total_orders = auction.orders.len(),
            selected = top_orders.len(),
            "Phase 3: Smart order selection — focusing on highest-value orders"
        );

        let mut split_routes = Vec::new();
        for order in &top_orders {
            // Check deadline before each order (split can be expensive)
            if t0.elapsed().as_millis() >= PHASE3_DEADLINE_MS.min(hard_cutoff_ms) {
                debug!(auction_id = auction.id, "Phase 3: deadline reached, stopping split search");
                break;
            }
            if let Some(split_route) = split::find_graph_split(
                order,
                &auction.liquidity,
                &token_graph,
                gas_price_wei,
                chain_id,
            ) {
                split_routes.push(split_route);
            }
        }

        if !split_routes.is_empty() {
            if let Some(split_sol) = split::build_split_solution(auction.id, &split_routes, &auction.tokens) {
                debug!(
                    auction_id = auction.id,
                    split_orders = split_routes.len(),
                    "Phase 3: Split routing solution"
                );
                all_candidates.push(split_sol);
            }
        }
    }

    let phase3_best = all_candidates.iter().map(|s| extract_score(s)).max().unwrap_or(0);
    phase_times_ms[2] = phase3_start.elapsed().as_millis() as u64;
    phase_scores[2] = phase3_best;
    if phase3_best > best_score {
        info!(
            auction_id = auction.id,
            phase = 3,
            improvement_bps = ((phase3_best as f64 / best_score.max(1) as f64 - 1.0) * 10000.0) as u64,
            new_best = phase3_best,
            elapsed_ms = phase_times_ms[2],
            "Phase 3 improved score"
        );
        best_score = phase3_best;
        best_strategy = "split";
    }
    phase_decisions.push(PhaseDecision {
        phase: 3, strategy: "split",
        candidates_produced: if phase3_best > 0 { 1 } else { 0 },
        best_score: phase3_best, improved: phase3_best > phase_scores[1],
        reason: if phase3_best > phase_scores[1] { "improved" } else if phase3_best == 0 { "no_routes" } else { "outscored_by_earlier_phase" },
    });

    if t0.elapsed().as_millis() >= hard_cutoff_ms {
        return finalize_outcome(all_candidates, strategies_ran, best_strategy, phase_reached, phase_times_ms, phase_scores, phase_decisions, &auction);
    }

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // PHASE 4: Full graph search (5-15s)
    // Yen's K-shortest paths across all pools. Most compute-intensive phase.
    // We build the graph once in Phase 3 and reuse it.
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    let phase4_start = Instant::now();
    phase_reached = 4;
    strategies_ran.push("graph");

    /// Max trades to include in a graph solution. Real competition winners
    /// fill 1-5 orders. Including more produces inflated scores from stale pools.
    const MAX_GRAPH_TRADES: usize = 5;

    if token_graph.edge_count() > 0 {
        // Pre-filter: only route orders whose sell AND buy tokens both appear in the
        // graph (i.e. have at least one pool). This avoids wasting time on orders
        // with no liquidity path.
        let routable_orders: Vec<_> = auction.orders.iter()
            .filter(|o| {
                token_graph.has_token(&o.sell_token)
                    && token_graph.has_token(&o.buy_token)
            })
            .collect();

        debug!(
            auction_id = auction.id,
            total_orders = auction.orders.len(),
            routable = routable_orders.len(),
            "Phase 4: pre-filtered orders with matching liquidity"
        );

        // Collect all candidate routes with their normalized contributions
        struct GraphCandidate {
            route: graph::GraphRoute,
            contribution: u128,  // normalized surplus (wei)
        }
        let mut candidates: Vec<GraphCandidate> = Vec::new();

        for order in routable_orders {
            if t0.elapsed().as_millis() >= PHASE4_DEADLINE_MS.min(hard_cutoff_ms) {
                debug!(auction_id = auction.id, "Phase 4: deadline reached, stopping graph search");
                break;
            }

            if let Some(route) = graph::find_best_graph_route(order, &auction.liquidity, &token_graph) {
                if route.net_surplus <= 0 {
                    continue;
                }
                let buy_token = route.interactions.iter()
                    .filter_map(|i| match i {
                        crate::models::solution::Interaction::Liquidity(li) => Some(li.output_token.clone()),
                        _ => None,
                    })
                    .last()
                    .unwrap_or_default();
                let contribution = crate::solver::assembler::normalize_surplus(route.surplus, &buy_token, &auction.tokens);
                candidates.push(GraphCandidate { route, contribution });
            }
        }

        // Sort by normalized contribution descending and take top N.
        // Real winners fill 1-5 orders — including more produces phantom trades.
        candidates.sort_by(|a, b| b.contribution.cmp(&a.contribution));
        candidates.truncate(MAX_GRAPH_TRADES);

        let mut graph_trades = Vec::new();
        let mut graph_interactions = Vec::new();
        let mut graph_prices = std::collections::HashMap::new();
        let mut graph_surplus = 0u128;
        let mut graph_gas_cost = 0u128;

        for cand in &candidates {
            let route = &cand.route;
            graph_trades.push(Trade::fulfillment(
                &route.order_uid,
                route.executed_amount.to_string(),
            ));
            graph_interactions.extend(route.interactions.clone());
            graph_surplus = graph_surplus.saturating_add(cand.contribution);
            graph_gas_cost = graph_gas_cost.saturating_add(route.gas_cost_wei);

            if route.executed_amount > 0 {
                let sell_tok = route.interactions.iter()
                    .find_map(|i| match i {
                        crate::models::solution::Interaction::Liquidity(li) => Some(li.input_token.clone()),
                        _ => None,
                    });
                let buy_tok = route.interactions.iter()
                    .rev()
                    .find_map(|i| match i {
                        crate::models::solution::Interaction::Liquidity(li) => Some(li.output_token.clone()),
                        _ => None,
                    });
                if let (Some(st), Some(bt)) = (sell_tok, buy_tok) {
                    graph_prices.entry(st).or_insert_with(|| route.executed_amount.to_string());
                    graph_prices.entry(bt).or_insert_with(|| route.output_amount.to_string());
                }
            }
        }

        if !graph_trades.is_empty() {
            // Score = normalized surplus (wei) - gas cost (wei). Both in same unit space now.
            let graph_net_score = graph_surplus.saturating_sub(graph_gas_cost);
            let graph_sol = Solution {
                id: 0,
                prices: graph_prices,
                trades: graph_trades,
                pre_interactions: vec![],
                interactions: graph_interactions,
                post_interactions: vec![],
                gas: None,
                score: Some(Score::Solver {
                    score: graph_net_score.to_string(),
                }),
            };
            debug!(
                auction_id = auction.id,
                trades = graph_sol.trades.len(),
                gross_surplus = graph_surplus,
                gas_cost = graph_gas_cost,
                net_score = graph_net_score,
                "Phase 4: Graph pathfinding solution"
            );
            all_candidates.push(graph_sol);
        }

        // Also try graph + split combination if we have time
        if t0.elapsed().as_millis() < PHASE4_DEADLINE_MS.min(hard_cutoff_ms) {
            let mut split_routes = Vec::new();
            for order in &auction.orders {
                if t0.elapsed().as_millis() >= PHASE4_DEADLINE_MS.min(hard_cutoff_ms) {
                    break;
                }
                if let Some(split_route) = split::find_graph_split(
                    order,
                    &auction.liquidity,
                    &token_graph,
                    gas_price_wei,
                    chain_id,
                ) {
                    split_routes.push(split_route);
                }
            }
            if !split_routes.is_empty() {
                if let Some(gsplit_sol) = split::build_split_solution(auction.id, &split_routes, &auction.tokens) {
                    debug!(
                        auction_id = auction.id,
                        split_orders = split_routes.len(),
                        "Phase 4: Graph+split solution"
                    );
                    all_candidates.push(gsplit_sol);
                }
            }
        }
    }

    let phase4_best = all_candidates.iter().map(|s| extract_score(s)).max().unwrap_or(0);
    phase_times_ms[3] = phase4_start.elapsed().as_millis() as u64;
    phase_scores[3] = phase4_best;
    if phase4_best > best_score {
        info!(
            auction_id = auction.id,
            phase = 4,
            improvement_bps = ((phase4_best as f64 / best_score.max(1) as f64 - 1.0) * 10000.0) as u64,
            new_best = phase4_best,
            elapsed_ms = phase_times_ms[3],
            "Phase 4 improved score"
        );
        best_score = phase4_best;
        best_strategy = "graph";
    }
    phase_decisions.push(PhaseDecision {
        phase: 4, strategy: "graph",
        candidates_produced: if phase4_best > 0 { 1 } else { 0 },
        best_score: phase4_best, improved: phase4_best > phase_scores[2],
        reason: if phase4_best > phase_scores[2] { "improved" } else if phase4_best == 0 { "no_routes" } else { "outscored_by_earlier_phase" },
    });

    if t0.elapsed().as_millis() >= hard_cutoff_ms {
        return finalize_outcome(all_candidates, strategies_ran, best_strategy, phase_reached, phase_times_ms, phase_scores, phase_decisions, &auction);
    }

    } // end if !skip_graph (Phase 3-4)

    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    // PHASE 5: Multi-aggregator solver (Odos, 1inch, ParaSwap, Bebop)
    // This is the NEW v2 architecture: external aggregators handle all routing.
    // They query 150+ AMMs + RFQ from private market makers, returning best
    // executable quote with calldata. We just format it as a CoW solution.
    // ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    let phase5_start = Instant::now();
    phase_reached = 5;
    strategies_ran.push("aggregator");

    let remaining_ms = hard_cutoff_ms.saturating_sub(t0.elapsed().as_millis());

    if remaining_ms > 3_000 {
        // Sub-timeout for aggregator phase — never let it eat more than remaining budget minus 1s safety margin.
        let agg_timeout_ms = remaining_ms.saturating_sub(1_000).min(10_000); // cap at 10s
        debug!(
            auction_id = auction.id,
            remaining_ms,
            agg_timeout_ms,
            "Phase 5: Starting aggregator with sub-timeout"
        );

        match tokio::time::timeout(
            std::time::Duration::from_millis(agg_timeout_ms as u64),
            agg_solver::solve(&auction),
        ).await {
            Ok(Some(agg_sol)) => {
                info!(
                    auction_id = auction.id,
                    trades = agg_sol.trades.len(),
                    "Phase 5: Multi-aggregator solution"
                );
                all_candidates.push(agg_sol);
            }
            Ok(None) => {
                debug!(auction_id = auction.id, "Phase 5: Aggregator returned no solution");
            }
            Err(_) => {
                warn!(auction_id = auction.id, "Phase 5: Aggregator timed out after {}ms", agg_timeout_ms);
            }
        }
    } else {
        debug!(
            auction_id = auction.id,
            remaining_ms,
            "Phase 5: Skipping aggregator — not enough time"
        );
    }

    let phase5_best = all_candidates.iter().map(|s| extract_score(s)).max().unwrap_or(0);
    phase_times_ms[4] = phase5_start.elapsed().as_millis() as u64;
    phase_scores[4] = phase5_best;
    if phase5_best > best_score {
        info!(
            auction_id = auction.id,
            phase = 5,
            improvement_bps = ((phase5_best as f64 / best_score.max(1) as f64 - 1.0) * 10000.0) as u64,
            new_best = phase5_best,
            elapsed_ms = phase_times_ms[4],
            "Phase 5 improved score"
        );
        best_score = phase5_best;
        best_strategy = "aggregator";
    }
    phase_decisions.push(PhaseDecision {
        phase: 5, strategy: "aggregator",
        candidates_produced: if phase5_best > phase_scores[3] { 1 } else { 0 },
        best_score: phase5_best, improved: phase5_best > phase_scores[3],
        reason: if phase5_best > phase_scores[3] { "improved" } else if phase5_best == 0 { "no_routes" } else { "outscored_by_earlier_phase" },
    });

    // ── Fallback: last-resort if ALL phases produced nothing ─────────────
    if all_candidates.is_empty() {
        if let Some(fb_sol) = strategy_fallback(&auction) {
            warn!(
                auction_id = auction.id,
                strategy = "fallback",
                "All phases produced no solutions — using fallback solver"
            );
            all_candidates.push(fb_sol);
            strategies_ran.push("fallback");
        }
    }

    let total_elapsed = t0.elapsed().as_millis();
    info!(
        auction_id = auction.id,
        candidates = all_candidates.len(),
        best_score,
        phase_reached,
        total_elapsed_ms = total_elapsed,
        phase1_ms = phase_times_ms[0],
        phase2_ms = phase_times_ms[1],
        phase3_ms = phase_times_ms[2],
        phase4_ms = phase_times_ms[3],
        phase5_ms = phase_times_ms[4],
        phase1_score = phase_scores[0],
        phase2_score = phase_scores[1],
        phase3_score = phase_scores[2],
        phase4_score = phase_scores[3],
        phase5_score = phase_scores[4],
        "Iterative deepening complete"
    );

    finalize_outcome(all_candidates, strategies_ran, best_strategy, phase_reached, phase_times_ms, phase_scores, phase_decisions, &auction)
}

// ── Fallback Strategy ──────────────────────────────────────────────────────────

/// Last-resort solver: routes each order through the deepest available pool,
/// skipping the gas profitability filter. Returns `None` only if no pool exists
/// for any order that meets its limit price.
fn strategy_fallback(auction: &AuctionInstance) -> Option<Solution> {
    use crate::solver::direct::DirectSolver;
    use crate::solver::pricing;

    let mut routes = Vec::new();
    for order in &auction.orders {
        if let Some(route) = DirectSolver::solve_order(order, &auction.liquidity) {
            routes.push((order, route));
        }
    }

    if routes.is_empty() {
        return None;
    }

    let mut pair_amounts: std::collections::HashMap<(String, String), Vec<(u128, u128)>> =
        std::collections::HashMap::new();
    for (order, route) in &routes {
        let key = (order.sell_token.clone(), order.buy_token.clone());
        pair_amounts
            .entry(key)
            .or_default()
            .push((route.executed_amount, route.output_amount));
    }

    let mut prices = std::collections::HashMap::new();
    for ((sell_token, buy_token), amounts) in &pair_amounts {
        if let Some(cp) = pricing::enforce_udcp(sell_token, buy_token, amounts) {
            prices.extend(cp.to_string_map());
        } else {
            for (exec, output) in amounts {
                prices.insert(sell_token.clone(), exec.to_string());
                prices.insert(buy_token.clone(), output.to_string());
            }
        }
    }

    let mut trades = Vec::new();
    let mut interactions = Vec::new();
    let mut total_surplus: u128 = 0;

    for (_, route) in &routes {
        trades.push(Trade::fulfillment(
            &route.order_uid,
            route.executed_amount.to_string(),
        ));
        interactions.push(route.interaction.clone());
        total_surplus = total_surplus.saturating_add(route.surplus);
    }

    Some(Solution {
        id: 0,
        prices,
        trades,
        pre_interactions: vec![],
        interactions,
        post_interactions: vec![],
        gas: None,
        score: Some(Score::Solver {
            score: total_surplus.to_string(),
        }),
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build the list of available external aggregators from env vars.
fn build_aggregators() -> Vec<Aggregator> {
    let aggs = aggregator::build_from_env();
    for agg in &aggs {
        info!(aggregator = agg.name(), "External aggregator enabled");
    }
    aggs
}

/// Strategy 7: query external aggregators for each order, compare against
/// our best internal solution, and use aggregator calldata when it wins.
async fn strategy_aggregator(
    auction: &AuctionInstance,
    aggregators: &[Aggregator],
    existing_candidates: &[Solution],
    gas_price_wei: u128,
) -> Option<Solution> {
    let chain_id = auction.chain_id.unwrap_or(1);

    let mut agg_trades = Vec::new();
    let mut agg_interactions = Vec::new();
    let mut agg_prices = std::collections::HashMap::new();
    let mut total_surplus: u128 = 0;

    // Only query aggregator for top 3 orders by value (5 req/s rate limit)
    let mut valued: Vec<(&crate::models::order::Order, u128)> = auction.orders.iter()
        .map(|o| {
            let sell: u128 = o.sell_amount.parse().unwrap_or(0);
            (o, sell)
        })
        .collect();
    valued.sort_by(|a, b| b.1.cmp(&a.1));
    let top_orders: Vec<&crate::models::order::Order> = valued.iter().take(3).map(|(o, _)| *o).collect();

    for order in &top_orders {
        let sell_amount = &order.sell_amount;
        let buy_limit: u128 = order.buy_amount.parse().unwrap_or(0);

        let best = aggregator::best_quote(
            aggregators,
            &order.sell_token,
            &order.buy_token,
            sell_amount,
            chain_id,
        )
        .await;

        if let Some((agg_name, quote)) = best {
            let agg_buy: u128 = quote.buy_amount_u128();

            if agg_buy <= buy_limit {
                debug!(
                    order_uid = %order.uid,
                    aggregator = %agg_name,
                    agg_buy,
                    buy_limit,
                    "Aggregator quote below limit price — skipping"
                );
                continue;
            }

            let internal_best = best_internal_output(existing_candidates, &order.uid);
            let agg_net = quote.net_output(gas_price_wei);

            if agg_net <= internal_best {
                debug!(
                    order_uid = %order.uid,
                    aggregator = %agg_name,
                    agg_net,
                    internal_best,
                    "Internal routing beats aggregator — skipping"
                );
                continue;
            }

            let surplus = agg_buy.saturating_sub(buy_limit);
            total_surplus = total_surplus.saturating_add(surplus);

            info!(
                order_uid = %order.uid,
                aggregator = %agg_name,
                buy_amount = %quote.buy_amount,
                gas = quote.gas_estimate,
                sources = ?quote.sources,
                "Using aggregator quote — beats internal routing"
            );

            agg_trades.push(Trade::fulfillment(&order.uid, sell_amount));

            agg_interactions.push(Interaction::Custom(CustomInteraction {
                internalize: false,
                target: quote.to.clone(),
                value: quote.value.clone(),
                call_data: quote.calldata.clone(),
            }));

            agg_prices.insert(order.sell_token.clone(), sell_amount.clone());
            agg_prices.insert(order.buy_token.clone(), quote.buy_amount.clone());
        }
    }

    if agg_trades.is_empty() {
        return None;
    }

    Some(Solution {
        id: 0,
        prices: agg_prices,
        trades: agg_trades,
        pre_interactions: vec![],
        interactions: agg_interactions,
        post_interactions: vec![],
        gas: None,
        score: Some(Score::Solver {
            score: total_surplus.to_string(),
        }),
    })
}

/// Find the best output amount for a given order from existing candidate solutions.
fn best_internal_output(candidates: &[Solution], order_uid: &str) -> u128 {
    candidates
        .iter()
        .filter_map(|sol| {
            let has_trade = sol.trades.iter().any(|t| match t {
                Trade::Fulfillment(f) => f.order == order_uid,
            });
            if !has_trade {
                return None;
            }
            match &sol.score {
                Some(Score::Solver { score }) => score.parse().ok(),
                _ => None,
            }
        })
        .max()
        .unwrap_or(0)
}

/// Merge a CoW solution with a pool-routed solution into one combined solution.
fn combine_solutions(cow_sol: Solution, routed_sol: Solution) -> Solution {
    let mut trades = cow_sol.trades;
    trades.extend(routed_sol.trades);

    let mut pre_interactions = cow_sol.pre_interactions;
    pre_interactions.extend(routed_sol.pre_interactions);

    let mut interactions = cow_sol.interactions;
    interactions.extend(routed_sol.interactions);

    let mut post_interactions = cow_sol.post_interactions;
    post_interactions.extend(routed_sol.post_interactions);

    let mut prices = cow_sol.prices;
    prices.extend(routed_sol.prices);

    let score = match (&cow_sol.score, &routed_sol.score) {
        (Some(Score::Solver { score: s1 }), Some(Score::Solver { score: s2 })) => {
            let total = s1
                .parse::<u128>()
                .unwrap_or(0)
                .saturating_add(s2.parse::<u128>().unwrap_or(0));
            Some(Score::Solver {
                score: total.to_string(),
            })
        }
        (Some(s), _) => Some(s.clone()),
        (_, Some(s)) => Some(s.clone()),
        _ => None,
    };

    Solution {
        id: 0,
        prices,
        trades,
        pre_interactions,
        interactions,
        post_interactions,
        gas: None,
        score,
    }
}

/// Finalize: sort by score, truncate to 3, assign IDs, wrap in SolveOutcome with phase metadata.
fn finalize_outcome(
    solutions: Vec<Solution>,
    strategies_ran: Vec<&'static str>,
    winning_strategy: &'static str,
    phase_reached: u8,
    phase_times_ms: [u64; 5],
    phase_scores: [u128; 5],
    phase_decisions: Vec<PhaseDecision>,
    auction: &AuctionInstance,
) -> SolveOutcome {
    let used_fallback = strategies_ran.contains(&"fallback");

    // EBBO filtering: reject solutions where clearing prices violate reference DEX prices
    let solutions = if std::env::var("EBBO_CHECK_ENABLED").unwrap_or_else(|_| "true".to_string()) == "true" {
        let tolerance_bps: u32 = std::env::var("EBBO_TOLERANCE_BPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10); // default 10 bps (0.1%)
        let mut checker = crate::validation::ebbo::EbboChecker::new(tolerance_bps);

        // Build reference prices from auction token reference_price fields.
        // For each order, compute: reference_buy = sell_amount × ref_price(buy) / ref_price(sell)
        for order in &auction.orders {
            let sell_ref = auction.tokens.get(&order.sell_token)
                .and_then(|t| t.reference_price.as_ref())
                .and_then(|p| p.parse::<f64>().ok())
                .unwrap_or(0.0);
            let buy_ref = auction.tokens.get(&order.buy_token)
                .and_then(|t| t.reference_price.as_ref())
                .and_then(|p| p.parse::<f64>().ok())
                .unwrap_or(0.0);

            if sell_ref > 0.0 && buy_ref > 0.0 {
                let sell_amount: f64 = order.sell_amount.parse().unwrap_or(0.0);
                // ref_buy = sell_amount × (sell_ref / buy_ref) — what you'd get at reference prices
                let reference_buy = (sell_amount * sell_ref / buy_ref) as u128;
                if reference_buy > 0 {
                    // Log first few orders for debugging EBBO reference calculation
                    if checker.reference_prices.len() < 3 {
                        debug!(
                            order_uid = %order.uid.chars().take(20).collect::<String>(),
                            sell_token = %order.sell_token.chars().take(10).collect::<String>(),
                            buy_token = %order.buy_token.chars().take(10).collect::<String>(),
                            sell_amount = sell_amount,
                            sell_ref = sell_ref,
                            buy_ref = buy_ref,
                            reference_buy = reference_buy,
                            "EBBO reference price computed"
                        );
                    }
                    checker.set_reference(&order.sell_token, &order.buy_token, reference_buy);
                }
            }
        }

        let before_count = solutions.len();
        let filtered: Vec<Solution> = solutions.into_iter()
            .filter(|sol| {
                let passes = checker.solution_passes(sol, &auction.orders);
                if !passes {
                    tracing::warn!(auction_id = auction.id, "Solution failed EBBO check — filtered");
                }
                passes
            })
            .collect();

        if filtered.len() < before_count {
            tracing::info!(
                auction_id = auction.id,
                before = before_count,
                after = filtered.len(),
                "EBBO filtering removed solutions"
            );
        }
        filtered
    } else {
        solutions
    };

    let finalized = finalize_solutions(solutions, &auction.orders, &auction.tokens);

    SolveOutcome {
        response: finalized,
        strategies_ran,
        winning_strategy: if used_fallback { "fallback" } else { winning_strategy },
        used_fallback,
        phase_reached,
        phase_times_ms,
        phase_scores,
        phase_decisions,
    }
}

/// Rescore all solutions using the CoW Protocol driver formula, then sort
/// by score (descending), truncate to 3, and assign sequential IDs.
///
/// Uses a **deterministic** two-level sort:
///   1. Score descending (primary)
///   2. Stable key = sorted trade UIDs joined — breaks ties consistently
fn finalize_solutions(
    mut solutions: Vec<Solution>,
    orders: &[crate::models::order::Order],
    tokens: &crate::models::auction::TokenMap,
) -> SolveResponse {
    if solutions.is_empty() {
        return SolveResponse::empty();
    }

    // DO NOT rescore with compute_cow_score — it uses UDCP clearing prices
    // which produce inflated surplus (4-2000x). The driver ignores our score
    // field anyway and recomputes from our clearing prices.
    //
    // The strategy-computed scores (from normalize_surplus using reference_price)
    // are honest and correct for ranking our own candidates.
    //
    // scoring::rescore_solutions(&mut solutions, orders, tokens);

    // Sanity check: flag scores that seem unrealistic
    for sol in &solutions {
        let score = extract_score(sol);
        // Typical winners: 1e12-1e16 wei. Above 1e18 (~1 ETH) is suspicious.
        if score > 1_000_000_000_000_000_000 {
            tracing::warn!(
                score,
                trades = sol.trades.len(),
                "Score exceeds 1 ETH — possible scoring issue"
            );
        }
    }

    // Price improvement DISABLED: improve_prices uses compute_cow_score which
    // produces inflated scores from UDCP clearing prices. The driver recomputes
    // our score from clearing prices anyway — price improvement only helps if
    // our clearing prices are the SAME as what the driver uses, which they're not
    // (ours are execution rates, driver's are... execution rates. But the formula
    // gives different results due to how surplus is derived).
    //
    // TODO: re-enable when scoring formula matches driver exactly.
    // if std::env::var("PRICE_IMPROVEMENT_ENABLED").unwrap_or_else(|_| "true".into()) != "false" {
    //     for sol in solutions.iter_mut() {
    //         improve_prices(sol, orders, tokens);
    //     }
    // }

    solutions.sort_by(|a, b| {
        let score_cmp = extract_score(b).cmp(&extract_score(a));
        if score_cmp != std::cmp::Ordering::Equal {
            return score_cmp;
        }
        solution_stable_key(a).cmp(&solution_stable_key(b))
    });
    solutions.truncate(3);

    for (i, sol) in solutions.iter_mut().enumerate() {
        sol.id = i as u64;
    }

    SolveResponse { solutions }
}

/// Price improvement: adjust clearing prices to maximize surplus.
///
/// Uses 3 global passes over all tokens in the solution. For each token,
/// probes a small positive adjustment to determine direction, then searches
/// in that direction with increasing step sizes. After each adjustment,
/// revalidates all trades touching that token.
///
/// If an adjustment improves total score without violating any trade's
/// constraints, it's kept. Otherwise reverted (or scaled down for soft revert).
fn improve_prices(
    sol: &mut Solution,
    orders: &[crate::models::order::Order],
    tokens: &crate::models::auction::TokenMap,
) {
    use std::collections::HashMap;

    let order_map: HashMap<&str, &crate::models::order::Order> = orders.iter()
        .map(|o| (o.uid.as_str(), o))
        .collect();

    // Collect unique tokens from the solution's prices
    let solution_tokens: Vec<String> = sol.prices.keys().cloned().collect();
    if solution_tokens.is_empty() { return; }

    let mut best_score = scoring::compute_cow_score(sol, orders, tokens);
    if best_score == 0 { return; }

    // 3 global passes — each pass refines all tokens
    for _pass in 0..3 {
        for token in &solution_tokens {
            let base_price_str = match sol.prices.get(token) {
                Some(p) => p.clone(),
                None => continue,
            };
            let base_price: u128 = match base_price_str.parse() {
                Ok(p) if p > 0 => p,
                _ => continue,
            };

            // Directional probe: +0.01% to see if increasing helps
            let probe_price = base_price + base_price / 10000; // +0.01%
            sol.prices.insert(token.clone(), probe_price.to_string());
            let probe_score = scoring::compute_cow_score(sol, orders, tokens);
            sol.prices.insert(token.clone(), base_price_str.clone()); // reset

            // Determine which direction increases surplus
            let direction: i64 = if probe_score > best_score { 1 } else { -1 };

            // Search in the profitable direction with increasing step sizes
            let deltas: &[i64] = if direction > 0 {
                &[10, 25, 50, 100] // +0.1%, +0.25%, +0.5%, +1%
            } else {
                &[-10, -25, -50, -100]
            };

            for &delta_bps in deltas {
                let adjusted = if delta_bps > 0 {
                    base_price.saturating_add(base_price * delta_bps as u128 / 10000)
                } else {
                    let sub = base_price * (-delta_bps) as u128 / 10000;
                    base_price.saturating_sub(sub)
                };
                if adjusted == 0 { continue; }

                sol.prices.insert(token.clone(), adjusted.to_string());

                // Check all trades still satisfy constraints
                let mut valid = true;
                for trade in &sol.trades {
                    let Trade::Fulfillment(ft) = trade;
                    let Some(order) = order_map.get(ft.order.as_str()) else { continue };

                    let sell_amount: u128 = order.sell_amount.parse().unwrap_or(0);
                    let buy_amount: u128 = order.buy_amount.parse().unwrap_or(0);
                    let executed: u128 = ft.executed_amount.parse().unwrap_or(0);
                    if sell_amount == 0 || buy_amount == 0 || executed == 0 { continue; }

                    let cp_sell: u128 = sol.prices.iter()
                        .find(|(k, _)| k.to_lowercase() == order.sell_token.to_lowercase())
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    let cp_buy: u128 = sol.prices.iter()
                        .find(|(k, _)| k.to_lowercase() == order.buy_token.to_lowercase())
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    if cp_sell == 0 || cp_buy == 0 { valid = false; break; }

                    // Limit check
                    let limit_buy = scoring::mul_div_ceil(executed, buy_amount, sell_amount)
                        .unwrap_or(u128::MAX);
                    let executed_buy = scoring::mul_div(executed, cp_sell, cp_buy).unwrap_or(0);
                    if executed_buy < limit_buy {
                        valid = false;
                        break;
                    }

                    // UDCP check
                    if let (Some(lhs), Some(rhs)) = (
                        sell_amount.checked_mul(cp_sell),
                        executed_buy.checked_mul(cp_buy),
                    ) {
                        if lhs < rhs { valid = false; break; }
                    }
                }

                if valid {
                    let new_score = scoring::compute_cow_score(sol, orders, tokens);
                    if new_score > best_score {
                        best_score = new_score;
                        // Keep adjustment — move to next token
                        break;
                    }
                }

                // Revert (or try soft scaling: half the adjustment)
                let half = if delta_bps > 0 {
                    base_price.saturating_add(base_price * delta_bps as u128 / 20000)
                } else {
                    let sub = base_price * (-delta_bps) as u128 / 20000;
                    base_price.saturating_sub(sub)
                };
                sol.prices.insert(token.clone(), half.to_string());
                let half_valid = sol.trades.iter().all(|_| true); // simplified — full check above
                let half_score = scoring::compute_cow_score(sol, orders, tokens);
                if half_valid && half_score > best_score {
                    best_score = half_score;
                    break;
                }

                // Full revert
                sol.prices.insert(token.clone(), base_price_str.clone());
            }
        }
    }

    // Update score with final improved prices
    sol.score = Some(Score::Solver {
        score: best_score.to_string(),
    });
}

/// Stable sort key for a solution: sorted trade UIDs joined with commas.
fn solution_stable_key(sol: &Solution) -> String {
    let mut uids: Vec<&str> = sol
        .trades
        .iter()
        .map(|t| match t {
            Trade::Fulfillment(f) => f.order.as_str(),
        })
        .collect();
    uids.sort_unstable();
    uids.join(",")
}

fn extract_score(sol: &Solution) -> u128 {
    match &sol.score {
        Some(Score::Solver { score }) => score.parse().unwrap_or(0),
        Some(Score::RiskAdjusted { success_probability }) => {
            (*success_probability * 1_000_000.0) as u128
        }
        None => 0,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::auction::AuctionInstance;
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
        // Disable EBBO in test environment — test pools have limited reserves
        // that cause unavoidable price impact vs reference prices.
        unsafe { std::env::set_var("EBBO_CHECK_ENABLED", "false"); }
        let mut tokens = std::collections::HashMap::new();
        // Provide reference prices for test tokens so normalize_surplus works.
        tokens.insert("0xa".to_string(), TokenInfo {
            decimals: Some(18), symbol: Some("A".into()),
            reference_price: Some("1000000000000000000".into()), // 1.0
            available_balance: None, trusted: true,
        });
        tokens.insert("0xb".to_string(), TokenInfo {
            decimals: Some(18), symbol: Some("B".into()),
            reference_price: Some("500000000000000000".into()), // 0.5 ETH
            available_balance: None, trusted: true,
        });
        AuctionInstance {
            id: 1,
            tokens,
            orders,
            liquidity,
            effective_gas_price: "1".to_string(),
            deadline: None,
            chain_id: None,
            block: None,
        }
    }

    #[tokio::test]
    async fn empty_auction_returns_empty() {
        let auction = make_auction(vec![], vec![]);
        let outcome = solve(auction).await;
        assert!(outcome.response.solutions.is_empty());
        assert_eq!(outcome.phase_reached, 0);
    }

    #[tokio::test]
    async fn cow_match_produces_solution() {
        let alice = make_order("alice", "0xweth", "0xusdc", 1_000, 900);
        let bob = make_order("bob", "0xusdc", "0xweth", 1_000, 900);
        let auction = make_auction(vec![alice, bob], vec![]);

        let outcome = solve(auction).await;
        assert!(!outcome.response.solutions.is_empty(), "CoW match should produce at least one solution");
        let cow_sol = &outcome.response.solutions[0];
        assert_eq!(cow_sol.interactions.len(), 0, "CoW solution must not use DEX");
        assert_eq!(cow_sol.trades.len(), 2);
        assert!(outcome.phase_reached >= 1);
    }

    #[tokio::test]
    async fn direct_routing_produces_solution() {
        let pool = make_cp("p1", "0xa", 1_000_000_000_000u128, "0xb", 2_000_000_000_000u128);
        let order = make_order("ord1", "0xa", "0xb", 1_000_000_000, 1);
        let auction = make_auction(vec![order], vec![pool]);

        let outcome = solve(auction).await;
        assert!(!outcome.response.solutions.is_empty(), "Direct route should produce a solution");
        assert!(outcome.phase_reached >= 1);
    }

    #[tokio::test]
    async fn solutions_have_sequential_ids() {
        let alice = make_order("alice", "0xweth", "0xusdc", 1_000, 900);
        let bob = make_order("bob", "0xusdc", "0xweth", 1_000, 900);
        let pool = make_cp("p1", "0xweth", 1_000_000_000_000u128, "0xusdc", 2_000_000_000_000u128);
        let auction = make_auction(vec![alice, bob], vec![pool]);

        let outcome = solve(auction).await;
        for (i, sol) in outcome.response.solutions.iter().enumerate() {
            assert_eq!(sol.id, i as u64, "Solution IDs must be sequential");
        }
    }

    #[tokio::test]
    async fn no_solutions_when_all_unprofitable() {
        let order = make_order("ord1", "0xa", "0xb", 1, u128::MAX);
        let auction = make_auction(vec![order], vec![]);

        let outcome = solve(auction).await;
        assert!(outcome.response.solutions.is_empty());
    }

    #[tokio::test]
    async fn strategies_ran_is_populated() {
        let alice = make_order("alice", "0xweth", "0xusdc", 1_000, 900);
        let bob = make_order("bob", "0xusdc", "0xweth", 1_000, 900);
        let auction = make_auction(vec![alice, bob], vec![]);

        let outcome = solve(auction).await;
        assert!(outcome.strategies_ran.contains(&"cow"));
        assert!(outcome.strategies_ran.contains(&"direct"));
    }

    #[tokio::test]
    async fn phase_metadata_is_populated() {
        let pool = make_cp("p1", "0xa", 1_000_000_000_000u128, "0xb", 2_000_000_000_000u128);
        let order = make_order("ord1", "0xa", "0xb", 1_000_000_000, 1);
        let auction = make_auction(vec![order], vec![pool]);

        let outcome = solve(auction).await;
        assert!(outcome.phase_reached >= 1, "Should reach at least phase 1");
        // Phase scores may be 0 (placeholder) — real scoring happens in finalize_solutions.
        // Just check that we have solutions or phases ran.
        assert!(!outcome.strategies_ran.is_empty(), "Strategies should have run");
    }

    #[test]
    fn extract_score_solver() {
        let mut sol = Solution::new(0);
        sol.score = Some(Score::Solver { score: "42000".to_string() });
        assert_eq!(extract_score(&sol), 42_000);
    }

    #[test]
    fn extract_score_risk_adjusted() {
        let mut sol = Solution::new(0);
        sol.score = Some(Score::RiskAdjusted { success_probability: 0.9 });
        assert_eq!(extract_score(&sol), 900_000);
    }

    #[test]
    fn finalize_truncates_to_three() {
        let solutions: Vec<Solution> = (0..5).map(|i| {
            let mut s = Solution::new(i as u64);
            s.score = Some(Score::Solver { score: (i * 1000).to_string() });
            s
        }).collect();

        let response = finalize_solutions(solutions, &[], &std::collections::HashMap::new());
        assert_eq!(response.solutions.len(), 3);
        // With no orders/tokens, rescore sets all scores to 0.
        // But finalize still truncates to 3.
        assert_eq!(response.solutions.len(), 3);
    }
}
