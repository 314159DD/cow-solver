//! Solver monitoring: Prometheus metrics, alerting, and background reporting.
//!
//! # Quick start
//!
//! In your solve handler, call the helpers:
//! ```rust,ignore
//! use solver_engine::monitoring;
//!
//! monitoring::record_solve(duration_ms, solutions_count, score_wei, actually_submitted);
//! monitoring::record_rpc(latency_ms, is_error);
//! monitoring::set_pool_count(count);
//! ```
//!
//! Register the `/metrics` route in `main.rs`:
//! ```rust,ignore
//! .route("/metrics", get(solver_engine::monitoring::metrics_handler))
//! ```
//!
//! Spawn the background task once at startup:
//! ```rust,ignore
//! tokio::spawn(solver_engine::monitoring::run_background_task());
//! ```

pub mod revenue;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use tracing::{debug, error, info, warn};

use revenue::RevenueTracker;

// ─── Metric Store ─────────────────────────────────────────────────────────────

/// All runtime counters for the solver.
///
/// Every field is either an `AtomicU64` (lock-free) or a plain `u64` read-only
/// after initialisation (start time).
///
/// Score accumulator (`score_sum_gwei`) stores values in **gwei** to avoid
/// u64 overflow. Sub-gwei scores are rounded up to 1 gwei. Truncation is
/// counted and logged.
pub struct SolverMetrics {
    // ── Auction / solve ──────────────────────────────────────────────────────
    /// Total POST /solve calls received.
    pub auctions_received: AtomicU64,
    /// /solve calls where we actually returned ≥1 solution to the driver.
    pub solutions_submitted: AtomicU64,
    /// /solve calls where we found solutions but held them (sim revert, risk, etc).
    pub solutions_held: AtomicU64,
    /// /solve calls where we returned an empty solution set (no solutions found).
    pub solutions_empty: AtomicU64,
    /// Sum of solve durations in milliseconds.
    pub total_solve_ms: AtomicU64,
    /// Number of solve samples (for average).
    pub solve_samples: AtomicU64,

    // ── Score ────────────────────────────────────────────────────────────────
    // score_sum moved to score_sum_wei below in the Score range section
    /// Number of score samples.
    pub score_samples: AtomicU64,

    // ── RPC ──────────────────────────────────────────────────────────────────
    /// Total RPC calls made.
    pub rpc_calls: AtomicU64,
    /// RPC calls that returned an error.
    pub rpc_errors: AtomicU64,
    /// Sum of RPC latencies in milliseconds.
    pub total_rpc_ms: AtomicU64,

    // ── Fallback / Strategy ────────────────────────────────────────────────────
    /// Number of auctions where the fail-safe fallback was used.
    pub fallback_used: AtomicU64,
    /// Per-strategy submission counts (indexed by strategy name hash).
    /// We track the top strategies via separate counters for simplicity.
    pub strategy_cow: AtomicU64,
    pub strategy_direct: AtomicU64,
    pub strategy_combined: AtomicU64,
    pub strategy_multihop: AtomicU64,
    pub strategy_graph: AtomicU64,
    pub strategy_split: AtomicU64,
    pub strategy_external: AtomicU64,
    pub strategy_fallback: AtomicU64,

    // ── Solve time histogram ──────────────────────────────────────────────────
    /// Bucketed solve duration counts.
    /// Fine-grained around 200-500ms (typical solve range):
    /// 50, 100, 150, 200, 250, 300, 350, 400, 500, 750, 1k, 2.5k, 5k, 10k, 25k, ∞
    pub solve_hist_50:    AtomicU64,
    pub solve_hist_100:   AtomicU64,
    pub solve_hist_150:   AtomicU64,
    pub solve_hist_200:   AtomicU64,
    pub solve_hist_250:   AtomicU64,
    pub solve_hist_300:   AtomicU64,
    pub solve_hist_350:   AtomicU64,
    pub solve_hist_400:   AtomicU64,
    pub solve_hist_500:   AtomicU64,
    pub solve_hist_750:   AtomicU64,
    pub solve_hist_1000:  AtomicU64,
    pub solve_hist_2500:  AtomicU64,
    pub solve_hist_5000:  AtomicU64,
    pub solve_hist_10000: AtomicU64,
    pub solve_hist_25000: AtomicU64,
    pub solve_hist_over:  AtomicU64, // >25 s — timeout territory

    // ── Score range (in gwei — safe from u64 overflow for realistic scores) ────
    /// Sum of scores in gwei (u64 holds up to ~18.4e9 gwei = ~18.4 ETH total).
    pub score_sum_gwei: AtomicU64,
    /// Minimum observed score (gwei). Initialised to u64::MAX.
    pub score_min_gwei: AtomicU64,
    /// Maximum observed score (gwei).
    pub score_max_gwei: AtomicU64,
    /// Number of scores that were truncated during u128→u64 conversion.
    pub score_truncated_count: AtomicU64,

    // ── Rolling hourly throughput ─────────────────────────────────────────────
    /// Unix second when the current 1-hour window started.
    pub hour_window_start: AtomicU64,
    /// Auctions received since the window opened.
    pub auctions_this_hour: AtomicU64,
    /// Auction count of the last completed 1-hour window (for stable display).
    pub last_hour_auctions: AtomicU64,

    // ── Phase tracking (iterative deepening) ───────────────────────────────────
    /// How many auctions reached each phase (1-5).
    pub phase_reached_1: AtomicU64,
    pub phase_reached_2: AtomicU64,
    pub phase_reached_3: AtomicU64,
    pub phase_reached_4: AtomicU64,
    pub phase_reached_5: AtomicU64,
    /// Cumulative time spent in each phase (ms).
    pub phase_time_sum_1: AtomicU64,
    pub phase_time_sum_2: AtomicU64,
    pub phase_time_sum_3: AtomicU64,
    pub phase_time_sum_4: AtomicU64,
    pub phase_time_sum_5: AtomicU64,

    // ── ETH price (for EUR/USD display) ─────────────────────────────────────
    /// ETH price in USD cents (e.g. 270000 = $2700.00). Updated from auction token data.
    pub eth_price_usd_cents: AtomicU64,

