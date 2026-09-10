//! Token Graph Cache (B.2)
//!
//! Persistent, pre-cached token graph that rebuilds incrementally on each block.
//! Wraps the existing `solver::graph::TokenGraph` with a global cache layer so
//! individual auctions can skip graph construction.
//!
//! ## Architecture
//!
//! The graph is rebuilt whenever new liquidity arrives. Instead of rebuilding
//! from scratch each auction, we cache the last-built graph and only rebuild
//! when the underlying block changes.
//!
//! ## Usage
//!
//! ```rust,ignore
//! // At auction time — get cached graph or rebuild:
//! let graph = graph_cache::get_or_build(&auction.liquidity, gas_price, chain_id, block);
//! ```

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::models::liquidity::Liquidity;
use crate::solver::graph::TokenGraph;

// ── Cached State ────────────────────────────────────────────────────────────

struct CachedGraph {
    graph: Arc<TokenGraph>,
    block_number: u64,
    pool_count: usize,
    built_at_ms: u128,
}

static GRAPH_CACHE: OnceLock<RwLock<Option<CachedGraph>>> = OnceLock::new();

fn cache() -> &'static RwLock<Option<CachedGraph>> {
    GRAPH_CACHE.get_or_init(|| RwLock::new(None))
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Get the cached token graph, or rebuild if stale (different block or pool count).
///
/// This is the primary entry point for the solver. Returns an `Arc<TokenGraph>`
/// that can be shared across strategies without cloning.
///
/// Rebuild happens when:
/// - No cached graph exists
/// - Block number changed
/// - Pool count changed significantly (>10% delta)
pub async fn get_or_build(
    liquidity: &[Liquidity],
    gas_price_wei: u128,
    chain_id: u64,
    block_number: u64,
) -> Arc<TokenGraph> {
    // Fast path: check if cached graph is still fresh
    {
        let guard = cache().read().await;
        if let Some(ref cached) = *guard {
            if cached.block_number == block_number && is_similar_pool_count(cached.pool_count, liquidity.len()) {
                debug!(
                    block = block_number,
                    pools = cached.pool_count,
                    built_ms = cached.built_at_ms,
                    "Using cached token graph"
                );
                return Arc::clone(&cached.graph);
            }
        }
    }

    // Slow path: rebuild the graph
    let start = Instant::now();
    let graph = TokenGraph::build(liquidity, gas_price_wei, chain_id);
    let build_time = start.elapsed().as_millis();

    info!(
        block = block_number,
        pools = liquidity.len(),
        build_ms = build_time,
        "Rebuilt token graph"
    );

    let graph = Arc::new(graph);

    // Cache the new graph
    {
        let mut guard = cache().write().await;
        *guard = Some(CachedGraph {
            graph: Arc::clone(&graph),
            block_number,
            pool_count: liquidity.len(),
            built_at_ms: build_time,
        });
    }

    graph
}

/// Force invalidate the cached graph (e.g., on state reset).
pub async fn invalidate() {
    let mut guard = cache().write().await;
    *guard = None;
    debug!("Token graph cache invalidated");
}

/// Get stats about the current cached graph (for metrics).
pub async fn cache_stats() -> Option<(u64, usize, u128)> {
    let guard = cache().read().await;
    guard.as_ref().map(|c| (c.block_number, c.pool_count, c.built_at_ms))
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Pool counts are "similar" if they differ by less than 10%.
fn is_similar_pool_count(cached: usize, current: usize) -> bool {
    if cached == 0 && current == 0 {
        return true;
    }
    if cached == 0 || current == 0 {
        return false;
    }
    let ratio = cached as f64 / current as f64;
    (0.9..=1.1).contains(&ratio)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similar_pool_count_same() {
        assert!(is_similar_pool_count(100, 100));
    }

    #[test]
    fn similar_pool_count_slight_change() {
        assert!(is_similar_pool_count(100, 105));
        assert!(is_similar_pool_count(100, 95));
    }

    #[test]
    fn dissimilar_pool_count_large_change() {
        assert!(!is_similar_pool_count(100, 50));
        assert!(!is_similar_pool_count(100, 200));
    }

    #[test]
    fn similar_pool_count_zero() {
        assert!(is_similar_pool_count(0, 0));
        assert!(!is_similar_pool_count(0, 10));
        assert!(!is_similar_pool_count(10, 0));
    }
}
