//! Submission Policy Engine (C.2)
//!
//! Decides how/when/whether to submit a candidate solution.
//! This is the "biggest missing piece" from v1 — finding the best route
//! is necessary but not sufficient.
//!
//! ## Kill Switches
//! - `SUBMISSION_REPLACE_ENABLED=false` — never replace a submitted solution
//! - `PROTECTED_SUBMIT_ENABLED=false` — all submissions go standard path
//!
//! ## Latency Budget: 50ms

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use tracing::debug;

use crate::freshness::FreshnessScore;

// ── Risk Classification ─────────────────────────────────────────────────────

/// Risk level for a candidate solution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskClass {
    /// Low risk — high freshness, known tokens, simulated successfully
    Low,
    /// Medium risk — acceptable freshness, some unknowns
    Medium,
    /// High risk — stale data, large amounts, or simulation issues
    High,
}

impl RiskClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskClass::Low => "low",
            RiskClass::Medium => "medium",
            RiskClass::High => "high",
        }
    }
}

// ── Submission Mode ─────────────────────────────────────────────────────────

/// How to submit the solution to the CoW driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionMode {
    /// Standard submission via POST response
    Standard,
    /// Protected submission (MEV-protected relay)
    Protected,
}

impl SubmissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubmissionMode::Standard => "standard",
            SubmissionMode::Protected => "protected",
        }
    }
}

// ── Submission Decision ─────────────────────────────────────────────────────

/// Timing decision: what to do with this candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingDecision {
    /// Submit this solution now
    Submit,
    /// Hold — wait for better data or score improvement
    Hold,
    /// Replace a previously submitted solution with this better one
    Replace,
}

impl TimingDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            TimingDecision::Submit => "submit",
            TimingDecision::Hold => "hold",
            TimingDecision::Replace => "replace",
        }
    }
}

/// Complete submission policy decision for a candidate.
#[derive(Debug, Clone)]
pub struct SubmissionDecision {
    pub risk_class: RiskClass,
    pub mode: SubmissionMode,
    pub timing: TimingDecision,
    pub expected_value_wei: i128,
    pub reason: &'static str,
}

// ── Policy Input ────────────────────────────────────────────────────────────

/// All signals needed to make a submission decision.
pub struct PolicyInput {
    /// Candidate's score (surplus - gas, in wei)
    pub score_wei: i128,
    /// Freshness of underlying data
    pub freshness: FreshnessScore,
    /// Whether simulation passed
    pub simulated: bool,
    /// Whether simulation reverted
    pub sim_reverted: bool,
    /// Seconds remaining until auction deadline
    pub time_remaining_secs: u64,
    /// Score of the previously submitted solution for this auction (0 if none)
    pub previous_score_wei: i128,
    /// Estimated gas cost for replacement submission
    pub replacement_gas_wei: u128,
    /// Whether any order involves large amounts (> 1 ETH)
    pub has_large_orders: bool,
}

// ── Policy Engine ───────────────────────────────────────────────────────────

/// Evaluate submission policy for a candidate solution.
///
/// Must complete within 50ms — pure logic, no I/O.
pub fn evaluate(input: &PolicyInput) -> SubmissionDecision {
    // Step 1: Risk classification
    let risk_class = classify_risk(input);

    // Step 2: Submission mode
    let mode = choose_mode(input, risk_class);

    // Step 3: Timing decision
    let (timing, reason) = choose_timing(input, risk_class);

    // Step 4: Expected value
    let expected_value_wei = compute_expected_value(input, risk_class);

    let decision = SubmissionDecision {
        risk_class,
        mode,
        timing,
        expected_value_wei,
        reason,
    };

    debug!(
        risk = risk_class.as_str(),
        mode = mode.as_str(),
        timing = timing.as_str(),
        expected_value_wei,
        freshness = input.freshness.confidence,
        reason,
        "Submission policy decision"
    );

    record_decision(&decision);
    decision
}

