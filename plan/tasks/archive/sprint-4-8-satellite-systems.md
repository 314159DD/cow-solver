# CoW Solver - Satellite Systems & Production Hardening (v2)

**Date**: 2026-03-26 (revised 2026-03-26 after expert review)
**Author**: CTO
**Audience**: Agentic engineering team
**Repo**: `cow-solver`
**Current state**: Sprint 4 COMPLETE (ops foundation). Sprints 1-3 done (solver engine, 7 strategies, 13 DEXs, 399 tests, shadow endpoint live, awaiting CoW routing activation ~2026-03-28).

---

## Context

The core solver engine is built and deployed. It receives auctions, computes routes across 13 DEXs/aggregators, and submits solutions. But having a solver is not enough. Top solvers (Barter, Laertes, Copium) win because they built the **system around the solver** - the satellite programs that feed it better data, faster, and the **decision systems** that choose when and how to submit.

This document defines every satellite system needed to go from "working solver" to "competitive solver earning revenue." Work is organized into **three parallel tracks** reflecting the reality that operational safety, performance, and competitive edge must advance together - not in a strict waterfall sequence.

### What changed in v2

Expert review identified five critical missing pieces and corrected three assumptions:

**Added:**
- **Submission Policy Engine** - the single biggest gap. "Find best route" is not enough; you also need "how/when/whether to submit it."
- **Opportunity Filter** - not every auction deserves full compute. Triage matters.
- **Strategy Attribution** - know which strategy *actually monetized*, not just which *generated candidates*.
- **State Freshness Scoring** - know which of your edges are real vs already stale.
- **Settlement Accounting Engine** - upgraded from simple revenue tracking. CoW's weekly net slippage accounting means you must track predicted vs realized vs reimbursed.

**Corrected:**
- Simulation reduces revert risk significantly but does **not** eliminate slashing risk (EBBO/fairness/accounting violations are separate).
- "Never return empty" refined to: never return empty **when a valid, positive-score, fairness-safe solution exists**. Toxic thin-margin solutions create downstream pain.
- MEV backrun reclassified as **advanced profit optimization with elevated operational risk** - not a normal sprint item.

**Restructured:**
- Linear sprint sequence replaced with three parallel tracks.
- Order Flow Prediction and full Grafana polish deferred until fundamentals are dominated.

---

## Architecture Overview

```
┌────────────────────────────────────────────────────────────────────────┐
│                            EXTERNAL                                    │
│                                                                        │
│  CoW Autopilot ──POST /solve──▶ Opportunity Filter ──▶ SOLVER ENGINE  │
│                                                              │         │
│  Alchemy/Infura ◀──WSS──────── Pool Indexer                 │         │
│                                                              ▼         │
│  Wintermute/Jump ◀──HTTP────── RFQ Service          Simulation Engine │
│                                                              │         │
│  Telegram ◀────webhook─────── Alert Service                  ▼         │
│                                                     Submission Policy  │
│  CoW Orderbook API ◀───────── Replay System                  │         │
│                                                              ▼         │
│  Settlement Contract ◀─────── Settlement Accounting         Submit     │
│                                                                        │
└────────────────────────────────────────────────────────────────────────┘

Internal data flow:

  Pool Indexer ──reserves──▶ Token Graph ──edges──▶ Solver Engine
       │                                                  │
       ▼                                                  │
  Freshness Score ──confidence──▶ Solver Engine ◀── RFQ Service
                                       │
                                       ▼
                         ┌─── Simulation Engine ───┐
                         │   (validate + gas est)   │
                         └───────────┬─────────────┘
                                     ▼
                         ┌─── Submission Policy ───┐
                         │  (risk / mode / timing)  │
                         └───────────┬─────────────┘
                                     ▼
                              Settlement tx
                                     │
                                     ▼
                         Settlement Accounting ──▶ Replay + Attribution
```

---

## Track A: Don't Die (Operational Safety)

**Goal**: The solver is observable, reliable, honest about its own state, and doesn't lose money silently.

### A.1 Structured JSON Logging - ✅ DONE (Sprint 4)

- `SolveOutcome` struct wraps response + `strategies_ran` + `used_fallback`
- One structured JSON log event per `/solve` call with full schema
- Files: `routes/solve.rs`, `solver/mod.rs`

### A.2 Fail-Safe Fallback - ✅ DONE (Sprint 4)

- `strategy_fallback()` runs DirectSolver without gas filter when all 7 strategies return nothing
- Enforces limit price and UDCP - won't submit garbage
- Rule: **Never return empty when a valid, positive-score, fairness-safe solution exists.** If the only available routes are toxic thin-margin solutions that would barely clear validation, returning empty is the correct decision.
- Files: `solver/mod.rs`

### A.3 Deterministic Output - ✅ DONE (Sprint 4)

