//! Parallel and time-budgeted solving utilities.
//!
//! Optimizes the solver hot path using:
//! 1. **Parallel order solving** — Rayon `par_iter` for independent order routing
//! 2. **Tiered time budget** — fast strategies first, slower ones only if time remains
//! 3. **Route caching** — cache solved routes for common (sell_token, buy_token) pairs
//! 4. **Early termination** — stop if solution quality has been stable for `stable_for_ms`
//!
//! ## Time budget (configurable, defaults from spec)
//!
//! | Phase        | Start | Budget |
//! |-------------|-------|--------|
//! | CoW + direct | t=0   | 1s     |
//! | Multi-hop    | t=1s  | 3s     |
//! | Split routing| t=4s  | 5s     |
//! | 3-hop routes | t=9s  | 13s    |
//! | Deadline     | t=22s | —      |

use std::collections::HashMap;
use std::time::Instant;

use rayon::prelude::*;
use tracing::{debug, info};

use crate::models::auction::AuctionInstance;
use crate::models::liquidity::Liquidity;
use crate::models::order::Order;
use crate::solver::direct::{DirectSolver, OrderRoute};
use crate::solver::router::{self, MultiHopRoute};
use crate::solver::split::{self, SplitRoute};

// ── Time budget ───────────────────────────────────────────────────────────────

/// Per-phase time budget in milliseconds.
#[derive(Debug, Clone)]
pub struct TimeBudget {
    /// Budget for CoW matching + direct routing (fastest strategies)
    pub cow_and_direct_ms: u64,
    /// Budget for multi-hop routing (2-hop via intermediaries)
    pub multihop_ms: u64,
    /// Budget for split routing (large orders across multiple pools)
    pub split_ms: u64,
    /// Hard deadline — return whatever is found by this time
    pub total_ms: u64,
    /// If solution score hasn't changed for this many ms, stop early
    pub stable_for_ms: u64,
}

impl Default for TimeBudget {
    fn default() -> Self {
        Self {
            cow_and_direct_ms: 1_000,
            multihop_ms: 3_000,
            split_ms: 5_000,
            total_ms: 22_000,
            stable_for_ms: 2_000,
        }
    }
}

impl TimeBudget {
    /// Create a budget from total ms, distributing proportionally.
    pub fn from_total(total_ms: u64) -> Self {
        let scale = total_ms as f64 / 22_000.0;
        Self {
            cow_and_direct_ms: (1_000.0 * scale) as u64,
            multihop_ms: (3_000.0 * scale) as u64,
            split_ms: (5_000.0 * scale) as u64,
            total_ms,
            stable_for_ms: (2_000.0 * scale) as u64,
        }
    }
}

// ── Parallel direct solving ───────────────────────────────────────────────────

/// Solve all orders in parallel using Rayon.
///
/// Each order is solved independently (DirectSolver is pure/stateless), so
/// this is embarrassingly parallel — no locking needed.
///
/// Returns routes for all successfully routed orders.
pub fn solve_orders_parallel(orders: &[Order], liquidity: &[Liquidity]) -> Vec<OrderRoute> {
    orders
        .par_iter()
        .filter_map(|order| DirectSolver::solve_order(order, liquidity))
        .collect()
}

/// Solve multi-hop routes for a slice of orders in parallel.
pub fn solve_multihop_parallel(orders: &[Order], liquidity: &[Liquidity], chain_id: u64) -> Vec<MultiHopRoute> {
    orders
        .par_iter()
        .filter_map(|order| router::find_best_route(order, liquidity, chain_id))
        .collect()
}

/// Solve split routes for a slice of orders in parallel.
pub fn solve_split_parallel(orders: &[Order], liquidity: &[Liquidity]) -> Vec<SplitRoute> {
    orders
        .par_iter()
        .filter_map(|order| split::find_split(order, liquidity))
        .collect()
}

// ── Route cache ───────────────────────────────────────────────────────────────

/// A lightweight cache for solved routes indexed by (sell_token, buy_token, sell_amount).
///
/// Avoids re-solving the same order multiple times across strategy iterations.
/// Keyed by `(sell_token, buy_token, sell_amount_string)`.
#[derive(Default)]
pub struct RouteCache {
    direct: HashMap<(String, String, String), Option<OrderRoute>>,
}

