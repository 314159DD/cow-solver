# Sprint: Truth & Calibration

**Goal:** Make the solver's economic decisions trustworthy and the dashboard a real debugging tool, not a vanity board.

**Owner:** Agentic team
**Priority:** This sprint before any new strategy work.
**Context:** Shadow mode on Arbitrum. Solver is operationally stable (0 errors, 300ms solve times) but we can't yet trust whether it's making correct economic decisions.

---

## 1. Fix Score Truncation Bug (CRITICAL)

**File:** `solver-engine/src/monitoring/mod.rs` line 258

**Problem:** `let wei = score_wei as u64` silently truncates scores > `u64::MAX` (18.4e18 wei = 18.4 ETH). Real Arbitrum surplus scores can reach 1.6e19 wei, causing:
- Truncated min/max values
- Corrupted sum → wrong average
- Dashboard showing avg < min (mathematically impossible with correct data)

**Fix options (pick one):**
- **A) Store as two u64s** (high + low 64 bits) - most precise, more complex
- **B) Store in gwei** (divide by 1e9 before storing) - loses sub-gwei precision but u64 holds up to 18.4e9 gwei (~18.4 ETH), still overflows for large scores
- **C) Use AtomicU128** - cleanest fix but requires `#![feature(integer_atomics)]` or a Mutex wrapper
- **D) Cap and log** - `score_wei.min(u64::MAX as u128) as u64` + warn on truncation - quick fix, reveals frequency

**Recommendation:** Option D as immediate fix (5 min), then Option C as proper fix. The cap prevents silent corruption; the log tells you how often scores exceed u64 range.

**Validation:** After fix, verify on dashboard that `avg >= min` and `min <= max` always hold. Add a sanity assertion in `build_dashboard_data()`.

---

## 2. Add Score Sanity Checks to Dashboard

**File:** `solver-engine/src/routes/dashboard.rs`

**Add to `PerformanceStats` or a new `ScoreSanity` struct:**

```rust
pub struct ScoreSanity {
    pub unit: &'static str,           // "wei"
    pub window: &'static str,         // "since_restart"
    pub sample_count: u64,
    pub non_zero_count: u64,
    pub truncated_count: u64,         // scores that hit u64 cap
    pub median_wei: u64,              // approximate from histogram-style buckets
    pub p95_wei: u64,
    pub warnings: Vec<String>,        // ["avg < min", "min > max", etc.]
}
```

**In the HTML dashboard**, add a warning banner if any sanity check fails. The dashboard currently looks authoritative but can show impossible numbers.

---

## 3. Per-Strategy Rejection Reasons

**File:** `solver-engine/src/attribution.rs`

**Problem:** Attribution tracks *what happened* (generated/sim_passed/submitted) but not *why a strategy lost*. When direct generates 12 solutions and submits 0, we can't tell if it's because:
- Split had higher scores (expected)
- Direct solutions failed pricing validation
- Direct solutions had wrong gas treatment
- Direct was killed by some threshold

**Add:**

```rust
pub enum RejectionReason {
    LostToHigherScore { winner: String, winner_score: u128, our_score: u128 },
    BelowMinimumImprovement { improvement_bps: u128, threshold_bps: u128 },
    NoRoutesAvailable,
    SimulationFailed(String),
    PolicyRejected(String),
    TimeoutExceeded,
    PricingValidationFailed,
}
```

**In `solver/mod.rs`** (the phase orchestrator), when a phase's best score doesn't beat the current best, log the rejection:

```rust
if phase_score <= best_score {
    attribution::record_rejection("direct", RejectionReason::LostToHigherScore {
        winner: best_strategy.clone(),
        winner_score: best_score,
        our_score: phase_score,
    });
}
```

**Dashboard addition:** Per-strategy row should show a "top rejection reason" column (e.g., "lost to split (12x), no routes (0x)").

---

## 4. Strategy Score Comparison Table

**File:** `solver-engine/src/routes/dashboard.rs`

**Add a new section to DashboardData:**

```rust
pub struct StrategyComparison {
    pub name: String,
    pub generated: u64,
    pub selected: u64,
    pub avg_raw_score: u64,
    pub avg_net_score: u64,       // after gas
    pub avg_gas_estimate: u64,
    pub sim_pass_rate: f64,
    pub avg_solve_ms: u64,        // time this strategy took
    pub top_rejection: String,    // most common rejection reason
}
```

This is the single most important missing view. It turns "split always wins" from a mystery into a diagnosis.

---

## 5. Triage Calibration

**File:** `solver-engine/src/triage.rs`

**Problem:** Any known token + 1 liquidity source = "profitable". With bootstrapped Arbitrum pools, every real auction qualifies. Triage is rubber-stamping.

