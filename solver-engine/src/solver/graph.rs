//! Graph-based pathfinding for multi-hop token routing.
//!
//! Builds a directed weighted graph from auction liquidity where:
//! - Nodes = token addresses
//! - Edges = pools (weighted by negative log of exchange rate + gas penalty)
//! - Multiple edges between same token pair (different DEXes)
//! - Shortest path = best exchange rate accounting for gas
//!
//! Supports:
//! - Yen's K-shortest paths algorithm to find top-K best-rate paths
//! - Bellman-Ford for negative cycle (arbitrage) detection
//! - Dynamic intermediary discovery from the liquidity graph
//! - Gas-aware edge weights for accurate surplus estimation

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::Instant;

use petgraph::graph::{DiGraph, EdgeIndex, NodeIndex};
use petgraph::visit::EdgeRef;
use tracing::debug;

use crate::gas;
use crate::models::liquidity::{ConstantProductPool, Liquidity};
use crate::models::order::Order;
use crate::models::solution::{Interaction, LiquidityInteraction};

// ── Configuration ────────────────────────────────────────────────────────────

/// Maximum number of hops (edges) in a path.
pub const MAX_HOPS: usize = 4;

/// Maximum number of candidate paths to return from Yen's algorithm.
pub const TOP_K_PATHS: usize = 10;

/// Reference amount used to estimate exchange rates during graph construction.
/// Using a moderate amount avoids extreme price impact distortion.
const REFERENCE_AMOUNT: u128 = 1_000_000_000; // 1e9

/// Minimum number of pools a token must appear in to be considered a viable intermediary.
const MIN_POOLS_FOR_INTERMEDIARY: usize = 2;

/// Common bridge tokens on Arbitrum. Used for heuristic prioritization.
pub const COMMON_BRIDGES: &[&str] = &[
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", // WETH (mainnet)
    "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1", // WETH (Arbitrum)
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", // USDC (mainnet)
    "0xaf88d065e77c8cC2239327C5EDb3A432268e5831", // USDC (Arbitrum)
    "0xdAC17F958D2ee523a2206206994597C13D831ec7", // USDT (mainnet)
    "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9", // USDT (Arbitrum)
    "0x912CE59144191C1204E64559FE8253a0e49E6548", // ARB
];

// ── Edge metadata ────────────────────────────────────────────────────────────

/// Metadata stored on each edge of the token graph.
#[derive(Debug, Clone)]
pub struct EdgeData {
    /// Pool ID from the auction liquidity array.
    pub pool_id: String,
    /// The source token address (token being sold into this edge).
    pub token_in: String,
    /// The destination token address (token being bought from this edge).
    pub token_out: String,
    /// Combined weight: -ln(exchange_rate) + gas_penalty.
    /// Lower = better rate after gas costs. Sum along path = total cost.
    pub weight: f64,
    /// The estimated exchange rate (amount_out / amount_in) for the reference amount.
    pub rate: f64,
    /// Reserve depth indicator for the input token (used for split routing hints).
    pub reserve_in: u128,
    /// Estimated gas cost for this single hop (in gas units).
    pub gas_units: u64,
    /// Whether this is a V3-style (concentrated liquidity) pool.
    pub is_v3: bool,
}

// ── Path result ──────────────────────────────────────────────────────────────

/// A candidate path through the token graph.
#[derive(Debug, Clone)]
pub struct GraphPath {
    /// Sequence of token addresses: [sell_token, intermediate_1, ..., buy_token].
    pub tokens: Vec<String>,
    /// Edge indices along the path (one per hop).
    pub edges: Vec<EdgeIndex>,
    /// Total weight (sum of edge weights).
    pub total_weight: f64,
    /// Estimated total exchange rate (product of per-hop rates).
    pub estimated_rate: f64,
    /// Total estimated gas cost in gas units for all hops.
    pub total_gas_units: u64,
}

/// A fully simulated route from graph pathfinding, ready for solution assembly.
#[derive(Debug, Clone)]
pub struct GraphRoute {
    pub order_uid: String,
    pub path: GraphPath,
    pub executed_amount: u128,
    pub output_amount: u128,
    pub surplus: u128,
    pub gas_cost_wei: u128,
    pub net_surplus: i128,
    pub interactions: Vec<Interaction>,
}

// ── Token Graph ──────────────────────────────────────────────────────────────

/// Directed weighted graph of token exchange rates built from auction liquidity.
pub struct TokenGraph {
    /// The underlying petgraph directed graph.
    /// Node weight = token address, Edge weight = EdgeData.
    graph: DiGraph<String, EdgeData>,
    /// Map from token address to node index for fast lookup.
    token_to_node: HashMap<String, NodeIndex>,
    /// Effective gas price from the auction (wei per gas unit).
    gas_price_wei: u128,
    /// Chain ID (affects gas cost model).
    chain_id: u64,
    /// Time taken to construct the graph.
    pub construction_time_ms: u128,
}