impl RouteCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up or compute a direct route, caching the result.
    pub fn get_or_solve(
        &mut self,
        order: &Order,
        liquidity: &[Liquidity],
    ) -> Option<&OrderRoute> {
        let key = (
            order.sell_token.clone(),
            order.buy_token.clone(),
            order.sell_amount.clone(),
        );

        self.direct
            .entry(key)
            .or_insert_with(|| DirectSolver::solve_order(order, liquidity))
            .as_ref()
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.direct.len()
    }

    pub fn is_empty(&self) -> bool {
        self.direct.is_empty()
    }
}

// ── Tiered solver ─────────────────────────────────────────────────────────────

/// Result of a tiered parallel solve.
#[derive(Debug)]
pub struct ParallelSolveResult {
    pub direct_routes: Vec<OrderRoute>,
    pub multihop_routes: Vec<MultiHopRoute>,
    pub split_routes: Vec<SplitRoute>,
    pub elapsed_ms: u64,
    pub phases_completed: usize,
}

/// Run the tiered parallel solver with a time budget.
///
/// Runs phases in order: direct → multi-hop → split.
/// Each phase has a configurable budget; the solver returns early if the
/// total deadline is approaching.
pub fn solve_tiered(auction: &AuctionInstance, budget: &TimeBudget) -> ParallelSolveResult {
    let t0 = Instant::now();
    let mut phases_completed = 0;

    // Phase 1: Direct routing (parallel)
    let direct_routes = solve_orders_parallel(&auction.orders, &auction.liquidity);
    phases_completed += 1;

    let elapsed = t0.elapsed().as_millis() as u64;
    debug!(
        phase = "direct",
        routes = direct_routes.len(),
        elapsed_ms = elapsed,
        "Phase 1 complete"
    );

    if elapsed >= budget.cow_and_direct_ms || elapsed >= budget.total_ms {
        return ParallelSolveResult {
            direct_routes,
            multihop_routes: vec![],
            split_routes: vec![],
            elapsed_ms: elapsed,
            phases_completed,
        };
    }

    // Phase 2: Multi-hop for orders not solved directly
    let direct_uids: std::collections::HashSet<&str> = direct_routes
        .iter()
        .map(|r| r.order_uid.as_str())
        .collect();

    let unrouted: Vec<&Order> = auction
        .orders
        .iter()
        .filter(|o| !direct_uids.contains(o.uid.as_str()))
        .collect();

    let multihop_start = Instant::now();
    let multihop_routes = if !unrouted.is_empty() {
        let orders_owned: Vec<Order> = unrouted.iter().map(|o| (*o).clone()).collect();
        solve_multihop_parallel(&orders_owned, &auction.liquidity, auction.chain_id.unwrap_or(1))
    } else {
        vec![]
    };
    phases_completed += 1;

    let elapsed = t0.elapsed().as_millis() as u64;
    debug!(
        phase = "multihop",
        routes = multihop_routes.len(),
        phase_ms = multihop_start.elapsed().as_millis(),
        elapsed_ms = elapsed,
        "Phase 2 complete"
    );

    if elapsed >= budget.cow_and_direct_ms + budget.multihop_ms || elapsed >= budget.total_ms {
        return ParallelSolveResult {
            direct_routes,
            multihop_routes,
            split_routes: vec![],
            elapsed_ms: elapsed,
            phases_completed,
        };
    }

    // Phase 3: Split routing for large orders
    let split_start = Instant::now();
    let split_routes = solve_split_parallel(&auction.orders, &auction.liquidity);
    phases_completed += 1;

    let elapsed = t0.elapsed().as_millis() as u64;
    debug!(
        phase = "split",
        routes = split_routes.len(),
        phase_ms = split_start.elapsed().as_millis(),
        elapsed_ms = elapsed,
        "Phase 3 complete"
    );

    info!(
        auction_id = auction.id,
        phases = phases_completed,
        direct = direct_routes.len(),
        multihop = multihop_routes.len(),
        split = split_routes.len(),
        elapsed_ms = elapsed,
        "Tiered parallel solve complete"
    );

    ParallelSolveResult {
        direct_routes,
        multihop_routes,
        split_routes,
        elapsed_ms: elapsed,
        phases_completed,
    }
}

