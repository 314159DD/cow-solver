//! State Freshness Scoring (B.4)
//!
//! Per-candidate confidence score reflecting how stale the underlying data is.
//! Must complete within 5ms — in-memory only, no I/O.
//!
//! ## Kill Switch
//! Set `FRESHNESS_GATE_ENABLED=false` to skip freshness gating.
//!
//! ## Freshness Dimensions
//!
//! | Signal | Weight | Source |
//! |--------|--------|--------|
//! | Source block age | High | current_block - cache_block |
//! | RPC lag | Medium | measured round-trip to RPC |
//! | RFQ quote age | High | now - quote_timestamp |
//! | Simulation block mismatch | Critical | sim_block != current_block |

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tracing::debug;

// ── Types ───────────────────────────────────────────────────────────────────

/// Which dimension dragged confidence down the most.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessDimension {
    /// Pool data is stale (block age)
    BlockAge,
    /// RPC latency is high
    RpcLag,
    /// RFQ quote is old
    RfqAge,
    /// Simulation was against a different block
    SimBlockMismatch,
    /// All dimensions are fresh
    AllFresh,
}

impl FreshnessDimension {
    pub fn as_str(&self) -> &'static str {
        match self {
            FreshnessDimension::BlockAge => "block_age",
            FreshnessDimension::RpcLag => "rpc_lag",
            FreshnessDimension::RfqAge => "rfq_age",
            FreshnessDimension::SimBlockMismatch => "sim_block_mismatch",
            FreshnessDimension::AllFresh => "all_fresh",
        }
    }
}

/// Freshness score for a candidate solution.
#[derive(Debug, Clone)]
pub struct FreshnessScore {
    /// 0.0 = completely stale, 1.0 = maximally fresh
    pub confidence: f64,
    /// Which dimension dragged confidence down most
    pub bottleneck: FreshnessDimension,
    /// Block number this score was computed at
    pub as_of_block: u64,
}

impl FreshnessScore {
    /// A conservative fallback score (used when scoring fails).
    pub fn conservative() -> Self {
        Self {
            confidence: 0.1,
            bottleneck: FreshnessDimension::BlockAge,
            as_of_block: 0,
        }
    }

    /// A maximally fresh score.
    pub fn fresh(block: u64) -> Self {
        Self {
            confidence: 1.0,
            bottleneck: FreshnessDimension::AllFresh,
            as_of_block: block,
        }
    }
}

// ── Input signals ───────────────────────────────────────────────────────────

/// All signals needed to compute a freshness score.
#[derive(Debug, Clone)]
pub struct FreshnessInput {
    /// Current block number from the auction
    pub current_block: u64,
    /// Block number when pool data was last refreshed
    pub cache_block: u64,
    /// RPC round-trip latency in milliseconds (0 if unknown)
    pub rpc_latency_ms: u64,
    /// Age of RFQ quote in milliseconds (u64::MAX if no RFQ)
    pub rfq_quote_age_ms: u64,
    /// Block used for simulation (0 if not simulated)
    pub sim_block: u64,
}

// ── Scoring ─────────────────────────────────────────────────────────────────

/// Weights for each freshness dimension.
const WEIGHT_BLOCK_AGE: f64 = 0.40;
const WEIGHT_RPC_LAG: f64 = 0.20;
const WEIGHT_RFQ_AGE: f64 = 0.25;
const WEIGHT_SIM_MISMATCH: f64 = 0.15;

/// Block age thresholds (Arbitrum: ~250ms per block)
const BLOCK_AGE_FRESH: u64 = 2;    // 0-2 blocks = fresh
const BLOCK_AGE_STALE: u64 = 10;   // 10+ blocks = stale

/// RPC latency thresholds
const RPC_FRESH_MS: u64 = 50;      // <50ms = fresh
const RPC_STALE_MS: u64 = 500;     // >500ms = stale

/// RFQ quote age thresholds
const RFQ_FRESH_MS: u64 = 500;     // <500ms = fresh
const RFQ_STALE_MS: u64 = 3000;    // >3s = stale