impl TokenGraph {
    /// Build a token graph from the auction's liquidity array.
    ///
    /// For each pool, adds edges in both directions between the pool's tokens,
    /// weighted by the negative log of the simulated exchange rate plus a gas penalty.
    ///
    /// Gas penalty = (gas_cost_wei / reference_amount_value) expressed as a rate penalty,
    /// so paths with high gas are penalized proportionally.
    pub fn build(liquidity: &[Liquidity], gas_price_wei: u128, chain_id: u64) -> Self {
        let t0 = Instant::now();
        let mut graph = DiGraph::new();
        let mut token_to_node: HashMap<String, NodeIndex> = HashMap::new();

        // Helper: get or create node for a token
        // Normalize all token addresses to lowercase for consistent lookups.
        // CoW driver may send checksummed addresses while our constants are lowercase.
        let get_node = |graph: &mut DiGraph<String, EdgeData>,
                            map: &mut HashMap<String, NodeIndex>,
                            token: &str|
         -> NodeIndex {
            let normalized = token.to_lowercase();
            if let Some(&idx) = map.get(&normalized) {
                idx
            } else {
                let idx = graph.add_node(normalized.clone());
                map.insert(normalized, idx);
                idx
            }
        };

        for pool in liquidity {
            match pool {
                Liquidity::ConstantProduct(p)
                | Liquidity::WeightedProduct(p)
                | Liquidity::Stable(p) => {
                    let tokens: Vec<&String> = p.tokens.keys().collect();
                    if tokens.len() < 2 {
                        continue;
                    }
                    for i in 0..tokens.len() {
                        for j in 0..tokens.len() {
                            if i == j {
                                continue;
                            }
                            let token_in = tokens[i];
                            let token_out = tokens[j];

                            if let Some(edge_data) =
                                compute_cp_edge(p, token_in, token_out, gas_price_wei, chain_id)
                            {
                                let node_in =
                                    get_node(&mut graph, &mut token_to_node, token_in);
                                let node_out =
                                    get_node(&mut graph, &mut token_to_node, token_out);
                                graph.add_edge(node_in, node_out, edge_data);
                            }
                        }
                    }
                }
                Liquidity::ConcentratedLiquidity(p) => {
                    if p.tokens.len() < 2 {
                        continue;
                    }
                    for i in 0..p.tokens.len() {
                        for j in 0..p.tokens.len() {
                            if i == j {
                                continue;
                            }
                            let token_in = &p.tokens[i];
                            let token_out = &p.tokens[j];

                            if let Some(edge_data) =
                                compute_cl_edge(p, token_in, token_out, gas_price_wei, chain_id)
                            {
                                let node_in =
                                    get_node(&mut graph, &mut token_to_node, token_in);
                                let node_out =
                                    get_node(&mut graph, &mut token_to_node, token_out);
                                graph.add_edge(node_in, node_out, edge_data);
                            }
                        }
                    }
                }
            }
        }

        let construction_time_ms = t0.elapsed().as_millis();
        debug!(
            nodes = graph.node_count(),
            edges = graph.edge_count(),
            time_ms = construction_time_ms,
            "Token graph constructed"
        );

        Self {
            graph,
            token_to_node,
            gas_price_wei,
            chain_id,
            construction_time_ms,
        }
    }

    /// Convenience builder that defaults to zero gas price (backward compatibility).
    pub fn build_no_gas(liquidity: &[Liquidity]) -> Self {
        Self::build(liquidity, 0, 1)
    }

    /// Return the number of tokens (nodes) in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Return the number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Check if a token exists in the graph (has at least one pool).
    pub fn has_token(&self, token: &str) -> bool {
        self.token_to_node.contains_key(&token.to_lowercase())
    }