    // ── State ─────────────────────────────────────────────────────────────────
    /// Number of liquidity pools currently tracked.
    pub pool_count: AtomicU64,
    /// Unix timestamp (seconds) when this process started.
    pub start_time_sec: u64,
    /// Unix timestamp (seconds) of the most recent auction received.
    pub last_auction_sec: AtomicU64,
}

impl SolverMetrics {
    fn new() -> Self {
        Self {
            auctions_received: AtomicU64::new(0),
            solutions_submitted: AtomicU64::new(0),
            solutions_held: AtomicU64::new(0),
            solutions_empty: AtomicU64::new(0),
            total_solve_ms: AtomicU64::new(0),
            solve_samples: AtomicU64::new(0),
            // score_sum is in the Score range section below
            score_samples: AtomicU64::new(0),
            rpc_calls: AtomicU64::new(0),
            rpc_errors: AtomicU64::new(0),
            total_rpc_ms: AtomicU64::new(0),
            fallback_used: AtomicU64::new(0),
            strategy_cow: AtomicU64::new(0),
            strategy_direct: AtomicU64::new(0),
            strategy_combined: AtomicU64::new(0),
            strategy_multihop: AtomicU64::new(0),
            strategy_graph: AtomicU64::new(0),
            strategy_split: AtomicU64::new(0),
            strategy_external: AtomicU64::new(0),
            strategy_fallback: AtomicU64::new(0),
            solve_hist_50:    AtomicU64::new(0),
            solve_hist_100:   AtomicU64::new(0),
            solve_hist_150:   AtomicU64::new(0),
            solve_hist_200:   AtomicU64::new(0),
            solve_hist_250:   AtomicU64::new(0),
            solve_hist_300:   AtomicU64::new(0),
            solve_hist_350:   AtomicU64::new(0),
            solve_hist_400:   AtomicU64::new(0),
            solve_hist_500:   AtomicU64::new(0),
            solve_hist_750:   AtomicU64::new(0),
            solve_hist_1000:  AtomicU64::new(0),
            solve_hist_2500:  AtomicU64::new(0),
            solve_hist_5000:  AtomicU64::new(0),
            solve_hist_10000: AtomicU64::new(0),
            solve_hist_25000: AtomicU64::new(0),
            solve_hist_over:  AtomicU64::new(0),
            score_sum_gwei:       AtomicU64::new(0),
            score_min_gwei:       AtomicU64::new(u64::MAX),
            score_max_gwei:       AtomicU64::new(0),
            score_truncated_count: AtomicU64::new(0),
            hour_window_start:  AtomicU64::new(now_secs()),
            auctions_this_hour: AtomicU64::new(0),
            last_hour_auctions: AtomicU64::new(0),
            phase_reached_1: AtomicU64::new(0),
            phase_reached_2: AtomicU64::new(0),
            phase_reached_3: AtomicU64::new(0),
            phase_reached_4: AtomicU64::new(0),
            phase_reached_5: AtomicU64::new(0),
            phase_time_sum_1: AtomicU64::new(0),
            phase_time_sum_2: AtomicU64::new(0),
            phase_time_sum_3: AtomicU64::new(0),
            phase_time_sum_4: AtomicU64::new(0),
            phase_time_sum_5: AtomicU64::new(0),
            pool_count: AtomicU64::new(0),
            eth_price_usd_cents: AtomicU64::new(0),
            start_time_sec: now_secs(),
            last_auction_sec: AtomicU64::new(0),
        }
    }
}

static METRICS: OnceLock<SolverMetrics> = OnceLock::new();

/// Returns the global singleton `SolverMetrics`.
pub fn metrics() -> &'static SolverMetrics {
    METRICS.get_or_init(SolverMetrics::new)
}

// ─── Recording Helpers ────────────────────────────────────────────────────────

/// Record the completion of one solve call.
///
/// * `duration_ms` — wall-clock time in the solve handler.
/// * `solutions` — number of solutions returned (0 = empty).
/// * `score_wei` — score of the best solution in wei (0 if no solution).
/// Record the outcome of one solve call.
///
/// `actually_submitted` — true only if solutions were returned to the driver.
/// `solutions_generated` — number of solutions the solver found (may be held).
pub fn record_solve(duration_ms: u64, solutions_generated: usize, score_wei: u128, actually_submitted: bool) {
    let m = metrics();
    m.auctions_received.fetch_add(1, Ordering::Relaxed);
    m.last_auction_sec.store(now_secs(), Ordering::Relaxed);
    m.total_solve_ms.fetch_add(duration_ms, Ordering::Relaxed);
    m.solve_samples.fetch_add(1, Ordering::Relaxed);
    m.auctions_this_hour.fetch_add(1, Ordering::Relaxed);

    if solutions_generated > 0 && actually_submitted {
        m.solutions_submitted.fetch_add(1, Ordering::Relaxed);
    } else if solutions_generated > 0 && !actually_submitted {
        m.solutions_held.fetch_add(1, Ordering::Relaxed);
    } else {
        m.solutions_empty.fetch_add(1, Ordering::Relaxed);
    }

    // Solve time histogram — fine-grained sub-50ms buckets for fast solvers
    match duration_ms {
        d if d <=   50 => m.solve_hist_50.fetch_add(1, Ordering::Relaxed),
        d if d <=  100 => m.solve_hist_100.fetch_add(1, Ordering::Relaxed),
        d if d <=  150 => m.solve_hist_150.fetch_add(1, Ordering::Relaxed),
        d if d <=  200 => m.solve_hist_200.fetch_add(1, Ordering::Relaxed),
        d if d <=  250 => m.solve_hist_250.fetch_add(1, Ordering::Relaxed),
        d if d <=  300 => m.solve_hist_300.fetch_add(1, Ordering::Relaxed),
        d if d <=  350 => m.solve_hist_350.fetch_add(1, Ordering::Relaxed),
        d if d <=  400 => m.solve_hist_400.fetch_add(1, Ordering::Relaxed),
        d if d <=  500 => m.solve_hist_500.fetch_add(1, Ordering::Relaxed),
        d if d <=  750 => m.solve_hist_750.fetch_add(1, Ordering::Relaxed),
        d if d <= 1000 => m.solve_hist_1000.fetch_add(1, Ordering::Relaxed),
        d if d <= 2500 => m.solve_hist_2500.fetch_add(1, Ordering::Relaxed),
        d if d <= 5000 => m.solve_hist_5000.fetch_add(1, Ordering::Relaxed),
        d if d <= 10000 => m.solve_hist_10000.fetch_add(1, Ordering::Relaxed),
        d if d <= 25000 => m.solve_hist_25000.fetch_add(1, Ordering::Relaxed),
        _ => m.solve_hist_over.fetch_add(1, Ordering::Relaxed),
    };

    if score_wei > 0 {
        // Store in gwei to avoid u64 overflow.
        // score_wei / 1e9 = gwei. Round up sub-gwei to 1 so they're not lost.
        let gwei = (score_wei / 1_000_000_000).max(1) as u64;

        // Check for values that exceed u64 range even in gwei (>18.4 ETH per score)
        let gwei_128 = score_wei / 1_000_000_000;
        if gwei_128 > u64::MAX as u128 {
            m.score_truncated_count.fetch_add(1, Ordering::Relaxed);
            warn!(
                score_wei = %score_wei,
                gwei_128 = %gwei_128,
                "Score exceeds u64 range even in gwei — truncated"
            );
        }

        m.score_sum_gwei.fetch_add(gwei, Ordering::Relaxed);
        m.score_samples.fetch_add(1, Ordering::Relaxed);
        m.score_min_gwei.fetch_min(gwei, Ordering::Relaxed);
        m.score_max_gwei.fetch_max(gwei, Ordering::Relaxed);
    }
}