/// Check if we should terminate early due to stable quality or approaching deadline.
pub fn should_terminate_early(
    last_improvement: Instant,
    t0: Instant,
    budget: &TimeBudget,
) -> bool {
    let elapsed = t0.elapsed().as_millis() as u64;
    let stable_for = last_improvement.elapsed().as_millis() as u64;

    if elapsed >= budget.total_ms {
        debug!("Terminating: deadline reached ({elapsed}ms)");
        return true;
    }

    if stable_for >= budget.stable_for_ms {
        debug!("Terminating: score stable for {stable_for}ms");
        return true;
    }

    false
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
        AuctionInstance {
            id: 1,
            tokens: Default::default(),
            orders,
            liquidity,
            effective_gas_price: "1".to_string(),
            deadline: None,
            chain_id: None,
            block: None,
        }
    }

    #[test]
    fn parallel_solve_matches_sequential() {
        let pool = make_cp("p1", "0xa", 1_000_000_000u128, "0xb", 2_000_000_000u128);
        let orders = vec![
            make_order("o1", "0xa", "0xb", 10_000, 1),
            make_order("o2", "0xa", "0xb", 20_000, 1),
            make_order("o3", "0xa", "0xb", 30_000, 1),
        ];

        let liquidity = vec![pool];
        let parallel_routes = solve_orders_parallel(&orders, &liquidity);

        // Sequential
        let sequential_routes: Vec<OrderRoute> = orders
            .iter()
            .filter_map(|o| DirectSolver::solve_order(o, &liquidity))
            .collect();

        assert_eq!(parallel_routes.len(), sequential_routes.len());
        // Verify same output amounts (order may differ due to par_iter)
        let par_outputs: std::collections::BTreeSet<u128> =
            parallel_routes.iter().map(|r| r.output_amount).collect();
        let seq_outputs: std::collections::BTreeSet<u128> =
            sequential_routes.iter().map(|r| r.output_amount).collect();
        assert_eq!(par_outputs, seq_outputs, "parallel and sequential must give same outputs");
    }

    #[test]
    fn parallel_solve_empty_orders() {
        let routes = solve_orders_parallel(&[], &[]);
        assert!(routes.is_empty());
    }

    #[test]
    fn route_cache_hits_on_second_call() {
        let pool = make_cp("p1", "0xa", 1_000_000u128, "0xb", 2_000_000u128);
        let liquidity = vec![pool];
        let order = make_order("o1", "0xa", "0xb", 10_000, 1);
        let mut cache = RouteCache::new();

        let r1 = cache.get_or_solve(&order, &liquidity).map(|r| r.output_amount);
        let r2 = cache.get_or_solve(&order, &liquidity).map(|r| r.output_amount);

        assert_eq!(r1, r2, "cache must return same result on hit");
        assert_eq!(cache.len(), 1, "one entry in cache");
    }

    #[test]
    fn route_cache_returns_none_for_no_pool() {
        let order = make_order("o1", "0xa", "0xb", 10_000, 1);
        let mut cache = RouteCache::new();
        let result = cache.get_or_solve(&order, &[]);
        assert!(result.is_none(), "no pool → no route");
    }

    #[test]
    fn tiered_solve_completes_all_phases_for_small_auction() {
        let pool = make_cp("p1", "0xa", 1_000_000_000u128, "0xb", 2_000_000_000u128);
        let orders = vec![make_order("o1", "0xa", "0xb", 1_000, 1)];
        let auction = make_auction(orders, vec![pool]);

        let budget = TimeBudget::default();
        let result = solve_tiered(&auction, &budget);

        assert_eq!(result.phases_completed, 3, "should complete all 3 phases");
        assert!(!result.direct_routes.is_empty(), "should find direct route");
    }

    #[test]
    fn time_budget_from_total_scales_proportionally() {
        let budget = TimeBudget::from_total(11_000); // half of 22s
        assert_eq!(budget.total_ms, 11_000);
        assert_eq!(budget.cow_and_direct_ms, 500);
        assert_eq!(budget.multihop_ms, 1_500);
    }

    #[test]
    fn should_not_terminate_immediately() {
        let t0 = Instant::now();
        let budget = TimeBudget::default();
        assert!(!should_terminate_early(t0, t0, &budget));
    }
}