    /// Get all tokens that appear in at least `MIN_POOLS_FOR_INTERMEDIARY` pools.
    /// These are viable intermediary tokens for multi-hop routing.
    pub fn viable_intermediaries(&self) -> Vec<String> {
        self.token_to_node
            .iter()
            .filter_map(|(token, &node)| {
                // Count distinct pool IDs this token participates in
                let pool_ids: HashSet<&str> = self
                    .graph
                    .edges(node)
                    .map(|e| e.weight().pool_id.as_str())
                    .collect();
                if pool_ids.len() >= MIN_POOLS_FOR_INTERMEDIARY {
                    Some(token.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find the top-K best-rate paths from `sell_token` to `buy_token`
    /// using Yen's K-shortest paths algorithm.
    ///
    /// Returns paths sorted by total weight (best first).
    pub fn find_paths(&self, sell_token: &str, buy_token: &str) -> Vec<GraphPath> {
        let t0 = Instant::now();

        let src = match self.token_to_node.get(sell_token) {
            Some(&n) => n,
            None => return vec![],
        };
        let dst = match self.token_to_node.get(buy_token) {
            Some(&n) => n,
            None => return vec![],
        };

        if src == dst {
            return vec![];
        }

        let paths = self.yen_k_shortest(src, dst, TOP_K_PATHS, MAX_HOPS);

        let elapsed_ms = t0.elapsed().as_millis();
        debug!(
            sell_token = sell_token,
            buy_token = buy_token,
            paths_found = paths.len(),
            time_ms = elapsed_ms,
            "Graph pathfinding complete (Yen's K={TOP_K_PATHS})"
        );

        paths
    }

    /// Simulate a full route along a graph path for a specific order amount.
    ///
    /// Walks each edge, simulating the actual swap output at each hop using
    /// the real pool math (not just the graph weights which use a reference amount).
    /// Also computes the gas cost and net surplus (output - limit - gas).
    pub fn simulate_path(
        &self,
        path: &GraphPath,
        order: &Order,
        liquidity: &[Liquidity],
    ) -> Option<GraphRoute> {
        let sell_amount: u128 = order.sell_amount.parse().ok()?;
        let buy_amount_min: u128 = order.buy_amount.parse().ok()?;

        if sell_amount == 0 {
            return None;
        }

        let mut current_amount = sell_amount;
        let mut interactions = Vec::new();
        let mut total_gas_units: u64 = 0;

        for (hop_idx, &edge_idx) in path.edges.iter().enumerate() {
            let edge = self.graph.edge_weight(edge_idx)?;

            // Find the pool in the liquidity array and simulate the swap
            let pool = liquidity.iter().find(|l| l.id() == edge.pool_id)?;
            let output = simulate_pool_swap(pool, &edge.token_in, &edge.token_out, current_amount)?;

            if output == 0 {
                return None;
            }

            total_gas_units += edge.gas_units;

            interactions.push(Interaction::Liquidity(LiquidityInteraction {
                internalize: false,
                id: edge.pool_id.clone(),
                input_token: edge.token_in.clone(),
                output_token: edge.token_out.clone(),
                input_amount: current_amount.to_string(),
                output_amount: output.to_string(),
            }));

            current_amount = output;

            // Safety: bail if we somehow got zero mid-path
            if current_amount == 0 && hop_idx < path.edges.len() - 1 {
                return None;
            }
        }

        if current_amount < buy_amount_min {
            return None;
        }

        let surplus = current_amount.saturating_sub(buy_amount_min);

        // Compute gas cost in wei
        let gas_cost_wei = gas::estimate_cost_wei(
            self.chain_id,
            path.edges.len(),
            path.edges.iter().any(|&ei| {
                self.graph
                    .edge_weight(ei)
                    .map_or(false, |e| e.is_v3)
            }),
            self.gas_price_wei,
        );

        // Net surplus = surplus - gas cost (can be negative)
        let net_surplus = surplus as i128 - gas_cost_wei as i128;

        Some(GraphRoute {
            order_uid: order.uid.clone(),
            path: GraphPath {
                total_gas_units,
                ..path.clone()
            },
            executed_amount: sell_amount,
            output_amount: current_amount,
            surplus,
            gas_cost_wei,
            net_surplus,
            interactions,
        })
    }

    /// Simulate a partial amount through a path (for split routing).
    ///
    /// Returns (output_amount, interactions) or None if the path can't handle this amount.
    pub fn simulate_path_partial(
        &self,
        path: &GraphPath,
        _sell_token: &str,
        _buy_token: &str,
        amount_in: u128,
        liquidity: &[Liquidity],
    ) -> Option<(u128, Vec<Interaction>)> {
        if amount_in == 0 {
            return None;
        }

        let mut current_amount = amount_in;
        let mut interactions = Vec::new();

        for &edge_idx in &path.edges {
            let edge = self.graph.edge_weight(edge_idx)?;
            let pool = liquidity.iter().find(|l| l.id() == edge.pool_id)?;
            let output = simulate_pool_swap(pool, &edge.token_in, &edge.token_out, current_amount)?;

            if output == 0 {
                return None;
            }

            interactions.push(Interaction::Liquidity(LiquidityInteraction {
                internalize: false,
                id: edge.pool_id.clone(),
                input_token: edge.token_in.clone(),
                output_token: edge.token_out.clone(),
                input_amount: current_amount.to_string(),
                output_amount: output.to_string(),
            }));

            current_amount = output;
        }

        Some((current_amount, interactions))
    }

    /// Compute the marginal exchange rate for a given path and input amount.
    ///
    /// This is the derivative of output w.r.t. input at the given amount,
    /// approximated by finite difference: (f(x+dx) - f(x-dx)) / (2*dx).
    /// Used by split routing to equalize marginal prices across routes.
    pub fn marginal_rate(
        &self,
        path: &GraphPath,
        amount_in: u128,
        liquidity: &[Liquidity],
    ) -> Option<f64> {
        if amount_in == 0 {
            return None;
        }

        // Use 0.1% perturbation for finite difference
        let dx = (amount_in / 1000).max(1);
        let x_lo = amount_in.saturating_sub(dx);
        let x_hi = amount_in.saturating_add(dx);

        let simulate = |amt: u128| -> Option<u128> {
            let mut current = amt;
            for &edge_idx in &path.edges {
                let edge = self.graph.edge_weight(edge_idx)?;
                let pool = liquidity.iter().find(|l| l.id() == edge.pool_id)?;
                current = simulate_pool_swap(pool, &edge.token_in, &edge.token_out, current)?;
                if current == 0 {
                    return None;
                }
            }
            Some(current)
        };

        let y_lo = simulate(x_lo)?;
        let y_hi = simulate(x_hi)?;

        let actual_dx = (x_hi - x_lo) as f64;
        if actual_dx == 0.0 {
            return None;
        }

        Some((y_hi as f64 - y_lo as f64) / actual_dx)
    }

    /// Get the gas cost in wei for a single hop on a given edge.
    pub fn edge_gas_cost_wei(&self, edge_idx: EdgeIndex) -> u128 {
        let edge = match self.graph.edge_weight(edge_idx) {
            Some(e) => e,
            None => return 0,
        };
        edge.gas_units as u128 * self.gas_price_wei
    }

    /// Detect negative cycles in the graph using Bellman-Ford.
    ///
    /// A negative cycle indicates an arbitrage opportunity: a sequence of swaps
    /// that produces more tokens than you started with.
    ///
    /// Returns the tokens involved in the cycle, if any.
    pub fn detect_negative_cycles(&self) -> Option<Vec<String>> {
        let n = self.graph.node_count();
        if n == 0 {
            return None;
        }

        let mut dist: HashMap<NodeIndex, f64> = HashMap::new();
        let mut pred: HashMap<NodeIndex, (NodeIndex, EdgeIndex)> = HashMap::new();

        for &node in self.token_to_node.values() {
            dist.insert(node, 0.0);
        }

        let edges: Vec<_> = self
            .graph
            .edge_indices()
            .filter_map(|ei| {
                let (src, dst) = self.graph.edge_endpoints(ei)?;
                let w = &self.graph[ei];
                Some((ei, src, dst, w.weight))
            })
            .collect();

        for _ in 0..n.saturating_sub(1) {
            let mut changed = false;
            for &(ei, src, dst, weight) in &edges {
                let d_src = match dist.get(&src) {
                    Some(&d) => d,
                    None => continue,
                };
                let new_dist = d_src + weight;
                let d_dst = dist.get(&dst).copied().unwrap_or(f64::INFINITY);
                if new_dist < d_dst - 1e-12 {
                    dist.insert(dst, new_dist);
                    pred.insert(dst, (src, ei));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // Check for negative cycle (one more relaxation pass)
        for &(_ei, src, dst, weight) in &edges {
            let d_src = match dist.get(&src) {
                Some(&d) => d,
                None => continue,
            };
            let new_dist = d_src + weight;
            let d_dst = dist.get(&dst).copied().unwrap_or(f64::INFINITY);
            if new_dist < d_dst - 1e-12 {
                let mut cycle_tokens = Vec::new();
                let mut visited = HashSet::new();
                let mut current = dst;

                for _ in 0..n {
                    if let Some(&(prev, _)) = pred.get(&current) {
                        current = prev;
                    } else {
                        break;
                    }
                }

                let cycle_start = current;
                loop {
                    let token = self.graph[current].clone();
                    if visited.contains(&current) && current == cycle_start && !cycle_tokens.is_empty()
                    {
                        cycle_tokens.push(token);
                        break;
                    }
                    visited.insert(current);
                    cycle_tokens.push(token);
                    if let Some(&(prev, _)) = pred.get(&current) {
                        current = prev;
                    } else {
                        break;
                    }
                }

                if cycle_tokens.len() > 1 {
                    debug!(
                        cycle_len = cycle_tokens.len(),
                        "Negative cycle (arbitrage opportunity) detected"
                    );
                    return Some(cycle_tokens);
                }
            }
        }

        None
    }

    // ── Internal: Yen's K-shortest paths ─────────────────────────────────────

    /// Find shortest path from src to dst using Dijkstra with hop limit.
    /// Returns None if no path exists within max_hops.
    fn dijkstra_shortest(
        &self,
        src: NodeIndex,
        dst: NodeIndex,
        max_hops: usize,
        blocked_edges: &HashSet<EdgeIndex>,
        blocked_nodes: &HashSet<NodeIndex>,
    ) -> Option<GraphPath> {
        #[derive(Clone)]
        struct State {
            neg_weight: OrderedFloat,
            node: NodeIndex,
            tokens: Vec<String>,
            edges: Vec<EdgeIndex>,
            weight: f64,
            gas_units: u64,
        }

        impl Eq for State {}
        impl PartialEq for State {
            fn eq(&self, other: &Self) -> bool {
                self.neg_weight == other.neg_weight
            }
        }
        impl Ord for State {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.neg_weight.cmp(&other.neg_weight)
            }
        }
        impl PartialOrd for State {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        let mut heap = BinaryHeap::new();
        // Track best known distance per (node, hop_count) to prune worse paths
        let mut best_dist: HashMap<(NodeIndex, usize), f64> = HashMap::new();

        heap.push(State {
            neg_weight: OrderedFloat(-0.0),
            node: src,
            tokens: vec![self.graph[src].clone()],
            edges: vec![],
            weight: 0.0,
            gas_units: 0,
        });

        while let Some(state) = heap.pop() {
            if state.node == dst && !state.edges.is_empty() {
                let estimated_rate = (-state.weight).exp();
                return Some(GraphPath {
                    tokens: state.tokens,
                    edges: state.edges,
                    total_weight: state.weight,
                    estimated_rate,
                    total_gas_units: state.gas_units,
                });
            }

            let hops = state.edges.len();
            if hops >= max_hops {
                continue;
            }

            // Prune if we've seen a better path to this (node, hop_count)
            let key = (state.node, hops);
            if let Some(&prev_best) = best_dist.get(&key) {
                if state.weight > prev_best + 1e-12 {
                    continue;
                }
            }
            best_dist.insert(key, state.weight);

            for edge in self.graph.edges(state.node) {
                let edge_id = edge.id();
                let next_node = edge.target();

                if blocked_edges.contains(&edge_id) {
                    continue;
                }
                if next_node != dst && blocked_nodes.contains(&next_node) {
                    continue;
                }

                // Prevent revisiting nodes in this path (no loops except reaching dst)
                let already_visited = state.tokens.iter().any(|t| *t == self.graph[next_node]);
                if already_visited && next_node != dst {
                    continue;
                }

                let edge_data = edge.weight();
                let new_weight = state.weight + edge_data.weight;

                let mut new_tokens = state.tokens.clone();
                new_tokens.push(self.graph[next_node].clone());

                let mut new_edges = state.edges.clone();
                new_edges.push(edge_id);

                heap.push(State {
                    neg_weight: OrderedFloat(-new_weight),
                    node: next_node,
                    tokens: new_tokens,
                    edges: new_edges,
                    weight: new_weight,
                    gas_units: state.gas_units + edge_data.gas_units,
                });
            }
        }

        None
    }

    /// Yen's K-shortest loopless paths algorithm.
    ///
    /// Finds the K shortest simple (loopless) paths from src to dst.
    /// This is the real Yen's algorithm: after finding each shortest path,
    /// it systematically explores deviations to find the next shortest.
    fn yen_k_shortest(
        &self,
        src: NodeIndex,
        dst: NodeIndex,
        k: usize,
        max_hops: usize,
    ) -> Vec<GraphPath> {
        let blocked_edges = HashSet::new();
        let blocked_nodes = HashSet::new();

        // Step 1: Find the shortest path (A[0])
        let first = match self.dijkstra_shortest(src, dst, max_hops, &blocked_edges, &blocked_nodes)
        {
            Some(p) => p,
            None => return vec![],
        };

        let mut a_paths: Vec<GraphPath> = vec![first];
        let mut b_candidates: Vec<GraphPath> = Vec::new();

        for k_idx in 1..k {
            let prev_path = &a_paths[k_idx - 1];

            // For each spur node in the previous shortest path
            for spur_idx in 0..prev_path.edges.len() {
                let spur_node = self
                    .token_to_node
                    .get(&prev_path.tokens[spur_idx])
                    .copied();
                let spur_node = match spur_node {
                    Some(n) => n,
                    None => continue,
                };

                // Root path = prev_path[0..spur_idx]
                let root_tokens = &prev_path.tokens[..=spur_idx];
                let root_edges = &prev_path.edges[..spur_idx];
                let root_weight: f64 = root_edges
                    .iter()
                    .filter_map(|&ei| self.graph.edge_weight(ei).map(|e| e.weight))
                    .sum();

                // Block edges that share the same root path prefix in previously found paths
                let mut spur_blocked_edges = HashSet::new();
                for existing_path in &a_paths {
                    if existing_path.tokens.len() > spur_idx
                        && existing_path.tokens[..=spur_idx] == *root_tokens
                    {
                        if let Some(&blocked_edge) = existing_path.edges.get(spur_idx) {
                            spur_blocked_edges.insert(blocked_edge);
                        }
                    }
                }

                // Block intermediate nodes in the root path (not src or spur)
                let mut spur_blocked_nodes = HashSet::new();
                for i in 0..spur_idx {
                    if let Some(&n) = self.token_to_node.get(&prev_path.tokens[i]) {
                        if n != src && n != spur_node {
                            spur_blocked_nodes.insert(n);
                        }
                    }
                }

                // Find spur path from spur_node to dst
                if let Some(spur_path) = self.dijkstra_shortest(
                    spur_node,
                    dst,
                    max_hops.saturating_sub(spur_idx),
                    &spur_blocked_edges,
                    &spur_blocked_nodes,
                ) {
                    // Combine root + spur
                    let mut combined_tokens = root_tokens.to_vec();
                    combined_tokens.extend_from_slice(&spur_path.tokens[1..]); // skip spur_node (already in root)

                    let mut combined_edges = root_edges.to_vec();
                    combined_edges.extend_from_slice(&spur_path.edges);

                    if combined_edges.len() > max_hops {
                        continue;
                    }

                    let total_weight = root_weight + spur_path.total_weight;
                    let estimated_rate = (-total_weight).exp();
                    let total_gas: u64 = combined_edges
                        .iter()
                        .filter_map(|&ei| self.graph.edge_weight(ei).map(|e| e.gas_units))
                        .sum();

                    let candidate = GraphPath {
                        tokens: combined_tokens,
                        edges: combined_edges,
                        total_weight,
                        estimated_rate,
                        total_gas_units: total_gas,
                    };

                    // Only add if we haven't seen this exact path
                    let is_dup = a_paths.iter().any(|p| p.edges == candidate.edges)
                        || b_candidates.iter().any(|p| p.edges == candidate.edges);

                    if !is_dup {
                        b_candidates.push(candidate);
                    }
                }
            }

            if b_candidates.is_empty() {
                break;
            }

            // Pick the best candidate from B
            b_candidates.sort_by(|a, b| {
                a.total_weight
                    .partial_cmp(&b.total_weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let best = b_candidates.remove(0);
            a_paths.push(best);
        }

        a_paths
    }
}

// ── OrderedFloat wrapper for BinaryHeap ──────────────────────────────────────

/// Wrapper around f64 that implements Ord for use in BinaryHeap.
/// NaN is treated as greater than all other values (pushed to the end).
#[derive(Clone, Copy, Debug)]
struct OrderedFloat(f64);

impl PartialEq for OrderedFloat {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == std::cmp::Ordering::Equal
    }
}

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // For max-heap: we store negative weights, so larger (less negative) = better
        self.0.total_cmp(&other.0)
    }
}

// ── Pool simulation helpers ──────────────────────────────────────────────────

/// Compute the gas penalty to add to an edge weight.
///
/// The idea: if gas costs X wei per hop, and we're routing REFERENCE_AMOUNT,
/// then the effective rate loss is gas_cost / reference_value.
/// We add -ln(1 - gas_cost/ref_value) to the weight, which penalizes gas-heavy paths.
fn gas_weight_penalty(gas_units: u64, gas_price_wei: u128) -> f64 {
    if gas_price_wei == 0 || gas_units == 0 {
        return 0.0;
    }

    let gas_cost = gas_units as f64 * gas_price_wei as f64;
    // Treat REFERENCE_AMOUNT as being worth 1e18 wei for normalization
    // (since we don't know the token price here, this is an approximation)
    let ref_value = REFERENCE_AMOUNT as f64;

    // Penalty: how much the gas cost degrades the effective rate
    // Small penalty for cheap gas, large for expensive gas
    let ratio = gas_cost / (ref_value * 1e9); // normalize to reasonable range
    if ratio >= 1.0 {
        return f64::INFINITY; // Gas cost exceeds trade value
    }

    // -ln(1 - ratio) approximation: for small ratio, this is ~ratio
    -(1.0 - ratio).ln()
}

/// Compute edge data for a constant-product pool edge (token_in -> token_out).
fn compute_cp_edge(
    pool: &ConstantProductPool,
    token_in: &str,
    token_out: &str,
    gas_price_wei: u128,
    _chain_id: u64,
) -> Option<EdgeData> {
    let reserve_in = pool.tokens.get(token_in)?.balance.parse::<u128>().ok()?;
    let reserve_out = pool.tokens.get(token_out)?.balance.parse::<u128>().ok()?;

    if reserve_in == 0 || reserve_out == 0 {
        return None;
    }

    let fee: f64 = pool.fee.parse().unwrap_or(0.003);
    let fee_bps = (fee * 10_000.0).round() as u128;
    let fee_mult = 10_000u128.checked_sub(fee_bps)?;

    // Simulate swap of REFERENCE_AMOUNT
    let amount_in = REFERENCE_AMOUNT.min(reserve_in / 10);
    if amount_in == 0 {
        return None;
    }

    let aiw = amount_in.checked_mul(fee_mult)?;
    let num = aiw.checked_mul(reserve_out)?;
    let den = reserve_in.checked_mul(10_000)?.checked_add(aiw)?;
    let amount_out = num.checked_div(den)?;

    if amount_out == 0 {
        return None;
    }

    let rate = amount_out as f64 / amount_in as f64;
    let rate_weight = -(rate.ln());

    let gas_units = gas::GAS_UNISWAP_V2_SWAP;
    let gas_penalty = gas_weight_penalty(gas_units, gas_price_wei);
    let weight = rate_weight + gas_penalty;

    Some(EdgeData {
        pool_id: pool.id.clone(),
        token_in: token_in.to_string(),
        token_out: token_out.to_string(),
        weight,
        rate,
        reserve_in,
        gas_units,
        is_v3: false,
    })
}

/// Compute edge data for a concentrated liquidity pool edge.
fn compute_cl_edge(
    pool: &crate::models::liquidity::ConcentratedLiquidityPool,
    token_in: &str,
    token_out: &str,
    gas_price_wei: u128,
    _chain_id: u64,
) -> Option<EdgeData> {
    let sqrt_price: u128 = pool.sqrt_price.parse().ok()?;
    let liquidity: u128 = pool.liquidity.parse().ok()?;
    let fee: f64 = pool.fee.parse().unwrap_or(0.0005);
    let fee_micro = (fee * 1_000_000.0).round() as u32;

    if sqrt_price == 0 || liquidity == 0 {
        return None;
    }

    let zero_for_one = token_in.to_lowercase() == pool.token0().to_lowercase();

    let amount_in = REFERENCE_AMOUNT;
    let amount_out = crate::liquidity::uniswap_v3::get_amount_out_approx(
        sqrt_price, liquidity, amount_in, zero_for_one, fee_micro,
    )?;

    if amount_out == 0 {
        return None;
    }

    let rate = amount_out as f64 / amount_in as f64;
    let rate_weight = -(rate.ln());

    let gas_units = gas::GAS_UNISWAP_V3_SWAP;
    let gas_penalty = gas_weight_penalty(gas_units, gas_price_wei);
    let weight = rate_weight + gas_penalty;

    // V3 pools don't store balances; use liquidity as a reserve proxy
    let reserve_in = liquidity;

    Some(EdgeData {
        pool_id: pool.id.clone(),
        token_in: token_in.to_string(),
        token_out: token_out.to_string(),
        weight,
        rate,
        reserve_in,
        gas_units,
        is_v3: true,
    })
}

/// Simulate an actual swap through a pool (not using reference amounts).
pub fn simulate_pool_swap(
    pool: &Liquidity,
    token_in: &str,
    token_out: &str,
    amount_in: u128,
) -> Option<u128> {
    match pool {
        Liquidity::ConstantProduct(p) | Liquidity::WeightedProduct(p) | Liquidity::Stable(p) => {
            let reserve_in = p.tokens.get(token_in)?.balance.parse::<u128>().ok()?;
            let reserve_out = p.tokens.get(token_out)?.balance.parse::<u128>().ok()?;
            if reserve_in == 0 || reserve_out == 0 {
                return None;
            }
            let fee: f64 = p.fee.parse().unwrap_or(0.003);
            let fee_bps = (fee * 10_000.0).round() as u128;
            let fee_mult = 10_000u128.checked_sub(fee_bps)?;
            let aiw = amount_in.checked_mul(fee_mult)?;
            let num = aiw.checked_mul(reserve_out)?;
            let den = reserve_in.checked_mul(10_000)?.checked_add(aiw)?;
            num.checked_div(den)
        }
        Liquidity::ConcentratedLiquidity(p) => {
            let sqrt_price: u128 = p.sqrt_price.parse().ok()?;
            let liquidity: u128 = p.liquidity.parse().ok()?;
            let fee: f64 = p.fee.parse().unwrap_or(0.0005);
            let fee_micro = (fee * 1_000_000.0).round() as u32;
            let zero_for_one = token_in.to_lowercase() == p.token0().to_lowercase();
            crate::liquidity::uniswap_v3::get_amount_out_approx(
                sqrt_price, liquidity, amount_in, zero_for_one, fee_micro,
            )
        }
    }
}

// ── Public helper: find best graph route for an order ────────────────────────

/// Find the best route for an order using graph-based pathfinding.
///
/// Builds the token graph, finds top-K paths via Yen's algorithm, simulates
/// each with the real order amount, and returns the route with the highest
/// net surplus (surplus - gas cost).
pub fn find_best_graph_route(
    order: &Order,
    liquidity: &[Liquidity],
    graph: &TokenGraph,
) -> Option<GraphRoute> {
    let paths = graph.find_paths(&order.sell_token, &order.buy_token);

    if paths.is_empty() {
        return None;
    }

    let mut best: Option<GraphRoute> = None;

    for path in &paths {
        if let Some(route) = graph.simulate_path(path, order, liquidity) {
            // Rank by net surplus (surplus - gas) to pick truly profitable routes
            let is_better = best
                .as_ref()
                .is_none_or(|b| route.net_surplus > b.net_surplus);
            if is_better && route.net_surplus > 0 {
                debug!(
                    order_uid = %order.uid,
                    hops = path.edges.len(),
                    surplus = route.surplus,
                    gas_cost = route.gas_cost_wei,
                    net_surplus = route.net_surplus,
                    "Graph route found"
                );
                best = Some(route);
            }
        }
    }

    best
}

/// Find the top N graph routes for an order, ranked by net surplus.
///
/// Returns up to `n` routes with positive net surplus. Used by split routing
/// to have multiple candidate routes to split across.
pub fn find_top_n_graph_routes(
    order: &Order,
    liquidity: &[Liquidity],
    graph: &TokenGraph,
    n: usize,
) -> Vec<GraphRoute> {
    let paths = graph.find_paths(&order.sell_token, &order.buy_token);

    let mut routes: Vec<GraphRoute> = paths
        .iter()
        .filter_map(|path| graph.simulate_path(path, order, liquidity))
        .filter(|route| route.net_surplus > 0)
        .collect();

    // Sort by net surplus descending
    routes.sort_by(|a, b| b.net_surplus.cmp(&a.net_surplus));
    routes.truncate(n);
    routes
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

    fn make_cp_pool(id: &str, t0: &str, r0: u128, t1: &str, r1: u128) -> Liquidity {
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
    fn graph_construction_from_liquidity() {
        let pools = vec![
            make_cp_pool("p1", "0xa", 1_000_000_000, "0xb", 2_000_000_000),
            make_cp_pool("p2", "0xb", 2_000_000_000, "0xc", 3_000_000_000),
            make_cp_pool("p3", "0xa", 1_000_000_000, "0xc", 3_000_000_000),
        ];

        let graph = TokenGraph::build_no_gas(&pools);
        assert_eq!(graph.node_count(), 3); // a, b, c
        assert_eq!(graph.edge_count(), 6); // 3 pools * 2 directions each
    }

    #[test]
    fn direct_path_found() {
        let pools = vec![make_cp_pool(
            "p1",
            "0xa",
            1_000_000_000,
            "0xb",
            2_000_000_000,
        )];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());
        assert_eq!(paths[0].tokens.len(), 2); // [0xa, 0xb]
        assert_eq!(paths[0].edges.len(), 1);
    }

    #[test]
    fn two_hop_path_found() {
        let pools = vec![
            make_cp_pool("p1", "0xa", 1_000_000_000, "0xmid", 1_000_000_000),
            make_cp_pool("p2", "0xmid", 1_000_000_000, "0xb", 1_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());
        let two_hop = paths.iter().find(|p| p.edges.len() == 2);
        assert!(two_hop.is_some());
        let p = two_hop.unwrap();
        assert_eq!(p.tokens, vec!["0xa", "0xmid", "0xb"]);
    }

    #[test]
    fn three_hop_path_found() {
        let pools = vec![
            make_cp_pool("p1", "0xa", 1_000_000_000, "0xm1", 1_000_000_000),
            make_cp_pool("p2", "0xm1", 1_000_000_000, "0xm2", 1_000_000_000),
            make_cp_pool("p3", "0xm2", 1_000_000_000, "0xb", 1_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());
        let three_hop = paths.iter().find(|p| p.edges.len() == 3);
        assert!(three_hop.is_some());
    }

    #[test]
    fn no_path_returns_empty() {
        let pools = vec![make_cp_pool(
            "p1",
            "0xa",
            1_000_000_000,
            "0xb",
            2_000_000_000,
        )];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xc"); // No pool for c
        assert!(paths.is_empty());
    }

    #[test]
    fn best_path_has_highest_rate() {
        let pools = vec![
            make_cp_pool("direct", "0xa", 100_000, "0xb", 50_000),
            make_cp_pool("leg1", "0xa", 10_000_000_000, "0xmid", 10_000_000_000),
            make_cp_pool("leg2", "0xmid", 10_000_000_000, "0xb", 10_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(paths.len() >= 2);
        let weights: Vec<f64> = paths.iter().map(|p| p.total_weight).collect();
        for i in 1..weights.len() {
            assert!(
                weights[i] >= weights[i - 1] - 1e-12,
                "Paths must be sorted by weight"
            );
        }
    }

    #[test]
    fn yen_finds_multiple_distinct_paths() {
        // Diamond graph: a->b via two 2-hop paths (mid1, mid2) plus direct
        let pools = vec![
            make_cp_pool("direct", "0xa", 1_000_000_000, "0xb", 2_000_000_000),
            make_cp_pool("leg1a", "0xa", 1_000_000_000, "0xm1", 1_000_000_000),
            make_cp_pool("leg1b", "0xm1", 1_000_000_000, "0xb", 1_000_000_000),
            make_cp_pool("leg2a", "0xa", 1_000_000_000, "0xm2", 1_500_000_000),
            make_cp_pool("leg2b", "0xm2", 1_500_000_000, "0xb", 1_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(
            paths.len() >= 3,
            "Should find at least 3 distinct paths: direct + 2 two-hop, got {}",
            paths.len()
        );

        // Verify all paths are distinct (different edge sets)
        for i in 0..paths.len() {
            for j in (i + 1)..paths.len() {
                assert_ne!(
                    paths[i].edges, paths[j].edges,
                    "Paths {i} and {j} should be distinct"
                );
            }
        }
    }

    #[test]
    fn simulate_path_produces_valid_route() {
        let pools = vec![
            make_cp_pool("p1", "0xa", 1_000_000_000, "0xmid", 2_000_000_000),
            make_cp_pool("p2", "0xmid", 2_000_000_000, "0xb", 3_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());

        let order = make_order("uid1", "0xa", "0xb", 1_000, 1);
        let route = graph.simulate_path(&paths[0], &order, &pools);
        assert!(route.is_some());

        let r = route.unwrap();
        assert_eq!(r.order_uid, "uid1");
        assert_eq!(r.executed_amount, 1_000);
        assert!(r.output_amount > 0);
        assert_eq!(r.interactions.len(), 2);
    }

    #[test]
    fn simulate_path_rejects_below_limit() {
        let pools = vec![make_cp_pool("p1", "0xa", 1_000, "0xb", 1_000)];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());

        let order = make_order("uid1", "0xa", "0xb", 500, 999_999_999);
        let route = graph.simulate_path(&paths[0], &order, &pools);
        assert!(route.is_none());
    }

    #[test]
    fn viable_intermediaries_filters_correctly() {
        let pools = vec![
            make_cp_pool("p1", "0xa", 1_000_000, "0xmid", 1_000_000),
            make_cp_pool("p2", "0xmid", 1_000_000, "0xb", 1_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let intermediaries = graph.viable_intermediaries();
        assert!(
            intermediaries.contains(&"0xmid".to_string()),
            "0xmid should be a viable intermediary"
        );
    }

    #[test]
    fn find_best_graph_route_works() {
        let pools = vec![
            make_cp_pool("p1", "0xa", 1_000_000_000, "0xmid", 2_000_000_000),
            make_cp_pool("p2", "0xmid", 2_000_000_000, "0xb", 3_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let order = make_order("uid1", "0xa", "0xb", 1_000, 1);
        let route = find_best_graph_route(&order, &pools, &graph);
        assert!(route.is_some());
        let r = route.unwrap();
        assert!(r.surplus > 0);
    }

    #[test]
    fn find_top_n_routes_returns_ranked() {
        let pools = vec![
            make_cp_pool("direct", "0xa", 1_000_000_000, "0xb", 2_000_000_000),
            make_cp_pool("leg1", "0xa", 1_000_000_000, "0xm", 1_500_000_000),
            make_cp_pool("leg2", "0xm", 1_500_000_000, "0xb", 1_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let order = make_order("uid1", "0xa", "0xb", 1_000, 1);
        let routes = find_top_n_graph_routes(&order, &pools, &graph, 5);
        assert!(!routes.is_empty());
        // Should be sorted by net_surplus descending
        for i in 1..routes.len() {
            assert!(routes[i].net_surplus <= routes[i - 1].net_surplus);
        }
    }

    #[test]
    fn empty_liquidity_produces_empty_graph() {
        let graph = TokenGraph::build_no_gas(&[]);
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(paths.is_empty());
    }

    #[test]
    fn respects_max_hops() {
        let pools = vec![
            make_cp_pool("p1", "0xa", 1_000_000_000, "0xm1", 1_000_000_000),
            make_cp_pool("p2", "0xm1", 1_000_000_000, "0xm2", 1_000_000_000),
            make_cp_pool("p3", "0xm2", 1_000_000_000, "0xm3", 1_000_000_000),
            make_cp_pool("p4", "0xm3", 1_000_000_000, "0xm4", 1_000_000_000),
            make_cp_pool("p5", "0xm4", 1_000_000_000, "0xm5", 1_000_000_000),
            make_cp_pool("p6", "0xm5", 1_000_000_000, "0xb", 1_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        for path in &paths {
            assert!(
                path.edges.len() <= MAX_HOPS,
                "Path has {} hops, max is {}",
                path.edges.len(),
                MAX_HOPS
            );
        }
    }

    #[test]
    fn negative_cycle_detection_no_cycle() {
        let pools = vec![
            make_cp_pool("p1", "0xa", 1_000_000_000, "0xb", 2_000_000_000),
            make_cp_pool("p2", "0xb", 2_000_000_000, "0xc", 3_000_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        let cycle = graph.detect_negative_cycles();
        assert!(
            cycle.is_none(),
            "Normal pools with fees should not have negative cycles"
        );
    }

    #[test]
    fn multiple_pools_same_pair_creates_multiple_edges() {
        let pools = vec![
            make_cp_pool("v2_pool", "0xa", 1_000_000_000, "0xb", 2_000_000_000),
            make_cp_pool("sushi_pool", "0xa", 500_000_000, "0xb", 1_200_000_000),
        ];
        let graph = TokenGraph::build_no_gas(&pools);
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 4); // 2 pools * 2 directions
    }

    #[test]
    fn gas_aware_graph_penalizes_more_hops() {
        // With nonzero gas price, direct should be preferred over multi-hop
        // even if the multi-hop rate is slightly better
        let pools = vec![
            make_cp_pool("direct", "0xa", 1_000_000_000, "0xb", 1_990_000_000),
            make_cp_pool("leg1", "0xa", 1_000_000_000, "0xm", 1_000_000_000),
            make_cp_pool("leg2", "0xm", 1_000_000_000, "0xb", 1_000_000_000),
        ];
        // Use a meaningful gas price
        let graph = TokenGraph::build(&pools, 30_000_000_000, 1); // 30 gwei
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());
        // First path should be the direct route (1 hop) due to gas penalty on 2-hop
        assert_eq!(
            paths[0].edges.len(),
            1,
            "Direct route should be preferred with gas costs"
        );
    }

    #[test]
    fn marginal_rate_positive_for_valid_path() {
        let pools = vec![make_cp_pool("p1", "0xa", 1_000_000_000, "0xb", 2_000_000_000)];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());

        let rate = graph.marginal_rate(&paths[0], 10_000, &pools);
        assert!(rate.is_some());
        assert!(rate.unwrap() > 0.0, "Marginal rate should be positive");
    }

    #[test]
    fn marginal_rate_decreases_with_amount() {
        // Constant product: marginal price should decrease as amount increases.
        // Use amounts large enough that the finite difference captures real price impact.
        let pools = vec![make_cp_pool("p1", "0xa", 1_000_000_000, "0xb", 2_000_000_000)];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());

        // Use 1M and 500M to see clear price impact difference
        let rate_small = graph.marginal_rate(&paths[0], 1_000_000, &pools).unwrap();
        let rate_large = graph.marginal_rate(&paths[0], 500_000_000, &pools).unwrap();
        assert!(
            rate_small > rate_large,
            "Marginal rate should decrease with larger amounts (price impact): small={rate_small} large={rate_large}"
        );
    }

    #[test]
    fn simulate_path_partial_works() {
        let pools = vec![make_cp_pool("p1", "0xa", 1_000_000_000, "0xb", 2_000_000_000)];
        let graph = TokenGraph::build_no_gas(&pools);
        let paths = graph.find_paths("0xa", "0xb");
        assert!(!paths.is_empty());

        let result = graph.simulate_path_partial(&paths[0], "0xa", "0xb", 5_000, &pools);
        assert!(result.is_some());
        let (output, interactions) = result.unwrap();
        assert!(output > 0);
        assert_eq!(interactions.len(), 1);
    }
}