- Two-level sort in `finalize()`: score descending, then stable key (sorted trade UIDs)
- `serialize_prices_sorted` - HashMap keys serialized in lexicographic order
- Regression test: 10 runs, byte-identical JSON output
- Files: `solver/mod.rs`, `models/solution.rs`, `tests/determinism.rs`

### A.4 Telegram Alerts - ✅ DONE (Sprint 4)

- `alerts::send_alert()` with 5-minute per-key deduplication
- Wired into `monitoring::check_alerts()` via `tokio::spawn`
- Silent no-op when env vars unset (safe in dev)
- Active triggers: solver silent 5min, win rate <1%, RPC errors >10%
- Files: `alerts.rs`, `monitoring/mod.rs`

### A.5 Simulation Engine (Anvil Fork) - ✅ PHASE 1 DONE (structural validation + kill switch + metrics, wired into solve handler)

**What**: Before submitting any solution, simulate the full settlement transaction on a local Anvil fork. Catches reverts, validates gas estimates, confirms surplus.

**Important caveat**: Simulation **greatly reduces revert risk and wasted gas** but does **not** eliminate all slashing risk. CoW's slashing and reimbursement logic is tied to rule violations (EBBO, payout accounting, fairness constraints), not just "a revert happened." The official driver already simulates candidates and continuously verifies viability during submission. Our simulation layer adds a second defense, but overconfidence here is dangerous.

**Architecture**:
```
Solver Engine ──candidate solution──▶ Simulation Engine
                                           │
                                      Fork Arbitrum at latest block
                                      (Anvil --fork-url)
                                           │
                                      Encode settlement tx
                                      (approve + swap + transfer)
                                           │
                                      eth_call simulation
                                           │
                                      ┌────┴────┐
                                      │         │
                                   Success    Revert
                                      │         │
                                   Submit    Discard + log + alert
```

**Implementation**:
- New crate or module: `simulator/`
- Maintain a persistent Anvil process forked at latest block
- On each new block: `anvil_reset` to latest state
- Simulation function: takes a `Solution`, encodes as settlement calldata, runs `eth_call`
- Returns: `SimResult { success: bool, gas_used: u64, revert_reason: Option<String>, block_number: u64 }`
- Timeout: 2 seconds max per simulation (if slower, skip and submit with `simulated: false` flag)
- Track simulation pass rate separately from submission outcomes

**Acceptance criteria**:
- All submitted solutions simulated first (when simulator is available)
- Simulation revert reasons logged with full context and alerted via Telegram
- Simulation adds <2 seconds to pipeline
- `simulated: bool` flag on every submitted solution

---

### A.6 Settlement Accounting Engine - ✅ DONE (SQLite, predicted/realized/reimbursed, wired into solve handler)

**What**: Upgraded from the original "Revenue Tracker" concept. CoW Protocol's reward and accounting is tied to score, slippage, and realized execution outcomes. The docs explicitly state that weekly net slippage can result in payment to or from the solver. A simple "surplus - gas" tracker is not enough.

**Why this matters**: Without settlement-quality accounting, you cannot answer: "Are we actually profitable after reimbursements, penalties, and accounting adjustments?"

**Data model**:
```sql
CREATE TABLE settlement_log (
  auction_id TEXT PRIMARY KEY,
  chain_id INTEGER,
  settled_at TIMESTAMP,

  -- Predicted (at submission time)
  predicted_surplus_wei TEXT,
  predicted_gas_cost_wei TEXT,
  predicted_net_score_wei TEXT,

  -- Realized (from on-chain settlement)
  realized_surplus_wei TEXT,
  realized_gas_cost_wei TEXT,
  realized_slippage_wei TEXT,      -- predicted - realized surplus
  settlement_tx_hash TEXT,

  -- Accounting (from CoW weekly settlement)
  reimbursement_wei TEXT,          -- positive = we received, negative = we paid
  accounting_period TEXT,          -- e.g. "2026-W13"
  net_pnl_wei TEXT,                -- realized surplus - gas - slippage ± reimbursement

  -- Attribution
  winning_strategy TEXT,
  simulated BOOLEAN,
  submission_mode TEXT             -- "standard" | "protected" | "flashbots"
);
```

**Tracked layers**:
| Layer | What | Source |
|-------|------|--------|
| Predicted | Route surplus, gas estimate, net score | Solver output at submission |
| Realized | Actual surplus, gas paid, slippage | On-chain settlement tx parsing |
| Accounting | Reimbursements, penalties, weekly net | CoW Protocol accounting API |
| Attribution | Which strategy produced the winning candidate | Internal strategy tagging |