/// Compute a freshness score from the input signals.
///
/// Each dimension is scored 0.0-1.0, then weighted and combined.
/// The bottleneck is the dimension with the lowest individual score.
///
/// Must complete within 5ms — pure arithmetic, no I/O.
pub fn score(input: &FreshnessInput) -> FreshnessScore {
    // Dimension 1: Block age
    let block_age = input.current_block.saturating_sub(input.cache_block);
    let block_score = if block_age <= BLOCK_AGE_FRESH {
        1.0
    } else if block_age >= BLOCK_AGE_STALE {
        0.0
    } else {
        1.0 - (block_age - BLOCK_AGE_FRESH) as f64 / (BLOCK_AGE_STALE - BLOCK_AGE_FRESH) as f64
    };

    // Dimension 2: RPC lag
    let rpc_score = if input.rpc_latency_ms == 0 {
        0.8 // unknown, assume decent
    } else if input.rpc_latency_ms <= RPC_FRESH_MS {
        1.0
    } else if input.rpc_latency_ms >= RPC_STALE_MS {
        0.0
    } else {
        1.0 - (input.rpc_latency_ms - RPC_FRESH_MS) as f64 / (RPC_STALE_MS - RPC_FRESH_MS) as f64
    };

    // Dimension 3: RFQ quote age
    let rfq_score = if input.rfq_quote_age_ms == u64::MAX {
        0.8 // no RFQ, not a penalty
    } else if input.rfq_quote_age_ms <= RFQ_FRESH_MS {
        1.0
    } else if input.rfq_quote_age_ms >= RFQ_STALE_MS {
        0.0
    } else {
        1.0 - (input.rfq_quote_age_ms - RFQ_FRESH_MS) as f64 / (RFQ_STALE_MS - RFQ_FRESH_MS) as f64
    };

    // Dimension 4: Simulation block mismatch
    let sim_score = if input.sim_block == 0 {
        0.7 // not simulated, mild penalty
    } else if input.sim_block == input.current_block {
        1.0
    } else if input.sim_block + 1 == input.current_block {
        0.8 // one block behind, acceptable
    } else {
        0.2 // more than 1 block behind, dangerous
    };

    // Find bottleneck
    let scores = [
        (block_score, FreshnessDimension::BlockAge),
        (rpc_score, FreshnessDimension::RpcLag),
        (rfq_score, FreshnessDimension::RfqAge),
        (sim_score, FreshnessDimension::SimBlockMismatch),
    ];
    let (min_score, bottleneck) = scores
        .iter()
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(s, d)| (*s, *d))
        .unwrap_or((0.0, FreshnessDimension::BlockAge));

    let bottleneck = if min_score >= 0.9 {
        FreshnessDimension::AllFresh
    } else {
        bottleneck
    };

    // Weighted combination
    let confidence = (block_score * WEIGHT_BLOCK_AGE
        + rpc_score * WEIGHT_RPC_LAG
        + rfq_score * WEIGHT_RFQ_AGE
        + sim_score * WEIGHT_SIM_MISMATCH)
        .clamp(0.0, 1.0);

    debug!(
        confidence,
        bottleneck = bottleneck.as_str(),
        block_age,
        block_score,
        rpc_score,
        rfq_score,
        sim_score,
        "Freshness scored"
    );

    record_freshness(confidence);

    FreshnessScore {
        confidence,
        bottleneck,
        as_of_block: input.current_block,
    }
}

// ── Kill Switch ─────────────────────────────────────────────────────────────

/// Minimum freshness confidence to allow submission (default: 0.3).
pub const DEFAULT_FRESHNESS_THRESHOLD: f64 = 0.3;

/// Check if freshness gating is enabled.
pub fn is_freshness_gate_enabled() -> bool {
    std::env::var("FRESHNESS_GATE_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

/// Check if a freshness score passes the submission threshold.
pub fn passes_threshold(score: &FreshnessScore) -> bool {
    if !is_freshness_gate_enabled() {
        return true;
    }
    let threshold = std::env::var("FRESHNESS_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_FRESHNESS_THRESHOLD);
    score.confidence >= threshold
}

// ── Metrics ─────────────────────────────────────────────────────────────────

pub struct FreshnessMetrics {
    /// Total freshness scores computed
    pub total: AtomicU64,
    /// Scores above 0.8 (fresh)
    pub fresh_count: AtomicU64,
    /// Scores between 0.3 and 0.8 (acceptable)
    pub acceptable_count: AtomicU64,
    /// Scores below 0.3 (stale)
    pub stale_count: AtomicU64,
    /// Sum of confidence * 1000 for average calculation
    pub confidence_sum_milli: AtomicU64,
}

impl FreshnessMetrics {
    fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            fresh_count: AtomicU64::new(0),
            acceptable_count: AtomicU64::new(0),
            stale_count: AtomicU64::new(0),
            confidence_sum_milli: AtomicU64::new(0),
        }
    }
}

static FRESHNESS_METRICS: OnceLock<FreshnessMetrics> = OnceLock::new();

pub fn freshness_metrics() -> &'static FreshnessMetrics {
    FRESHNESS_METRICS.get_or_init(FreshnessMetrics::new)
}