/// Returns the 15 solve-time histogram bucket counts.
/// Buckets (upper bound ms): 1,2,5,10,25,50,100,200,500,1k,2.5k,5k,10k,25k,∞
pub fn solve_histogram() -> [u64; 16] {
    let m = metrics();
    [
        m.solve_hist_50.load(Ordering::Relaxed),
        m.solve_hist_100.load(Ordering::Relaxed),
        m.solve_hist_150.load(Ordering::Relaxed),
        m.solve_hist_200.load(Ordering::Relaxed),
        m.solve_hist_250.load(Ordering::Relaxed),
        m.solve_hist_300.load(Ordering::Relaxed),
        m.solve_hist_350.load(Ordering::Relaxed),
        m.solve_hist_400.load(Ordering::Relaxed),
        m.solve_hist_500.load(Ordering::Relaxed),
        m.solve_hist_750.load(Ordering::Relaxed),
        m.solve_hist_1000.load(Ordering::Relaxed),
        m.solve_hist_2500.load(Ordering::Relaxed),
        m.solve_hist_5000.load(Ordering::Relaxed),
        m.solve_hist_10000.load(Ordering::Relaxed),
        m.solve_hist_25000.load(Ordering::Relaxed),
        m.solve_hist_over.load(Ordering::Relaxed),
    ]
}

/// Histogram bucket upper bounds in milliseconds (matches solve_histogram order).
pub const HIST_BOUNDS: [u64; 16] = [50, 100, 150, 200, 250, 300, 350, 400, 500, 750, 1000, 2500, 5000, 10000, 25000, 30000];

/// Approximate p50 / p95 / p99 from the histogram (returns ms).
pub fn compute_percentiles() -> (u64, u64, u64) {
    let hist = solve_histogram();
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return (0, 0, 0);
    }
    let find_p = |pct: f64| -> u64 {
        let target = ((pct * total as f64).ceil() as u64).max(1);
        let mut cum = 0u64;
        for (i, &count) in hist.iter().enumerate() {
            cum += count;
            if cum >= target {
                return HIST_BOUNDS[i];
            }
        }
        HIST_BOUNDS[15]
    };
    (find_p(0.50), find_p(0.95), find_p(0.99))
}

/// Returns (min_gwei, max_gwei). Returns (0,0) if no scored solutions yet.
pub fn score_range() -> (u64, u64) {
    let m = metrics();
    let min = m.score_min_gwei.load(Ordering::Relaxed);
    let max = m.score_max_gwei.load(Ordering::Relaxed);
    if min == u64::MAX { (0, 0) } else { (min, max) }
}

/// Returns the count of scores that were truncated during recording.
pub fn score_truncated_count() -> u64 {
    metrics().score_truncated_count.load(Ordering::Relaxed)
}

/// Returns estimated auctions per hour based on rolling data.
/// Uses the last completed hour if available, otherwise extrapolates from current window.
pub fn auctions_per_hour() -> u64 {
    let m = metrics();
    let last_completed = m.last_hour_auctions.load(Ordering::Relaxed);
    if last_completed > 0 {
        return last_completed;
    }
    // Extrapolate from current window
    let window_start = m.hour_window_start.load(Ordering::Relaxed);
    let elapsed_secs = now_secs().saturating_sub(window_start);
    if elapsed_secs < 60 {
        return 0; // Too early to extrapolate meaningfully
    }
    let current = m.auctions_this_hour.load(Ordering::Relaxed);
    // Extrapolate: (current / elapsed_secs) * 3600
    (current as f64 / elapsed_secs as f64 * 3600.0) as u64
}

/// Rotate the hourly window — call from the background task every hour.
pub fn rotate_hour_window() {
    let m = metrics();
    let count = m.auctions_this_hour.swap(0, Ordering::Relaxed);
    m.last_hour_auctions.store(count, Ordering::Relaxed);
    m.hour_window_start.store(now_secs(), Ordering::Relaxed);
}