**CLI**:
```bash
cargo run --bin accounting -- --daily          # today's P&L breakdown
cargo run --bin accounting -- --weekly         # weekly accounting summary
cargo run --bin accounting -- --slippage       # predicted vs realized analysis
cargo run --bin accounting -- --by-strategy    # P&L per strategy
```

**Acceptance criteria**:
- Every won auction tracked across all 4 layers
- Predicted vs realized slippage visible per auction and in aggregate
- Weekly accounting reconciles with CoW Protocol payouts
- Strategy-level P&L available (not just aggregate)

---

### A.7 Auction Replay System - ✅ DONE (gzip-compressed SQLite + CLI tool, wired into solve handler)

**What**: Record every auction received and the solution we submitted. Also fetch the winning solution from CoW's API. Compare offline to understand why we won or lost.

**Why this is high leverage**: Most teams improve too slowly because they cannot answer one simple question: *"Why did we lose this exact auction?"* Replay is one of the highest-leverage investments in the whole system.

**Architecture**:
```
/solve handler ──auction + our solution──▶ SQLite
                                                │
CoW Orderbook API ──winning solution──────────▶ │
                                                ▼
                                          Replay CLI tool:
                                          - Load past auction
                                          - Run current solver
                                          - Compare vs winner
                                          - Show delta analysis
                                          - Strategy attribution
```

**Data stored per auction**:
```sql
CREATE TABLE auction_log (
  id TEXT PRIMARY KEY,
  chain_id INTEGER,
  received_at TIMESTAMP,
  orders_count INTEGER,
  auction_json TEXT,              -- full input (compressed)
  our_solution_json TEXT,         -- what we submitted
  our_score_wei TEXT,
  winning_solver TEXT,            -- who won
  winning_score_wei TEXT,
  score_delta_wei TEXT,           -- how far off we were
  response_time_ms INTEGER,
  strategies_used TEXT,           -- JSON array of all strategies that ran
  strategy_submitted TEXT,        -- which strategy produced the submitted solution
  strategy_best_predicted TEXT,   -- which strategy had best predicted score
  triage_class TEXT,              -- "profitable" | "unprofitable" | "unwinnable" | "risky"
  simulated BOOLEAN,
  sim_result TEXT,                -- "pass" | "revert" | "timeout" | "skipped"
  result TEXT                     -- "won" | "lost" | "timeout" | "empty" | "error"
);
```

**Replay CLI**:
```bash
cargo run --bin replay -- --auction-id abc123                 # replay specific auction
cargo run --bin replay -- --last 100 --summary                # win rate summary
cargo run --bin replay -- --last 1000 --by-strategy           # strategy breakdown
cargo run --bin replay -- --last 1000 --attribution           # decision-level attribution
cargo run --bin replay -- --losses --last 50 --delta-analysis # why did we lose?
```

**Acceptance criteria**:
- Every auction logged (input + output + winning solution + triage class + attribution)
- Replay tool can re-run any historical auction with current solver code
- Summary shows: win rate, avg score delta, strategy breakdown, triage accuracy
- Decision-level attribution: generated vs survived simulation vs submitted vs won vs realized PnL
- Storage: ~1KB per auction compressed, SQLite

---

## Track B: Get Fast (Performance & Data Freshness)

**Goal**: The solver has fresh liquidity data, knows what's stale, and doesn't waste time on junk.

### B.1 Pool Indexer / Liquidity Monitor

**What**: A continuously running service that monitors on-chain pool reserves across all 13 DEXs. Maintains an in-memory cache of pool state that the solver reads during auctions instead of making RPC calls.

**Why this is critical**: Without this, every auction spends 3-10 seconds on RPC calls to read pool reserves. Top solvers pre-cache everything and solve in <500ms. You are an RPC tourist until this is built.

**Architecture**:
```
Alchemy WSS ──new block event──▶ Pool Indexer
                                      │
                                 multicall batch
                                 (read reserves for
                                  all tracked pools)
                                      │
                                      ▼
                               In-memory cache
                               (DashMap<PoolId, PoolState>)
                                      │
                                      ▼
                               Solver reads cache
                               (zero RPC during auction)
```

**Tracked state per pool type**:
| DEX | State to cache |
|-----|---------------|
| Uniswap V2 / Camelot V2 | reserve0, reserve1 |
| Uniswap V3 / Camelot V3 | sqrtPriceX96, liquidity, tick |
| Curve | balances[], A, fee |
| Balancer V2 | balances[], weights[], swapFee |
| GMX V2 | pool amounts, pricing params |
| Trader Joe V2.1 | bin reserves, active bin |
| DODO | base/quote reserves, k, R |
| Wombat | cash, liability, ampFactor |