fn record_freshness(confidence: f64) {
    let m = freshness_metrics();
    m.total.fetch_add(1, Ordering::Relaxed);
    m.confidence_sum_milli.fetch_add((confidence * 1000.0) as u64, Ordering::Relaxed);
    if confidence >= 0.8 {
        m.fresh_count.fetch_add(1, Ordering::Relaxed);
    } else if confidence >= 0.3 {
        m.acceptable_count.fetch_add(1, Ordering::Relaxed);
    } else {
        m.stale_count.fetch_add(1, Ordering::Relaxed);
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfectly_fresh_input() {
        let input = FreshnessInput {
            current_block: 100,
            cache_block: 100,
            rpc_latency_ms: 20,
            rfq_quote_age_ms: 200,
            sim_block: 100,
        };
        let result = score(&input);
        assert!(result.confidence > 0.9, "confidence={}", result.confidence);
        assert_eq!(result.bottleneck, FreshnessDimension::AllFresh);
    }

    #[test]
    fn stale_block_drags_confidence() {
        let input = FreshnessInput {
            current_block: 100,
            cache_block: 80, // 20 blocks behind
            rpc_latency_ms: 20,
            rfq_quote_age_ms: u64::MAX,
            sim_block: 100,
        };
        let result = score(&input);
        assert!(result.confidence < 0.7, "confidence={}", result.confidence);
        assert_eq!(result.bottleneck, FreshnessDimension::BlockAge);
    }

    #[test]
    fn high_rpc_latency_reduces_confidence() {
        let input = FreshnessInput {
            current_block: 100,
            cache_block: 100,
            rpc_latency_ms: 1000, // 1s — very high
            rfq_quote_age_ms: u64::MAX,
            sim_block: 100,
        };
        let result = score(&input);
        assert!(result.confidence < 0.9, "confidence={}", result.confidence);
    }

    #[test]
    fn sim_block_mismatch_penalty() {
        let input = FreshnessInput {
            current_block: 100,
            cache_block: 100,
            rpc_latency_ms: 20,
            rfq_quote_age_ms: u64::MAX,
            sim_block: 95, // 5 blocks behind
        };
        let result = score(&input);
        assert!(result.confidence < 0.9, "confidence={}", result.confidence);
        assert_eq!(result.bottleneck, FreshnessDimension::SimBlockMismatch);
    }

    #[test]
    fn no_rfq_no_sim_is_acceptable() {
        let input = FreshnessInput {
            current_block: 100,
            cache_block: 99,
            rpc_latency_ms: 0,
            rfq_quote_age_ms: u64::MAX,
            sim_block: 0,
        };
        let result = score(&input);
        // Should still be above threshold
        assert!(result.confidence >= DEFAULT_FRESHNESS_THRESHOLD);
    }

    #[test]
    fn conservative_fallback() {
        let f = FreshnessScore::conservative();
        assert_eq!(f.confidence, 0.1);
        assert!(!passes_threshold(&f)); // below default 0.3
    }

    #[test]
    fn fresh_passes_threshold() {
        let f = FreshnessScore::fresh(100);
        assert!(passes_threshold(&f));
    }

    #[test]
    fn metrics_tracked() {
        let m = freshness_metrics();
        let before = m.total.load(Ordering::Relaxed);
        let input = FreshnessInput {
            current_block: 100,
            cache_block: 100,
            rpc_latency_ms: 20,
            rfq_quote_age_ms: u64::MAX,
            sim_block: 0,
        };
        let _ = score(&input);
        // Use >= because other concurrent tests may also increment the global counter
        assert!(m.total.load(Ordering::Relaxed) >= before + 1);
    }
}