fn classify_risk(input: &PolicyInput) -> RiskClass {
    // Simulation reverted = always high risk
    if input.sim_reverted {
        return RiskClass::High;
    }

    // Low freshness = high risk
    if input.freshness.confidence < 0.3 {
        return RiskClass::High;
    }

    // Not simulated + large orders = medium risk
    if !input.simulated && input.has_large_orders {
        return RiskClass::Medium;
    }

    // Simulated + fresh + reasonable score = low risk
    if input.simulated && input.freshness.confidence >= 0.7 {
        return RiskClass::Low;
    }

    RiskClass::Medium
}

fn choose_mode(input: &PolicyInput, risk: RiskClass) -> SubmissionMode {
    // Kill switch
    let protected_enabled = std::env::var("PROTECTED_SUBMIT_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);

    if !protected_enabled {
        return SubmissionMode::Standard;
    }

    // Large orders with medium+ risk → protected
    if input.has_large_orders && risk != RiskClass::Low {
        return SubmissionMode::Protected;
    }

    SubmissionMode::Standard
}

fn choose_timing(input: &PolicyInput, risk: RiskClass) -> (TimingDecision, &'static str) {
    // Kill switch for replacement
    let replace_enabled = std::env::var("SUBMISSION_REPLACE_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);

    // Simulation reverted → hold (don't submit known-bad solutions)
    if input.sim_reverted {
        return (TimingDecision::Hold, "simulation_reverted");
    }

    // Negative score → hold
    if input.score_wei <= 0 {
        return (TimingDecision::Hold, "negative_score");
    }

    // Time pressure: less than 3 seconds remaining → submit whatever we have
    if input.time_remaining_secs < 3 {
        return (TimingDecision::Submit, "time_pressure");
    }

    // Very stale data + high risk → hold if we have time
    if risk == RiskClass::High && input.time_remaining_secs > 10 {
        return (TimingDecision::Hold, "high_risk_waiting");
    }

    // Check if replacement makes sense
    if input.previous_score_wei > 0 && replace_enabled {
        let improvement = input.score_wei - input.previous_score_wei;
        if improvement > input.replacement_gas_wei as i128 * 2 {
            // Score improvement more than covers replacement gas (2x margin)
            return (TimingDecision::Replace, "score_improvement");
        }
    }

    // Default: submit
    (TimingDecision::Submit, "standard")
}

fn compute_expected_value(input: &PolicyInput, risk: RiskClass) -> i128 {
    let base_ev = input.score_wei;

    // Risk discount
    let risk_multiplier = match risk {
        RiskClass::Low => 0.95,
        RiskClass::Medium => 0.80,
        RiskClass::High => 0.50,
    };

    // Freshness discount
    let freshness_multiplier = input.freshness.confidence.max(0.1);

    (base_ev as f64 * risk_multiplier * freshness_multiplier) as i128
}

// ── Metrics ─────────────────────────────────────────────────────────────────

pub struct SubmissionMetrics {
    pub total_decisions: AtomicU64,
    pub submit_count: AtomicU64,
    pub hold_count: AtomicU64,
    pub replace_count: AtomicU64,
    pub risk_low: AtomicU64,
    pub risk_medium: AtomicU64,
    pub risk_high: AtomicU64,
}

impl SubmissionMetrics {
    fn new() -> Self {
        Self {
            total_decisions: AtomicU64::new(0),
            submit_count: AtomicU64::new(0),
            hold_count: AtomicU64::new(0),
            replace_count: AtomicU64::new(0),
            risk_low: AtomicU64::new(0),
            risk_medium: AtomicU64::new(0),
            risk_high: AtomicU64::new(0),
        }
    }
}

static SUBMISSION_METRICS: OnceLock<SubmissionMetrics> = OnceLock::new();

pub fn submission_metrics() -> &'static SubmissionMetrics {
    SUBMISSION_METRICS.get_or_init(SubmissionMetrics::new)
}

fn record_decision(d: &SubmissionDecision) {
    let m = submission_metrics();
    m.total_decisions.fetch_add(1, Ordering::Relaxed);
    match d.timing {
        TimingDecision::Submit => m.submit_count.fetch_add(1, Ordering::Relaxed),
        TimingDecision::Hold => m.hold_count.fetch_add(1, Ordering::Relaxed),
        TimingDecision::Replace => m.replace_count.fetch_add(1, Ordering::Relaxed),
    };
    match d.risk_class {
        RiskClass::Low => m.risk_low.fetch_add(1, Ordering::Relaxed),
        RiskClass::Medium => m.risk_medium.fetch_add(1, Ordering::Relaxed),
        RiskClass::High => m.risk_high.fetch_add(1, Ordering::Relaxed),
    };
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::freshness::{FreshnessScore, FreshnessDimension};

    fn fresh_score() -> FreshnessScore {
        FreshnessScore { confidence: 0.95, bottleneck: FreshnessDimension::AllFresh, as_of_block: 100 }
    }

    fn stale_score() -> FreshnessScore {
        FreshnessScore { confidence: 0.1, bottleneck: FreshnessDimension::BlockAge, as_of_block: 100 }
    }

    #[test]
    fn good_candidate_submits() {
        let input = PolicyInput {
            score_wei: 1_000_000_000,
            freshness: fresh_score(),
            simulated: true,
            sim_reverted: false,
            time_remaining_secs: 15,
            previous_score_wei: 0,
            replacement_gas_wei: 0,
            has_large_orders: false,
        };
        let d = evaluate(&input);
        assert_eq!(d.timing, TimingDecision::Submit);
        assert_eq!(d.risk_class, RiskClass::Low);
        assert!(d.expected_value_wei > 0);
    }

    #[test]
    fn sim_reverted_holds() {
        let input = PolicyInput {
            score_wei: 1_000_000_000,
            freshness: fresh_score(),
            simulated: true,
            sim_reverted: true,
            time_remaining_secs: 15,
            previous_score_wei: 0,
            replacement_gas_wei: 0,
            has_large_orders: false,
        };
        let d = evaluate(&input);
        assert_eq!(d.timing, TimingDecision::Hold);
        assert_eq!(d.risk_class, RiskClass::High);
    }

    #[test]
    fn negative_score_holds() {
        let input = PolicyInput {
            score_wei: -500,
            freshness: fresh_score(),
            simulated: true,
            sim_reverted: false,
            time_remaining_secs: 15,
            previous_score_wei: 0,
            replacement_gas_wei: 0,
            has_large_orders: false,
        };
        let d = evaluate(&input);
        assert_eq!(d.timing, TimingDecision::Hold);
    }

    #[test]
    fn time_pressure_forces_submit() {
        let input = PolicyInput {
            score_wei: 100,
            freshness: stale_score(),
            simulated: false,
            sim_reverted: false,
            time_remaining_secs: 2, // <3s
            previous_score_wei: 0,
            replacement_gas_wei: 0,
            has_large_orders: true,
        };
        let d = evaluate(&input);
        assert_eq!(d.timing, TimingDecision::Submit);
        assert_eq!(d.reason, "time_pressure");
    }

    #[test]
    fn replacement_when_score_improves() {
        let input = PolicyInput {
            score_wei: 2_000_000_000,
            freshness: fresh_score(),
            simulated: true,
            sim_reverted: false,
            time_remaining_secs: 15,
            previous_score_wei: 500_000_000,
            replacement_gas_wei: 100_000_000,
            has_large_orders: false,
        };
        let d = evaluate(&input);
        assert_eq!(d.timing, TimingDecision::Replace);
    }

    #[test]
    fn large_orders_medium_risk_get_protected() {
        let input = PolicyInput {
            score_wei: 1_000_000_000,
            freshness: FreshnessScore { confidence: 0.5, bottleneck: FreshnessDimension::RpcLag, as_of_block: 100 },
            simulated: false,
            sim_reverted: false,
            time_remaining_secs: 15,
            previous_score_wei: 0,
            replacement_gas_wei: 0,
            has_large_orders: true,
        };
        let d = evaluate(&input);
        assert_eq!(d.mode, SubmissionMode::Protected);
    }

    #[test]
    fn stale_high_risk_holds_when_time_available() {
        let input = PolicyInput {
            score_wei: 500_000_000,
            freshness: stale_score(),
            simulated: false,
            sim_reverted: false,
            time_remaining_secs: 20,
            previous_score_wei: 0,
            replacement_gas_wei: 0,
            has_large_orders: false,
        };
        let d = evaluate(&input);
        assert_eq!(d.timing, TimingDecision::Hold);
        assert_eq!(d.risk_class, RiskClass::High);
    }
}