**Fixes:**
1. Add an order-size threshold for "profitable" - e.g., surplus must exceed estimated gas cost
2. Add a "marginal" band - orders where surplus is 1-3x gas cost
3. Log triage reasoning per order, not just per auction
4. Add token pair frequency tracking to dashboard (which pairs are we seeing?)

**Expected result:** Triage panel should show a realistic mix over time (60-70% profitable, 20% marginal, 10% skip/unwinnable).

---

## 6. Honest Simulation Labeling

**File:** `solver-engine/src/simulation.rs`

**Problem:** "Simulation" currently checks `!trades.is_empty() && !prices.is_empty()`. That's structural validation, not simulation. Dashboard showing "12/12 sim pass" implies EVM verification that isn't happening.

**Immediate fix (no code change needed for sim itself):**
1. Rename dashboard label from "Simulation" to "Structural Validation" or add a subtitle "(structural only, no EVM fork)"
2. Add a `sim_mode` field to the dashboard: `"structural"` | `"anvil_fork"` | `"tenderly"`
3. When actual Anvil fork sim is implemented, flip the mode

**This manages expectations** - the team (and external reviewers) should know what "pass" means.

---

## 7. Auction Decision Explainer

**Files:** `solver-engine/src/solver/mod.rs`, `solver-engine/src/routes/dashboard.rs`

**For the last N auctions, store and display:**

```rust
pub struct AuctionDecision {
    pub auction_id: String,
    pub selected_strategy: String,
    pub selected_score: u128,
    pub runner_up_strategy: Option<String>,
    pub runner_up_score: Option<u128>,
    pub score_delta_bps: Option<u128>,
    pub phase_reached: u8,
    pub reason_selected: String,       // "highest_score" | "only_candidate" | "time_limit"
    pub reasons_others_lost: Vec<(String, String)>,  // [("direct", "score 15% lower")]
}
```

**Store in replay.db** alongside the existing auction_log. Display as a compact table in the dashboard.

This turns the dashboard from "status display" into an optimization tool.

---

## 8. Shadow Competitiveness Panel (HIGHEST VALUE)

**Problem:** We don't know if our solutions would have been competitive against actual winners.

**Approach:**
1. After submitting a solution, query the CoW API for the auction result (delayed by ~60s)
2. Store: `our_score`, `winner_score`, `winner_solver`, `our_rank`
3. Dashboard panel:
   - Win rate (our score was highest)
   - Median delta vs winner (how far off)
   - Auctions within 10% of winner
   - Trend over time

**API endpoint:** `https://api.cow.fi/arbitrum/api/v1/solver_competition/by_tx_hash/{hash}` or by auction ID.

**This is the most important metric for the whole solver.** Everything else is internal housekeeping. This tells you "are we actually competitive?"

---

## 9. Dead Strategy Detector

**File:** `solver-engine/src/routes/dashboard.rs`

**Simple rule:** If a strategy has `generated > 50` and `selected == 0`, flag it in the dashboard with a warning badge.

Currently `cow`, `combined`, `multihop`, and `graph` are all in this category. Either:
- They're not wired up yet (expected in shadow)
- They're wired up but structurally broken
- They're wired up but split always beats them

The dashboard should distinguish these states.

---

## Priority Order

| # | Task | Effort | Impact | Blocks |
|---|------|--------|--------|--------|
| 1 | Fix score truncation (u128→u64) | 30 min | Critical | Dashboard trust |
| 2 | Score sanity checks + warnings | 1 hr | High | Decision quality |
| 3 | Honest simulation labeling | 15 min | Medium | Expectation mgmt |
| 4 | Per-strategy rejection reasons | 3 hrs | High | Strategy debugging |
| 5 | Strategy comparison table | 2 hrs | High | Optimization |
| 6 | Auction decision explainer | 3 hrs | High | Per-auction debugging |
| 7 | Triage calibration | 2 hrs | Medium | Filtering quality |
| 8 | Dead strategy detector | 30 min | Low | Housekeeping |
| 9 | Shadow competitiveness panel | 4 hrs | Highest | Competition readiness |

**Items 1-3 should be done immediately.** They're small fixes that prevent bad decisions.
**Items 4-6 are the core of this sprint.** They make the dashboard a real tool.
**Items 7-9 complete the picture** but can overlap with next sprint.

---

## What NOT to do this sprint

- No new strategies
- No new DEX integrations
- No latency optimization
- No infra changes

This sprint is about **knowing the truth**, not building more features on uncertain foundations.

---

## Definition of Done

- [ ] Dashboard score cards are internally consistent (avg >= min <= max)
- [ ] Every strategy shows its top rejection reason
- [ ] Simulation label honestly reflects what it tests
- [ ] At least 1 day of shadow data shows realistic triage mix
- [ ] Auction decision log shows selected vs. runner-up per auction
- [ ] Shadow competitiveness panel shows our rank vs. actual winners