**Implementation**:
- New crate: `pool-indexer/`
- Subscribe to `newHeads` via WSS (Arbitrum ~250ms blocks)
- On each block: multicall batch read for all tracked pools (batch size ~500 calls per multicall)
- Store in `DashMap` with block number + timestamp
- Expose via in-process shared reference (if same binary) or local HTTP API (if separate process)
- Pool discovery: on startup, read factory events for all supported DEXs to build initial pool list
- Stale detection: if a pool hasn't updated in 100 blocks, mark stale and log warning

**Acceptance criteria**:
- All 13 DEX pool types cached
- Cache refresh latency: <500ms after new block
- Solver reads from cache with 0 RPC calls during `/solve`
- Handles WSS reconnection gracefully (auto-reconnect with backoff)

---

### B.2 Token Graph Builder - ✅ DONE (RwLock-cached weighted graph with staleness detection)

**What**: A weighted directed graph of all tradeable token pairs. Edge weight = expected output amount for a reference input size. Enables fast pathfinding (Yen's K-shortest-paths) without brute-forcing routes.

**Implementation**:
- Module within `pool-indexer/` or `solver-engine/src/graph/`
- Rebuild edges on every block (incremental: only recompute edges for pools whose reserves changed)
- Edge weight = `-log(output/input)` so shortest path = best rate (standard trick)
- Include gas cost per hop in edge weight
- Pre-compute top-50 token pairs on startup for instant lookup

**Acceptance criteria**:
- Graph rebuilt in <50ms per block
- K-shortest-paths returns in <5ms for any token pair
- Handles 10,000+ pools without performance degradation

---

### B.3 Gas Oracle (Arbitrum L1 + L2) - ✅ DONE (Sprint 4 + background refresh)

**What**: Accurate real-time gas cost estimation for Arbitrum transactions. Arbitrum gas has two components: L2 execution gas + L1 calldata posting cost. Most solvers get this wrong.

**Implementation**:
- Read `ArbGasInfo` precompile (`0x000000000000000000000000000000000000006C`) for L1 pricing
- Track L1 base fee (fluctuates with Ethereum congestion)
- Per-interaction gas estimate: `l2_gas * l2_price + calldata_bytes * l1_price_per_byte`
- Cache per block, expose as `estimate_gas(calldata: &[u8]) -> u64`

**Acceptance criteria**:
- Gas estimates within 10% of actual on-chain cost
- L1 component correctly accounts for calldata compression
- Updates every block

---

### B.4 State Freshness Scoring - ✅ DONE (4-dimension weighted confidence, wired into solve handler)

**What**: A per-candidate confidence score reflecting how stale the underlying data is. Winning teams are not just faster - they are better at understanding which edges are real versus already stale.

**Freshness dimensions**:
| Signal | Weight | Source |
|--------|--------|--------|
| Source block age | High | `current_block - cache_block` for each pool touched |
| Pool change count | Medium | How many touched pools changed since our last refresh |
| RPC lag | Medium | Measured round-trip to RPC node |
| RFQ quote age | High | `now - quote_timestamp` (RFQ quotes decay fast) |
| Simulation block mismatch | Critical | `sim_block != submission_block` means state may have shifted |
| Predicted pool volatility | Low | Historical volatility of touched pools |

**Output**:
```rust
pub struct FreshnessScore {
    /// 0.0 = completely stale, 1.0 = maximally fresh
    pub confidence: f64,
    /// Which dimension dragged confidence down most
    pub bottleneck: FreshnessDimension,
    /// Block number this score was computed at
    pub as_of_block: u64,
}
```

**Usage**:
- Scoring: multiply candidate score by freshness confidence
- Submission policy: don't submit candidates below freshness threshold
- Alerts: fire warning if average freshness drops below 0.5
- Metrics: track freshness distribution over time

**Acceptance criteria**:
- Every candidate solution annotated with freshness score
- Submission policy respects freshness threshold
- Replay logs include freshness at time of submission

---

### B.5 Opportunity Filter / Auction Triage - ✅ DONE (fast classify, wired into solve handler)

**What**: A fast classifier that bins incoming auctions into categories before committing full solver resources. Your scarce resources are not just compute - they include time budget, simulation budget, RFQ budget, and cognitive clarity in analysis.

**Why this matters**: You do not want your expensive path search + RFQ + simulation pipeline firing at full power on junk auctions. One classifier can massively improve effective performance.

**Triage classes**:
| Class | Action | Criteria |
|-------|--------|----------|
| `profitable` | Full pipeline + RFQ + simulation | Orders with clear liquidity, reasonable size, known tokens |
| `marginal` | Internal strategies only, skip RFQ | Small orders, thin pools, high gas relative to surplus |
| `unwinnable` | Fast direct-only, minimal compute | Tokens we have no liquidity for, or orders where top solvers have structural advantage (RFQ-only pairs) |
| `skip` | Return empty immediately | Malformed, dust orders, tokens with zero known liquidity |

**Implementation**:
- Cheap pre-score in first 10-30ms of `/solve` handler
- Based on: order size vs pool depth, token familiarity, historical win rate for similar orders
- Decide which pipeline depth to run
- Maintain separate metrics per triage class
- Evaluate win rate conditional on triage bucket (are we triaging correctly?)

**Acceptance criteria**:
- Triage runs in <30ms before main pipeline
- Each auction tagged with triage class in logs and replay data
- Win rate tracked per triage bucket
- Compute savings measurable (fewer RFQ calls, fewer simulations on junk)

---

## Track C: Actually Win (Competitive Edge)

**Goal**: Access to hidden liquidity, smart submission, and honest attribution of what's working.

### C.1 Private Liquidity / RFQ Service

**What**: Connect to private market makers who offer off-chain quotes. This is the single biggest differentiator between mid-tier and top-tier solvers. Barter and Laertes consistently win auctions because they get quotes from Wintermute, Jump, Tokka Labs etc. that aren't visible on-chain.

**RFQ providers to integrate**:
| Provider | API | Notes |
|----------|-----|-------|
| Wintermute | RFQ API | Large inventory, good for big orders |
| 0x Professional | 0x RFQ | Aggregated MM quotes |
| Hashflow | Hashflow RFQ | Gasless quotes, MEV-protected |
| Paraswap Augustus | Private pools | Access via API key |

**Implementation**:
- New crate: `rfq-service/`
- Async HTTP client per provider
- Request quotes in parallel with DEX routing
- Timeout: 500ms per RFQ (if slower, use DEX-only solution)
- Quote comparison: include RFQ quote as a "virtual pool" in the routing graph
- Track RFQ win rate vs DEX-only to measure value
- RFQ calls gated by Opportunity Filter - don't waste quota on `marginal` or `unwinnable` auctions

**Integration with solver**:
```
Auction received ──▶ Opportunity Filter
    │
    │  (if class = "profitable")
    │
    ├──▶ DEX routing (existing pipeline)
    │
    ├──▶ RFQ quotes (parallel, 500ms timeout)
    │
    └──▶ Merge: pick best price per order (DEX vs RFQ)
              │
              ▼
         Solution assembly ──▶ Simulation ──▶ Submission Policy
```

**Acceptance criteria**:
- At least 2 RFQ providers integrated
- Quotes requested in parallel with DEX routing (no added latency if <500ms)
- Fallback to DEX-only if all RFQ calls fail
- Metrics: RFQ usage rate, RFQ win rate vs DEX, RFQ quota utilization

---

### C.2 Submission Policy Engine - ✅ DONE (risk/mode/timing + EV, wired into solve handler)

**What**: The biggest missing piece from v1 of this plan. Having a good solution is necessary but not sufficient on CoW. You also need the best **submission behavior**.

**Why this is critical**: The CoW driver continuously verifies settlement viability during submission. Submission is not fire-and-forget. Without explicit submission policy, you may build a strong solver that still underperforms in practice because of bad submission timing, volatility exposure, or wasted gas on stale candidates.

**Decision framework per candidate**:
| Question | Input | Output |
|----------|-------|--------|
| How risky is this candidate? | Freshness score, pool volatility, order size | Risk classification: low / medium / high |
| How fresh is the underlying state? | FreshnessScore | Confidence threshold check |
| Should this go protected or public? | MEV exposure analysis | Submission mode: standard / protected / flashbots |
| Should we submit now or wait? | Time remaining, state freshness, score delta vs previous submission | Timing decision: submit / hold / replace |
| Should we replace a previous submission? | Score improvement delta, gas cost of replacement | Replace / keep |
| What's the expected value after adjustments? | Predicted surplus, gas, slippage model, fail probability | EV in wei |
| What happened to similar submissions historically? | Replay data for same triage class, strategy, token pair | Historical success rate |

**Implementation**:
- New module: `solver-engine/src/submission/`
- Classify each candidate solution before submission
- Choose submission mode based on MEV exposure
- Log expected value before submission
- Track "great route, bad submission" losses separately from "bad route" losses
- Integrate with Replay system for historical comparison

**Acceptance criteria**:
- Every submission annotated with: risk class, submission mode, expected value, freshness score
- Protected path used when MEV exposure detected
- Replacement logic: only replace if score improvement > replacement gas cost
- Replay can filter by submission policy decisions
- Separate metrics: route quality vs submission quality

---

### C.3 Strategy Attribution (Decision-Level) - ✅ DONE (per-strategy funnel, wired into solve handler + Prometheus)

**What**: Know which strategy **actually monetized**, not just which one generated candidates. Without decision-level attribution, you end up with misleading conclusions ("graph routing looks great" when graph routing just generates many candidates and simpler paths are the actual monetizers).

**Attribution chain**:
```
Strategy generated candidate
    ▼
Strategy survived simulation
    ▼
Strategy had best predicted score
    ▼
Strategy was actually submitted
    ▼
Strategy won the auction
    ▼
Strategy had best realized PnL (not just nominal score)
```

**What to track per auction**:
| Field | Meaning |
|-------|---------|
| `strategies_generated` | All strategies that produced at least one candidate |
| `strategy_best_predicted` | Strategy with highest predicted score |
| `strategy_sim_passed` | Strategies whose candidates passed simulation |
| `strategy_submitted` | Strategy that produced the actually submitted solution |
| `strategy_won` | Whether our submission won (boolean) |
| `strategy_realized_pnl` | Realized P&L of the submitted strategy |

**CLI / Replay integration**:
```bash
cargo run --bin replay -- --last 1000 --attribution
# Output:
#   Strategy     | Generated | Sim Pass | Submitted | Won  | Realized PnL
#   cow          |  234      |  234     |  45       |  12  | +0.23 ETH
#   direct       |  891      |  880     |  310      |  89  | +1.45 ETH
#   graph        |  567      |  498     |  201      |  34  | +0.67 ETH
#   rfq          |  123      |  123     |  98       |  67  | +3.21 ETH
#   aggregator   |   45      |   40     |  12       |   3  | +0.08 ETH
```

**Acceptance criteria**:
- Full attribution chain logged per auction
- Replay tool shows per-strategy funnel: generated → simulated → submitted → won → realized PnL
- Dashboard exposes strategy effectiveness metrics

---

### C.4 MEV-Aware Backrun Detector

**Classification**: Advanced profit optimization with **elevated operational risk**. Do not treat this as a normal sprint item.

**Why it's high-risk**: Backrun capture is not just "simulate, detect dislocation, append interaction." It becomes a submission-strategy problem, bundle-strategy problem, revert-risk problem, and latency problem. CoW's driver docs point out that submission strategy changes depending on whether a settlement exposes MEV, including MEV-protected RPC paths.

**Prerequisites** (must be stable before starting):
- Simulation engine (A.5) - battle-tested, not just working
- Submission policy engine (C.2) - can route to protected paths
- Settlement accounting (A.6) - can measure actual backrun PnL vs predicted
- Replay system (A.7) - can evaluate backrun decisions historically

**How it works**:
```
Simulate our settlement on Anvil fork
    ▼
Read all affected pool prices post-settlement
    ▼
Compare post-settlement prices across DEXs for same pair
    ▼
If dislocation > 2x gas cost: encode backrun as additional interaction
    ▼
Route through protected submission path (mandatory)
    ▼
Track: attempts, successes, reverts, realized revenue
```

**Acceptance criteria** (when eventually implemented):
- Backrun detection within simulation step (no added latency)
- Conservative threshold: profit > 2x gas cost
- **Mandatory** protected submission path for backrun-containing solutions
- Separate metrics and P&L tracking for backrun revenue
- Kill switch: disable backruns without redeploying

---

### C.5 Essential Metrics (Prometheus) - ✅ DONE (strategy, triage, simulation, freshness, submission, attribution)

**What**: Expose the metrics needed for operational awareness. **Defer full Grafana dashboard polish** - for month 1, the must-have views are a simple set of operational signals, not a beautiful dashboard suite.

**Must-have metrics** (Prometheus format, already partially implemented):
```
# Counters
solver_auctions_received_total{chain="arbitrum"}
solver_auctions_responded_total{chain="arbitrum"}
solver_auctions_won_total{chain="arbitrum"}
solver_auctions_empty_total{chain="arbitrum"}
solver_auctions_timeout_total{chain="arbitrum"}
solver_auctions_fallback_total{chain="arbitrum"}
solver_ebbo_violations_total{chain="arbitrum"}
solver_sim_pass_total{chain="arbitrum"}
solver_sim_revert_total{chain="arbitrum"}

# Histograms
solver_response_time_seconds{chain="arbitrum"}

# Gauges
solver_gas_wallet_balance_eth{chain="arbitrum"}
solver_pool_cache_size{chain="arbitrum"}
solver_pool_cache_staleness_blocks{chain="arbitrum"}
solver_freshness_avg{chain="arbitrum"}
solver_rfq_hit_rate{chain="arbitrum"}
solver_score_delta_vs_winner_avg{chain="arbitrum"}
solver_realized_pnl_eth{chain="arbitrum"}
```

**Month-1 essential views** (can be plain terminal or minimal web UI):
- Auctions received / responded / won
- p50 / p95 latency
- Fallback rate
- Simulation pass rate
- Gas wallet balance
- RFQ hit rate
- Score delta vs winner
- Realized P&L

**Acceptance criteria**:
- All metrics above exposed on `/metrics`
- At least a terminal-based summary view (full Grafana is deferred)

---

## Deferred (Build After Fundamentals Are Dominated)

These items are intentionally deferred. They become valuable only after Tracks A-C are stable and producing real auction data.

### D.1 Order Flow Predictor

Pre-compute routes for recurring order patterns (Aave rebalances, whale DCA, gauge votes). Interesting but not urgent until:
- Replay data shows strong recurring structure
- RFQ quality is optimized
- Submission policy is stable
- Settlement accounting is reconciling cleanly

### D.2 Strategy A/B Testing Framework

Config-driven strategy weights, 50/50 split, separate metrics per variant. Useful for iteration velocity but requires stable replay and attribution first.

### D.3 Full Grafana Dashboard Suite

Auto-provisioned Grafana + Prometheus with polished panels, alerts, time series. Nice to have but raw metrics + terminal + Telegram alerts cover month 1.

---

## Build Priority & Dependencies

```
                    Track A: Don't Die
                    ┌──────────────────────────┐
                    │ A.1-A.4 ✅ DONE          │
                    │ A.5 Simulation Engine     │──────┐
                    │ A.6 Settlement Accounting │──┐   │
                    │ A.7 Auction Replay        │  │   │
                    └──────────────────────────┘  │   │
                                                   │   │
                    Track B: Get Fast              │   │
                    ┌──────────────────────────┐   │   │
                    │ B.1 Pool Indexer          │   │   │
                    │ B.2 Token Graph           │   │   │
                    │ B.3 Gas Oracle            │   │   │
                    │ B.4 Freshness Scoring ────│───│───┤
                    │ B.5 Opportunity Filter    │   │   │
                    └──────────────────────────┘   │   │
                                                   │   │
                    Track C: Actually Win          │   │
                    ┌──────────────────────────┐   │   │
                    │ C.1 RFQ Service           │   │   │
                    │ C.2 Submission Policy  ◀──│───┘───┘
                    │ C.3 Strategy Attribution  │
                    │ C.4 MEV Backrun (ADVANCED)│  ◀── requires A.5, A.6, C.2 stable
                    │ C.5 Essential Metrics     │
                    └──────────────────────────┘
```

**Recommended build order** (parallelized across tracks):

| Phase | Track A | Track B | Track C | Duration |
|-------|---------|---------|---------|----------|
| **1** (NOW) | A.5 Simulation | B.1 Pool Indexer + B.3 Gas Oracle | C.5 Essential Metrics | 1-2 weeks |
| **2** | A.7 Replay + A.6 Accounting | B.2 Token Graph + B.4 Freshness | C.1 RFQ Service | 2-3 weeks |
| **3** | - | B.5 Opportunity Filter | C.2 Submission Policy + C.3 Attribution | 2-3 weeks |
| **4** | - | - | C.4 MEV Backrun (only if A.5+C.2 are battle-tested) | 2+ weeks |

---

## Latency Budgets (Mandatory)

Every component has a hard ceiling. If a component exceeds its budget, that is a bug - not a tradeoff. A beautiful slow solver is a losing solver.

| Component | Budget | Notes |
|-----------|--------|-------|
| Opportunity Filter (triage) | **30ms** | Must complete before main pipeline starts |
| Core route generation (strategies 1-6) | **150ms** | With pre-cached pool state (Track B) |
| RFQ collection | **500ms max** | Parallel with route generation, hard timeout |
| Simulation (per candidate) | **2s max** | Skip and flag `simulated: false` if exceeded |
| Freshness scoring | **5ms** | In-memory only, no I/O |
| Submission policy decision | **50ms** | Classification + mode selection |
| **Total normal path** | **< 3s** | Triage + route + RFQ + sim + submit decision |
| **Total hard stop** | **< 20s** | Return best-so-far well before 25s deadline |

If any component consistently runs at >80% of its budget, file it as a performance issue immediately.

---

## Kill Switches (Required for All Risky Subsystems)

Production systems never fail cleanly. Every risky subsystem must be disableable at runtime via environment variable - no redeploy required.

| Subsystem | Kill Switch Env Var | Disabled Behavior |
|-----------|--------------------|--------------------|
| RFQ providers (all) | `RFQ_ENABLED=false` | Skip RFQ, DEX-only routing |
| Individual RFQ provider | `RFQ_WINTERMUTE_ENABLED=false` etc. | Skip that provider, others still active |
| Simulation | `SIMULATION_ENABLED=false` | Submit without simulation, flag `simulated: false` |
| Submission replacement | `SUBMISSION_REPLACE_ENABLED=false` | Never replace a submitted solution |
| Freshness threshold | `FRESHNESS_GATE_ENABLED=false` | Submit regardless of freshness score |
| Opportunity filter | `TRIAGE_ENABLED=false` | Run full pipeline on every auction |
| Protected submission routing | `PROTECTED_SUBMIT_ENABLED=false` | All submissions go standard path |
| MEV backrun | `BACKRUN_ENABLED=false` | Never append backrun interactions |

**Rule**: Every kill switch defaults to the **safe** state. If the env var is unset, the subsystem is either disabled (for risky features like backruns) or runs in its conservative mode.

---

## Module Contracts (Failure Behavior)

Every subsystem has a defined failure mode. The universal principle: **on failure, be conservative, never optimistic.** A subsystem that silently returns an optimistic default is more dangerous than one that loudly returns nothing.

| Module | Inputs | Outputs | On Failure |
|--------|--------|---------|------------|
| B.5 Opportunity Filter | Auction orders, pool cache | Triage class | Default to `profitable` (run full pipeline) - never silently skip viable auctions |
| B.1 Pool Indexer | WSS block events | DashMap cache | Serve last-known state, alert stale, log block gap |
| B.4 Freshness Scoring | Cache metadata, RPC latency, quote timestamps | FreshnessScore (0.0-1.0) | Return `confidence: 0.1` (conservative low) - never return optimistic freshness on failure |
| C.1 RFQ Service | Order details, token pair | Quote or None | Return None, fall back to DEX-only - never fabricate a quote |
| A.5 Simulation | Candidate solution | SimResult (pass/revert/timeout) | Flag `simulated: false`, submit anyway with risk tag - never silently claim simulation passed |
| C.2 Submission Policy | Candidate + freshness + risk class | Submit/hold/replace decision | Submit via standard path with conservative parameters - never hold indefinitely |
| A.6 Settlement Accounting | Tx hash, on-chain events | Realized P&L | Log error, mark auction as `accounting_pending` - never drop the record |

---

## Phase Completion Gate

A phase is **not** considered complete when the code compiles. It is complete when all of the following are true:

| Gate | What it means |
|------|---------------|
| **Code merged** | On `main` or `dev`, not a stale feature branch |
| **Tests exist** | Unit tests for core logic, integration test for the happy path, edge case tests for failure modes |
| **Replay coverage** | Where relevant: the system's decisions are logged in replay data and can be evaluated historically |
| **Metrics emitted** | The system's key health signals appear on `/metrics` |
| **Alerts wired** | If failure is costly, Telegram alerts fire on the failure condition |
| **Kill switch exists** | For risky subsystems: env-var disable with documented safe default |
| **Failure mode documented** | Module contract table entry exists with defined "on failure" behavior |

This gate applies especially to: Simulation (A.5), RFQ (C.1), Submission Policy (C.2), and MEV Backrun (C.4). These systems interact with real money. "It works in tests" is not done.

---

## Non-Negotiable Rules for All Agents

1. **Never return empty when a valid, positive-score, fairness-safe solution exists** - but don't submit toxic thin-margin garbage just to avoid returning empty. Use judgment.
2. **Never submit without simulation** once the simulation engine is live - but simulation does not eliminate all risk. Respect EBBO and accounting rules independently.
3. **Log everything** - if it's not logged, it didn't happen. Especially: triage class, strategy attribution, freshness score, submission policy decision.
4. **Respect the 25-second deadline** - better to submit a suboptimal solution at 20s than a perfect one at 26s.
5. **Test with replay** - every strategy change must be validated against historical auctions before deploying.
6. **Gas wallet monitoring is non-negotiable** - if the wallet runs dry, the solver is dead.
7. **Deterministic output** - same input, same output, always. This is enforced by regression tests.
8. **Track predicted vs realized** - every submission must record predicted surplus, and every settlement must record realized surplus. If you can't measure the gap, you can't close it.
9. **Attribute at the decision level** - know which strategy generated, which survived, which submitted, which won, which monetized. Aggregate stats hide the truth.
10. **No side quests** - nothing from the Deferred section gets built until Phases 1-3 are stable and producing real auction data. No ML experiments, no exotic routing without replay evidence, no framework abstractions where a narrow service would do.

---

## Success Metrics

| Metric | Month 1 Target | Month 3 Target | Month 6 Target |
|--------|----------------|----------------|----------------|
| Win rate | 1-3% | 5-10% | 10-20% |
| Avg response time | <500ms | <300ms | <200ms |
| Monthly revenue | $500-2K | $2-10K | $10-20K |
| Slashing events | 0 | 0 | 0 |
| Uptime | 99% | 99.9% | 99.9% |
| Auctions responded | >95% | >99% | >99.5% |
| Simulation pass rate | >95% | >98% | >99% |
| Predicted vs realized gap | - | <20% | <10% |
| Triage accuracy | - | >70% | >85% |
| Score delta vs winner (p50) | - | <30% | <15% |