/// Bootstrap in-memory counters from replay DB so dashboard isn't empty after restart.
pub fn bootstrap_from_replay() {
    match crate::replay::load_bootstrap_metrics() {
        Ok(bm) if bm.auctions_received > 0 => {
            let m = metrics();
            m.auctions_received.store(bm.auctions_received, Ordering::Relaxed);
            m.solutions_submitted.store(bm.solutions_submitted, Ordering::Relaxed);
            m.solutions_held.store(bm.solutions_held, Ordering::Relaxed);
            m.solutions_empty.store(bm.solutions_empty, Ordering::Relaxed);
            m.total_solve_ms.store(bm.total_solve_ms, Ordering::Relaxed);
            m.solve_samples.store(bm.auctions_received, Ordering::Relaxed);
            m.last_auction_sec.store(bm.last_auction_sec, Ordering::Relaxed);
            if bm.avg_score_gwei > 0 {
                m.score_sum_gwei.store(bm.avg_score_gwei * bm.auctions_received, Ordering::Relaxed);
                m.score_samples.store(bm.auctions_received, Ordering::Relaxed);
            }
            info!(
                auctions = bm.auctions_received,
                submitted = bm.solutions_submitted,
                held = bm.solutions_held,
                empty = bm.solutions_empty,
                last_auction_sec = bm.last_auction_sec,
                "Bootstrapped monitoring counters from replay DB"
            );
        }
        Ok(_) => {
            debug!("No replay data to bootstrap monitoring counters");
        }
        Err(e) => {
            warn!(error = %e, "Failed to bootstrap monitoring counters from replay DB");
        }
    }
}

/// Record that the fail-safe fallback was used for this auction.
pub fn record_fallback() {
    metrics().fallback_used.fetch_add(1, Ordering::Relaxed);
}

/// Record which strategy produced the submitted solution.
pub fn record_strategy(strategy: &str) {
    let m = metrics();
    match strategy {
        "cow" => m.strategy_cow.fetch_add(1, Ordering::Relaxed),
        "direct" => m.strategy_direct.fetch_add(1, Ordering::Relaxed),
        "combined" => m.strategy_combined.fetch_add(1, Ordering::Relaxed),
        "multihop" => m.strategy_multihop.fetch_add(1, Ordering::Relaxed),
        "graph" => m.strategy_graph.fetch_add(1, Ordering::Relaxed),
        "split" => m.strategy_split.fetch_add(1, Ordering::Relaxed),
        "external" | "aggregator" => m.strategy_external.fetch_add(1, Ordering::Relaxed),
        "fallback" => m.strategy_fallback.fetch_add(1, Ordering::Relaxed),
        _ => 0, // unknown strategy, no counter
    };
}

/// Record phase completion data from iterative deepening.
///
/// * `phase_reached` — highest phase reached (1-5).
/// * `phase_times_ms` — time spent in each phase (index 0 = phase 1).
pub fn record_phase(phase_reached: u8, phase_times_ms: &[u64; 5]) {
    let m = metrics();
    // Increment reached counters for all phases up to and including phase_reached
    if phase_reached >= 1 {
        m.phase_reached_1.fetch_add(1, Ordering::Relaxed);
        m.phase_time_sum_1.fetch_add(phase_times_ms[0], Ordering::Relaxed);
    }
    if phase_reached >= 2 {
        m.phase_reached_2.fetch_add(1, Ordering::Relaxed);
        m.phase_time_sum_2.fetch_add(phase_times_ms[1], Ordering::Relaxed);
    }
    if phase_reached >= 3 {
        m.phase_reached_3.fetch_add(1, Ordering::Relaxed);
        m.phase_time_sum_3.fetch_add(phase_times_ms[2], Ordering::Relaxed);
    }
    if phase_reached >= 4 {
        m.phase_reached_4.fetch_add(1, Ordering::Relaxed);
        m.phase_time_sum_4.fetch_add(phase_times_ms[3], Ordering::Relaxed);
    }
    if phase_reached >= 5 {
        m.phase_reached_5.fetch_add(1, Ordering::Relaxed);
        m.phase_time_sum_5.fetch_add(phase_times_ms[4], Ordering::Relaxed);
    }
}

