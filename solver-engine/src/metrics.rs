use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;

/// Atomic counters tracking solver activity across all auction batches.
///
/// All fields use `Ordering::Relaxed` — we care about individual counter
/// accuracy, not cross-counter ordering guarantees.
#[derive(Debug, Default)]
pub struct SolverMetrics {
    /// Total auctions received at POST /solve
    pub auctions_received: AtomicU64,
    /// Auctions where at least one solution was returned
    pub auctions_solved: AtomicU64,
    /// Auctions where no profitable solution was found (returned empty)
    pub auctions_empty: AtomicU64,
    /// Auctions that resulted in an internal error
    pub auctions_errored: AtomicU64,
    /// Cumulative solve time in milliseconds
    pub total_solve_time_ms: AtomicU64,
    /// Total RPC calls made
    pub rpc_calls: AtomicU64,
    /// RPC calls that returned an error
    pub rpc_errors: AtomicU64,
}

impl SolverMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Increment helpers ─────────────────────────────────────────────────────

    pub fn inc_auctions_received(&self) {
        self.auctions_received.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_auctions_solved(&self) {
        self.auctions_solved.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_auctions_empty(&self) {
        self.auctions_empty.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_auctions_errored(&self) {
        self.auctions_errored.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_solve_time_ms(&self, ms: u64) {
        self.total_solve_time_ms.fetch_add(ms, Ordering::Relaxed);
    }

    pub fn inc_rpc_calls(&self) {
        self.rpc_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_rpc_errors(&self) {
        self.rpc_errors.fetch_add(1, Ordering::Relaxed);
    }

    // ── Snapshot ──────────────────────────────────────────────────────────────

    /// Take an instantaneous snapshot of all counters.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            auctions_received: self.auctions_received.load(Ordering::Relaxed),
            auctions_solved: self.auctions_solved.load(Ordering::Relaxed),
            auctions_empty: self.auctions_empty.load(Ordering::Relaxed),
            auctions_errored: self.auctions_errored.load(Ordering::Relaxed),
            total_solve_time_ms: self.total_solve_time_ms.load(Ordering::Relaxed),
            rpc_calls: self.rpc_calls.load(Ordering::Relaxed),
            rpc_errors: self.rpc_errors.load(Ordering::Relaxed),
        }
    }
}

/// Serializable snapshot of metrics (for GET /metrics).
#[derive(Debug, Serialize)]
pub struct MetricsSnapshot {
    pub auctions_received: u64,
    pub auctions_solved: u64,
    pub auctions_empty: u64,
    pub auctions_errored: u64,
    pub total_solve_time_ms: u64,
    pub rpc_calls: u64,
    pub rpc_errors: u64,
}

/// Shared metrics handle passed through Axum app state.
pub type SharedMetrics = Arc<SolverMetrics>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero() {
        let m = SolverMetrics::new();
        let snap = m.snapshot();
        assert_eq!(snap.auctions_received, 0);
        assert_eq!(snap.rpc_errors, 0);
    }

    #[test]
    fn increment_and_snapshot() {
        let m = SolverMetrics::new();
        m.inc_auctions_received();
        m.inc_auctions_received();
        m.inc_auctions_solved();
        m.add_solve_time_ms(250);
        m.inc_rpc_calls();
        m.inc_rpc_errors();

        let snap = m.snapshot();
        assert_eq!(snap.auctions_received, 2);
        assert_eq!(snap.auctions_solved, 1);
        assert_eq!(snap.auctions_empty, 0);
        assert_eq!(snap.total_solve_time_ms, 250);
        assert_eq!(snap.rpc_calls, 1);
        assert_eq!(snap.rpc_errors, 1);
    }

    #[test]
    fn shared_metrics_arc() {
        let m: SharedMetrics = Arc::new(SolverMetrics::new());
        let m2 = Arc::clone(&m);
        m.inc_auctions_received();
        assert_eq!(m2.snapshot().auctions_received, 1);
    }
}