/// Record one RPC call.
///
/// * `duration_ms` — call latency.
/// * `is_error` — whether the call returned an error.
pub fn record_rpc(duration_ms: u64, is_error: bool) {
    let m = metrics();
    m.rpc_calls.fetch_add(1, Ordering::Relaxed);
    m.total_rpc_ms.fetch_add(duration_ms, Ordering::Relaxed);
    if is_error {
        m.rpc_errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// Update the current pool count gauge.
pub fn set_pool_count(count: u64) {
    metrics().pool_count.store(count, Ordering::Relaxed);
}

/// Set the ETH price in USD cents (e.g. 270000 = $2700.00).
/// Called from the solve handler using auction token reference prices.
pub fn set_eth_price_usd_cents(cents: u64) {
    if cents > 0 {
        metrics().eth_price_usd_cents.store(cents, Ordering::Relaxed);
    }
}

/// Get the cached ETH price in USD cents. Returns 0 if not yet set.
pub fn eth_price_usd_cents() -> u64 {
    metrics().eth_price_usd_cents.load(Ordering::Relaxed)
}

// ─── Prometheus Renderer ──────────────────────────────────────────────────────

/// Renders all metrics in Prometheus text format (exposition format 0.0.4).
pub fn render_prometheus() -> String {
    let m = metrics();
    let now_sec = now_secs();

    let auctions = m.auctions_received.load(Ordering::Relaxed);
    let submitted = m.solutions_submitted.load(Ordering::Relaxed);
    let held = m.solutions_held.load(Ordering::Relaxed);
    let empty = m.solutions_empty.load(Ordering::Relaxed);
    let solve_ms = m.total_solve_ms.load(Ordering::Relaxed);
    let solve_n = m.solve_samples.load(Ordering::Relaxed);
    let score_gwei = m.score_sum_gwei.load(Ordering::Relaxed);
    let score_n = m.score_samples.load(Ordering::Relaxed);
    let rpc_calls = m.rpc_calls.load(Ordering::Relaxed);
    let rpc_errors = m.rpc_errors.load(Ordering::Relaxed);
    let rpc_ms = m.total_rpc_ms.load(Ordering::Relaxed);
    let pools = m.pool_count.load(Ordering::Relaxed);
    let last_auction = m.last_auction_sec.load(Ordering::Relaxed);
    let uptime = now_sec.saturating_sub(m.start_time_sec);

    // Derived
    let avg_solve_ms = if solve_n > 0 { solve_ms / solve_n } else { 0 };
    let avg_rpc_ms = if rpc_calls > 0 { rpc_ms / rpc_calls } else { 0 };
    let avg_score_gwei = if score_n > 0 { score_gwei / score_n } else { 0 };
    let win_rate_pct: f64 = if auctions > 0 {
        submitted as f64 / auctions as f64 * 100.0
    } else {
        0.0
    };
    let rpc_error_pct: f64 = if rpc_calls > 0 {
        rpc_errors as f64 / rpc_calls as f64 * 100.0
    } else {
        0.0
    };
    let secs_since_auction = if last_auction > 0 {
        now_sec.saturating_sub(last_auction)
    } else {
        u64::MAX
    };

    let mut out = String::with_capacity(2048);

    macro_rules! counter {
        ($name:expr, $help:expr, $val:expr) => {
            out.push_str(&format!(
                "# HELP {} {}\n# TYPE {} counter\n{} {}\n",
                $name, $help, $name, $name, $val
            ));
        };
    }

    macro_rules! gauge {
        ($name:expr, $help:expr, $val:expr) => {
            out.push_str(&format!(
                "# HELP {} {}\n# TYPE {} gauge\n{} {}\n",
                $name, $help, $name, $name, $val
            ));
        };
    }

    // ── Auction counters ─────────────────────────────────────────────────────
    counter!(
        "cow_solver_auctions_received_total",
        "Total number of POST /solve calls received.",
        auctions
    );
    counter!(
        "cow_solver_solutions_submitted_total",
        "Number of solve calls where at least one solution was returned.",
        submitted
    );
    counter!(
        "cow_solver_solutions_held_total",
        "Number of solve calls where solutions were found but held (sim revert, risk, etc).",
        held
    );
    counter!(
        "cow_solver_solutions_empty_total",
        "Number of solve calls where no profitable solution was found.",
        empty
    );

    // ── Solve performance ────────────────────────────────────────────────────
    gauge!(
        "cow_solver_solve_duration_ms_avg",
        "Average solve duration in milliseconds.",
        avg_solve_ms
    );
    gauge!(
        "cow_solver_win_rate_pct",
        "Percentage of auctions where a non-empty solution was returned.",
        win_rate_pct
    );

    // ── Score (in gwei — safe from u64 overflow) ──────────────────────────
    gauge!(
        "cow_solver_avg_score_gwei",
        "Average solution score in gwei.",
        avg_score_gwei
    );

    // ── RPC ─────────────────────────────────────────────────────────────────
    counter!(
        "cow_solver_rpc_calls_total",
        "Total Ethereum RPC calls made.",
        rpc_calls
    );
    counter!(
        "cow_solver_rpc_errors_total",
        "RPC calls that returned an error.",
        rpc_errors
    );
    gauge!(
        "cow_solver_rpc_latency_ms_avg",
        "Average RPC call latency in milliseconds.",
        avg_rpc_ms
    );
    gauge!(
        "cow_solver_rpc_error_pct",
        "Percentage of RPC calls that errored.",
        rpc_error_pct
    );

    // ── Solve time histogram ─────────────────────────────────────────────────
    {
        let hist = solve_histogram();
        let bounds = HIST_BOUNDS;
        out.push_str("# HELP cow_solver_solve_duration_ms_bucket Solve time histogram (cumulative).\n");
        out.push_str("# TYPE cow_solver_solve_duration_ms_bucket counter\n");
        let mut cum = 0u64;
        for (i, &count) in hist.iter().enumerate() {
            cum += count;
            out.push_str(&format!(
                "cow_solver_solve_duration_ms_bucket{{le=\"{}\"}} {}\n",
                bounds[i], cum
            ));
        }
        let (p50, p95, p99) = compute_percentiles();
        gauge!("cow_solver_solve_p50_ms", "Approximate p50 solve latency (ms).", p50);
        gauge!("cow_solver_solve_p95_ms", "Approximate p95 solve latency (ms).", p95);
        gauge!("cow_solver_solve_p99_ms", "Approximate p99 solve latency (ms).", p99);
    }

    // ── Score range ───────────────────────────────────────────────────────────
    {
        let (min, max) = score_range();
        gauge!("cow_solver_score_min_gwei", "Minimum observed solution score (gwei).", min);
        gauge!("cow_solver_score_max_gwei", "Maximum observed solution score (gwei).", max);
    }

    // ── Throughput ────────────────────────────────────────────────────────────
    gauge!(
        "cow_solver_auctions_per_hour",
        "Auctions received in the last completed 1-hour window.",
        auctions_per_hour()
    );

    // ── Fallback ──────────────────────────────────────────────────────────────
    counter!(
        "cow_solver_fallback_used_total",
        "Number of auctions where the fail-safe fallback produced the solution.",
        m.fallback_used.load(Ordering::Relaxed)
    );

    // ── Strategy breakdown ───────────────────────────────────────────────────
    let strategies = [
        ("cow", m.strategy_cow.load(Ordering::Relaxed)),
        ("direct", m.strategy_direct.load(Ordering::Relaxed)),
        ("combined", m.strategy_combined.load(Ordering::Relaxed)),
        ("multihop", m.strategy_multihop.load(Ordering::Relaxed)),
        ("graph", m.strategy_graph.load(Ordering::Relaxed)),
        ("split", m.strategy_split.load(Ordering::Relaxed)),
        ("external", m.strategy_external.load(Ordering::Relaxed)),
        ("fallback", m.strategy_fallback.load(Ordering::Relaxed)),
    ];
    out.push_str("# HELP cow_solver_strategy_submitted_total Solutions submitted per strategy.\n");
    out.push_str("# TYPE cow_solver_strategy_submitted_total counter\n");
    for (name, count) in &strategies {
        out.push_str(&format!(
            "cow_solver_strategy_submitted_total{{strategy=\"{}\"}} {}\n",
            name, count
        ));
    }

    // ── Phase tracking (iterative deepening) ───────────────────────────────
    out.push_str("# HELP cow_solver_phase_reached_total Auctions reaching each phase.\n");
    out.push_str("# TYPE cow_solver_phase_reached_total counter\n");
    for phase in 1..=5u8 {
        let count = match phase {
            1 => m.phase_reached_1.load(Ordering::Relaxed),
            2 => m.phase_reached_2.load(Ordering::Relaxed),
            3 => m.phase_reached_3.load(Ordering::Relaxed),
            4 => m.phase_reached_4.load(Ordering::Relaxed),
            5 => m.phase_reached_5.load(Ordering::Relaxed),
            _ => 0,
        };
        out.push_str(&format!(
            "cow_solver_phase_reached_total{{phase=\"{}\"}} {}\n",
            phase, count
        ));
    }

    out.push_str("# HELP cow_solver_phase_time_sum_ms Cumulative time in each phase (ms).\n");
    out.push_str("# TYPE cow_solver_phase_time_sum_ms counter\n");
    for phase in 1..=5u8 {
        let time_sum = match phase {
            1 => m.phase_time_sum_1.load(Ordering::Relaxed),
            2 => m.phase_time_sum_2.load(Ordering::Relaxed),
            3 => m.phase_time_sum_3.load(Ordering::Relaxed),
            4 => m.phase_time_sum_4.load(Ordering::Relaxed),
            5 => m.phase_time_sum_5.load(Ordering::Relaxed),
            _ => 0,
        };
        out.push_str(&format!(
            "cow_solver_phase_time_sum_ms{{phase=\"{}\"}} {}\n",
            phase, time_sum
        ));
    }

    // ── Simulation (A.5) ──────────────────────────────────────────────────
    {
        let sm = crate::simulation::sim_metrics();
        counter!(
            "cow_solver_simulation_total",
            "Total simulation attempts.",
            sm.total.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_simulation_passed_total",
            "Simulations that passed.",
            sm.passed.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_simulation_reverted_total",
            "Simulations that reverted.",
            sm.reverted.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_simulation_timeout_total",
            "Simulations that timed out.",
            sm.timeout.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_simulation_skipped_total",
            "Simulations skipped (kill switch or disabled).",
            sm.skipped.load(Ordering::Relaxed)
        );
    }

    // ── Triage (B.5) ───────────────────────────────────────────────────────
    {
        let tm = crate::triage::triage_metrics();
        let triage_classes = [
            ("profitable", tm.profitable.load(Ordering::Relaxed)),
            ("marginal", tm.marginal.load(Ordering::Relaxed)),
            ("unwinnable", tm.unwinnable.load(Ordering::Relaxed)),
            ("skip", tm.skip.load(Ordering::Relaxed)),
        ];
        out.push_str("# HELP cow_solver_triage_total Auctions classified per triage class.\n");
        out.push_str("# TYPE cow_solver_triage_total counter\n");
        for (class, count) in &triage_classes {
            out.push_str(&format!(
                "cow_solver_triage_total{{class=\"{}\"}} {}\n",
                class, count
            ));
        }
    }

    // ── Freshness (B.4) ─────────────────────────────────────────────────────
    {
        let fm = crate::freshness::freshness_metrics();
        counter!(
            "cow_solver_freshness_total",
            "Total freshness scores computed.",
            fm.total.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_freshness_fresh_total",
            "Freshness scores above 0.8 (fresh).",
            fm.fresh_count.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_freshness_acceptable_total",
            "Freshness scores between 0.3 and 0.8.",
            fm.acceptable_count.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_freshness_stale_total",
            "Freshness scores below 0.3 (stale).",
            fm.stale_count.load(Ordering::Relaxed)
        );
        let total = fm.total.load(Ordering::Relaxed);
        let sum_milli = fm.confidence_sum_milli.load(Ordering::Relaxed);
        let avg_confidence = if total > 0 { sum_milli as f64 / total as f64 / 1000.0 } else { 0.0 };
        gauge!(
            "cow_solver_freshness_avg_confidence",
            "Average freshness confidence (0.0-1.0).",
            format!("{:.3}", avg_confidence)
        );
    }

    // ── Submission Policy (C.2) ─────────────────────────────────────────────
    {
        let sm = crate::submission::submission_metrics();
        counter!(
            "cow_solver_submission_decisions_total",
            "Total submission policy evaluations.",
            sm.total_decisions.load(Ordering::Relaxed)
        );
        let timing = [
            ("submit", sm.submit_count.load(Ordering::Relaxed)),
            ("hold", sm.hold_count.load(Ordering::Relaxed)),
            ("replace", sm.replace_count.load(Ordering::Relaxed)),
        ];
        out.push_str("# HELP cow_solver_submission_timing_total Submission timing decisions.\n");
        out.push_str("# TYPE cow_solver_submission_timing_total counter\n");
        for (decision, count) in &timing {
            out.push_str(&format!(
                "cow_solver_submission_timing_total{{decision=\"{}\"}} {}\n",
                decision, count
            ));
        }
        let risks = [
            ("low", sm.risk_low.load(Ordering::Relaxed)),
            ("medium", sm.risk_medium.load(Ordering::Relaxed)),
            ("high", sm.risk_high.load(Ordering::Relaxed)),
        ];
        out.push_str("# HELP cow_solver_submission_risk_total Submission risk classifications.\n");
        out.push_str("# TYPE cow_solver_submission_risk_total counter\n");
        for (risk, count) in &risks {
            out.push_str(&format!(
                "cow_solver_submission_risk_total{{risk=\"{}\"}} {}\n",
                risk, count
            ));
        }
    }

    // ── Attribution (C.3) ───────────────────────────────────────────────────
    {
        let snapshots = crate::attribution::all_snapshots();
        out.push_str("# HELP cow_solver_attribution_generated_total Candidates generated per strategy.\n");
        out.push_str("# TYPE cow_solver_attribution_generated_total counter\n");
        for (name, snap) in &snapshots {
            out.push_str(&format!(
                "cow_solver_attribution_generated_total{{strategy=\"{}\"}} {}\n",
                name, snap.generated
            ));
        }
        out.push_str("# HELP cow_solver_attribution_submitted_total Candidates submitted per strategy.\n");
        out.push_str("# TYPE cow_solver_attribution_submitted_total counter\n");
        for (name, snap) in &snapshots {
            out.push_str(&format!(
                "cow_solver_attribution_submitted_total{{strategy=\"{}\"}} {}\n",
                name, snap.submitted
            ));
        }
        out.push_str("# HELP cow_solver_attribution_won_total Auctions won per strategy.\n");
        out.push_str("# TYPE cow_solver_attribution_won_total counter\n");
        for (name, snap) in &snapshots {
            out.push_str(&format!(
                "cow_solver_attribution_won_total{{strategy=\"{}\"}} {}\n",
                name, snap.won
            ));
        }
        out.push_str("# HELP cow_solver_attribution_sim_pass_rate Simulation pass rate per strategy.\n");
        out.push_str("# TYPE cow_solver_attribution_sim_pass_rate gauge\n");
        for (name, snap) in &snapshots {
            out.push_str(&format!(
                "cow_solver_attribution_sim_pass_rate{{strategy=\"{}\"}} {:.3}\n",
                name, snap.sim_pass_rate()
            ));
        }
    }

    // ── Pool Indexer (B.1) ───────────────────────────────────────────────────
    {
        gauge!(
            "cow_solver_indexer_current_block",
            "Current chain head block known to the pool indexer.",
            crate::pool_indexer::current_block()
        );
        gauge!(
            "cow_solver_indexer_pool_cache_size",
            "Number of pools in the hot reserve cache.",
            crate::pool_indexer::pool_count() as u64
        );
    }

    // ── RFQ Service (C.1) ─────────────────────────────────────────────────
    {
        let rm = crate::rfq::rfq_metrics();
        counter!(
            "cow_solver_rfq_requests_total",
            "Total RFQ quote requests made.",
            rm.total_requests.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_rfq_successful_total",
            "RFQ requests where at least one provider responded.",
            rm.successful_requests.load(Ordering::Relaxed)
        );
        counter!(
            "cow_solver_rfq_won_total",
            "Times an RFQ quote beat AMM price and was used.",
            rm.rfq_won.load(Ordering::Relaxed)
        );
    }

    // ── State ────────────────────────────────────────────────────────────────
    gauge!(
        "cow_solver_pool_count",
        "Number of liquidity pools currently tracked.",
        pools
    );
    gauge!(
        "cow_solver_uptime_seconds",
        "Seconds since the solver process started.",
        uptime
    );
    gauge!(
        "cow_solver_seconds_since_last_auction",
        "Seconds since the last auction was received (very large if none received yet).",
        if secs_since_auction == u64::MAX { 0 } else { secs_since_auction }
    );

    out
}

// ─── Axum Handler ─────────────────────────────────────────────────────────────

/// GET /metrics — returns Prometheus text exposition format.
pub async fn metrics_handler() -> impl IntoResponse {
    let body = render_prometheus();
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    (StatusCode::OK, headers, body)
}

// ─── Alerting ────────────────────────────────────────────────────────────────

/// Checks all alert conditions, emits structured log events, and fires Telegram
/// alerts (via `tokio::spawn`) for conditions that need human attention.
///
/// Must be called from within a Tokio runtime (the background task qualifies).
pub fn check_alerts() {
    let m = metrics();
    let now = now_secs();
    let last_auction = m.last_auction_sec.load(Ordering::Relaxed);
    let auctions = m.auctions_received.load(Ordering::Relaxed);
    let submitted = m.solutions_submitted.load(Ordering::Relaxed);
    let rpc_calls = m.rpc_calls.load(Ordering::Relaxed);
    let rpc_errors = m.rpc_errors.load(Ordering::Relaxed);

    // CRITICAL: no auctions for 5+ minutes (when we expect them)
    if last_auction > 0 {
        let secs_silent = now.saturating_sub(last_auction);
        if secs_silent >= 300 {
            error!(
                secs_silent,
                alert = "CRITICAL",
                "No auctions received for 5+ minutes — solver may be disconnected from driver"
            );
            let msg = format!(
                "No auctions received for {}s — check CoW driver connection",
                secs_silent
            );
            tokio::spawn(crate::alerts::send_alert("CRITICAL", "no_auctions", msg));
        }
    }

    // WARNING: win rate < 1% after 100+ auctions
    if auctions >= 100 {
        let win_rate = submitted as f64 / auctions as f64;
        if win_rate < 0.01 {
            warn!(
                auctions_received = auctions,
                solutions_submitted = submitted,
                win_rate_pct = win_rate * 100.0,
                alert = "WARNING",
                "Win rate below 1% — solver may not be finding profitable solutions"
            );
            let msg = format!(
                "Win rate {:.2}% after {} auctions ({} submitted) — review scoring",
                win_rate * 100.0,
                auctions,
                submitted
            );
            tokio::spawn(crate::alerts::send_alert("WARNING", "low_win_rate", msg));
        }
    }

    // WARNING: freshness degradation — majority of scores are stale
    {
        let fm = crate::freshness::freshness_metrics();
        let total = fm.total.load(Ordering::Relaxed);
        let stale = fm.stale_count.load(Ordering::Relaxed);
        if total >= 20 {
            let stale_rate = stale as f64 / total as f64;
            if stale_rate > 0.50 {
                warn!(
                    total,
                    stale,
                    stale_rate_pct = stale_rate * 100.0,
                    alert = "WARNING",
                    "Over 50% of freshness scores are stale — data may be severely outdated"
                );
                let msg = format!(
                    "Freshness degradation: {:.0}% stale ({}/{} scores below 0.3) — check RPC and pool data",
                    stale_rate * 100.0, stale, total
                );
                tokio::spawn(crate::alerts::send_alert("WARNING", "freshness_stale", msg));
            }
        }
    }

    // WARNING: RPC error rate > 10%
    if rpc_calls >= 20 {
        let error_rate = rpc_errors as f64 / rpc_calls as f64;
        if error_rate > 0.10 {
            warn!(
                rpc_calls,
                rpc_errors,
                error_rate_pct = error_rate * 100.0,
                alert = "WARNING",
                "RPC error rate above 10% — check RPC endpoint health"
            );
            let msg = format!(
                "RPC error rate {:.1}% ({}/{} calls failed) — check RPC endpoint",
                error_rate * 100.0,
                rpc_errors,
                rpc_calls
            );
            tokio::spawn(crate::alerts::send_alert("WARNING", "rpc_errors", msg));
        }
    }
}

// ─── Background Task ─────────────────────────────────────────────────────────

/// Long-running async task. Spawn once at startup.
///
/// * Checks alerts every 60 seconds.
/// * Logs an hourly summary.
/// * Logs a daily revenue report at midnight UTC.
/// * Persists revenue data to disk every hour.
pub async fn run_background_task() {
    use tokio::time::{Duration, sleep};

    let revenue = RevenueTracker::load_or_new();
    let revenue = std::sync::Arc::new(tokio::sync::Mutex::new(revenue));

    let mut last_hourly = now_secs();
    let mut last_daily = today_utc_day();

    info!("Monitoring background task started.");

    loop {
        sleep(Duration::from_secs(60)).await;

        check_alerts();

        let now = now_secs();
        let today = today_utc_day();

        // Hourly summary
        if now.saturating_sub(last_hourly) >= 3600 {
            last_hourly = now;
            rotate_hour_window();
            log_hourly_summary();

            // Persist revenue to disk
            let rev = revenue.lock().await;
            if let Err(e) = rev.save() {
                warn!(error = %e, "Failed to persist revenue data");
            }
        }

        // Daily summary at midnight UTC (day number changed)
        if today != last_daily {
            last_daily = today;
            let rev = revenue.lock().await;
            rev.log_daily_summary();
        }
    }
}

fn log_hourly_summary() {
    let m = metrics();
    let auctions = m.auctions_received.load(Ordering::Relaxed);
    let submitted = m.solutions_submitted.load(Ordering::Relaxed);
    let empty = m.solutions_empty.load(Ordering::Relaxed);
    let solve_n = m.solve_samples.load(Ordering::Relaxed);
    let solve_ms = m.total_solve_ms.load(Ordering::Relaxed);
    let rpc_calls = m.rpc_calls.load(Ordering::Relaxed);
    let rpc_errors = m.rpc_errors.load(Ordering::Relaxed);
    let pools = m.pool_count.load(Ordering::Relaxed);
    let uptime = now_secs().saturating_sub(m.start_time_sec);

    let avg_ms = if solve_n > 0 { solve_ms / solve_n } else { 0 };
    let win_rate: f64 = if auctions > 0 { submitted as f64 / auctions as f64 * 100.0 } else { 0.0 };
    let rpc_err_pct: f64 = if rpc_calls > 0 { rpc_errors as f64 / rpc_calls as f64 * 100.0 } else { 0.0 };

    info!(
        summary = "hourly",
        auctions_received = auctions,
        solutions_submitted = submitted,
        solutions_empty = empty,
        win_rate_pct = win_rate,
        avg_solve_ms = avg_ms,
        rpc_calls = rpc_calls,
        rpc_error_pct = rpc_err_pct,
        pool_count = pools,
        uptime_seconds = uptime,
        "Hourly solver summary"
    );
}

// ─── Utilities ────────────────────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Returns the number of days since the Unix epoch (UTC day counter).
fn today_utc_day() -> u64 {
    now_secs() / 86400
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_output_contains_expected_keys() {
        let out = render_prometheus();
        assert!(out.contains("cow_solver_auctions_received_total"));
        assert!(out.contains("cow_solver_solutions_submitted_total"));
        assert!(out.contains("cow_solver_win_rate_pct"));
        assert!(out.contains("cow_solver_rpc_calls_total"));
        assert!(out.contains("cow_solver_pool_count"));
        assert!(out.contains("cow_solver_uptime_seconds"));
    }

    #[test]
    fn record_solve_increments_counters() {
        // Use fresh metrics without polluting global state by observing deltas
        let m = metrics();
        let before = m.auctions_received.load(Ordering::Relaxed);
        let before_sub = m.solutions_submitted.load(Ordering::Relaxed);

        record_solve(42, 2, 1_000_000_000, true);

        let after = m.auctions_received.load(Ordering::Relaxed);
        let after_sub = m.solutions_submitted.load(Ordering::Relaxed);

        assert_eq!(after, before + 1);
        assert_eq!(after_sub, before_sub + 1);
    }

    #[test]
    fn record_solve_held_increments_held_counter() {
        let m = metrics();
        let before = m.solutions_held.load(Ordering::Relaxed);
        record_solve(10, 2, 1_000_000_000, false);
        let after = m.solutions_held.load(Ordering::Relaxed);
        assert_eq!(after, before + 1);
    }

    #[test]
    fn record_solve_empty_increments_empty_counter() {
        let m = metrics();
        let before = m.solutions_empty.load(Ordering::Relaxed);
        record_solve(10, 0, 0, false);
        let after = m.solutions_empty.load(Ordering::Relaxed);
        assert_eq!(after, before + 1);
    }

    #[test]
    fn record_rpc_error() {
        let m = metrics();
        let before_err = m.rpc_errors.load(Ordering::Relaxed);
        record_rpc(5, true);
        let after_err = m.rpc_errors.load(Ordering::Relaxed);
        assert_eq!(after_err, before_err + 1);
    }

    #[test]
    fn record_strategy_increments_correct_counter() {
        let m = metrics();
        let before = m.strategy_direct.load(Ordering::Relaxed);
        record_strategy("direct");
        let after = m.strategy_direct.load(Ordering::Relaxed);
        assert_eq!(after, before + 1);
    }

    #[test]
    fn record_fallback_increments_counter() {
        let m = metrics();
        let before = m.fallback_used.load(Ordering::Relaxed);
        record_fallback();
        let after = m.fallback_used.load(Ordering::Relaxed);
        assert_eq!(after, before + 1);
    }

    #[test]
    fn prometheus_output_contains_strategy_metrics() {
        record_strategy("cow");
        let out = render_prometheus();
        assert!(out.contains("cow_solver_strategy_submitted_total"));
        assert!(out.contains("cow_solver_fallback_used_total"));
        assert!(out.contains(r#"strategy="cow""#));
    }
}
