# Sprint: Critical Launch Blockers

**Created:** 2026-03-30
**Priority:** BLOCKER - must fix before live submission
**Source:** Shadow competition feedback (27,644 auctions analyzed)

## Executive Summary

The solver pipeline works end-to-end (stable infra, 59h uptime, 0% RPC errors) but has **3 critical bugs** that result in **0 real submissions** and **100% simulation reverts**. All strategies are structurally present and enabled, but the combination of bugs means the solver earns 0 ETH.

---

## BLOCKER 1: Metrics Lie - Dashboard Says "100% Submit" But 0 Actually Submitted

### Root Cause (CONFIRMED in code)

**File:** `solver-engine/src/routes/solve.rs:293` + `solver-engine/src/monitoring/mod.rs:239-240`

The call chain:
1. `solve()` finds solutions → `solutions_found > 0` ✓
2. `submission::evaluate()` returns `Hold` (because sim reverted or freshness too low)
3. `should_submit = false` (line 238-241)
4. BUT `monitoring::record_solve(duration_ms, solutions_found, score)` is called at **line 293** - BEFORE the hold check at line 345
5. `record_solve()` increments `solutions_submitted` when `solutions > 0` (monitoring/mod.rs:239)
6. Line 345-351 returns empty `{ solutions: [] }` - the actual HTTP response has NO solutions

**Result:** Dashboard shows 100% submit rate. Driver receives empty responses. 0 actual submissions.

### Fix

Move `record_solve` AFTER the hold decision, or split it into two counters:
- `solutions_generated` - what the solver found (for debugging)
- `solutions_actually_submitted` - what was returned in the HTTP response

**Files to change:**
- `solver-engine/src/routes/solve.rs` - move metrics recording or add new counter
- `solver-engine/src/monitoring/mod.rs` - add `solutions_held` counter
- `solver-engine/src/routes/dashboard.rs` - display actual vs held counts

### Validation
After fix, dashboard should show: Submit: X, Hold: Y, where X + Y = total auctions with solutions.

---

## BLOCKER 2: Settlement Simulation Always Reverts → All Solutions Held

### Root Cause (CONFIRMED in code)

**File:** `solver-engine/src/settlement.rs:157-158`

The settlement encoder skips trade encoding:
```rust
// Empty trades array for now (simulation focuses on interactions)
let trades_encoded = encode_empty_dynamic_array();  // line 158
```

The `settle()` function on the GPv2Settlement contract REQUIRES valid trades. An empty trades array with non-empty tokens/prices causes the contract to revert. Every single `eth_call` returns a revert → `sim_reverted = true` → submission policy holds.

Additionally, `encode_trade_placeholder()` (line 404-431) uses zeroed `sellTokenIndex`/`buyTokenIndex` and zero addresses - even if trades were included, they'd be invalid.

### The Cascade

```
settlement.rs encodes empty trades → eth_call reverts → sim_reverted=true
→ submission.rs:207-208: "Simulation reverted → hold"
→ solve.rs:345: returns empty response
→ 0 submissions, 100% hold, 100% high risk
```

This single bug causes ALL THREE dashboard symptoms:
- 100% revert (simulation always fails)
- 100% high risk (sim_reverted → RiskClass::High)
- 0 submissions (Hold policy on sim_reverted)

### Fix Options (pick one)

**Option A: Proper trade encoding (recommended)**
Carry full order data through the pipeline into `encode_settlement()`. Map each trade to its order's sell/buy token indices, amounts, signatures, etc. This produces valid calldata that the settlement contract can execute.

**Option B: Disable eth_call simulation, use structural validation only**
Set `SIM_ETH_CALL=false` in .env. The simulation falls through to line 152-154 which returns `SimResult::passed()`. Solutions get `simulated: false, success: true`. Submission policy then allows submission (line 207 check fails, freshness check at line 222 may still hold if stale).

**Option C: Skip simulation entirely**
Set `SIMULATION_ENABLED=false`. Returns `SimResult::skipped()` with `success: true, simulated: false`. Submission policy allows (no sim_reverted, no high-risk from sim). Solutions go through.

**Recommended path:** Option C as immediate unblock, then Option A as proper fix.

**Files to change:**
- `solver-engine/src/settlement.rs` - fix `encode_settlement()` to include real trades
- `solver-engine/src/models/solution.rs` - may need to carry order data for trade encoding
- `.env` on VPS - set `SIMULATION_ENABLED=false` as stopgap

### Validation
After fix, `sim_metrics().passed` should be > 0. Submission policy should return `Submit` for solutions with positive scores and acceptable freshness.

---

## BLOCKER 3: Pool Enrichment Re-introduces Stale Data

### Root Cause (CONFIRMED in code)

**File:** `solver-engine/src/routes/solve.rs:120-147`

Despite the solver's Phase 1 fix comment saying "use ONLY driver-provided pools" (solver/mod.rs:107-113), the solve route handler at lines 120-147 ENRICHES the auction with stale cached pools BEFORE passing to `solver::solve()`:

```rust
let indexer_pools = pool_indexer::cached_as_liquidity();  // line 124
// ... merges them into auction.liquidity (lines 126-147)
```

So by the time `solver::solve()` sees `auction.liquidity`, it contains BOTH driver pools (fresh) AND indexer pools (stale). The solver routes through all of them indiscriminately - any route touching a stale pool produces wrong outputs.

### Fix

Remove the enrichment block entirely (lines 120-147 in solve.rs). The solver should ONLY use `auction.liquidity` as received from the driver. Cached pools should only be used for graph topology in Phase 4 (already handled separately in solver/mod.rs via `token_graph`).

**Files to change:**
- `solver-engine/src/routes/solve.rs` - remove lines 120-147 (pool enrichment)

### Validation
After fix, `auction.liquidity.len()` should equal driver-provided pool count only. No "Enriched auction liquidity from pool indexer" log messages.

---

## MAJOR (Non-Blocker): Solve Time Flat at 2500ms

### Root Cause

All auctions hit the same time profile because Phase 3 (split routing) runs for its full 5s budget on every auction, and Phase 4 (graph search) adds more. The "avg 1480ms, p50/p95/p99 = 2500ms" flat distribution suggests the solver always reaches Phase 3 and burns time there.

### Fix

Add early exit when Phase 1 or 2 already covers all orders with positive surplus. Don't enter Phase 3/4 unless there are uncovered orders or clear improvement potential.

**Files to change:**
- `solver-engine/src/solver/mod.rs` - add early exit logic after Phase 2

---

## Priority Order for Agent Team

### Step 1: Immediate Unblock (30 min)
1. SSH to VPS
2. Set `SIMULATION_ENABLED=false` in `.env`
3. Remove pool enrichment (lines 120-147 in `routes/solve.rs`)
4. Rebuild and restart

**Expected result:** Solutions start being submitted. Dashboard shows real submit/hold split. Revert rate drops from 100% to whatever the driver's own simulation produces.

### Step 2: Fix Metrics (1-2 hours)
1. Add `solutions_held` counter to monitoring
2. Move `record_solve` after hold decision, or split into `record_generated` / `record_submitted`
3. Update dashboard to show actual vs held

### Step 3: Fix Settlement Encoding (4-8 hours)
1. Carry full order data through pipeline (order uid → order fields mapping)
2. Implement proper GPv2Trade encoding in `encode_trade_placeholder()`
3. Map sell/buy tokens to correct indices from the token array
4. Include order signatures (from auction payload)
5. Re-enable `SIMULATION_ENABLED=true`
6. Verify: sim pass rate should be > 0%

### Step 4: Early Exit + Timing (2-3 hours)
1. After Phase 1, check if all orders are covered with positive surplus
2. If so, skip Phase 3/4 entirely
3. Target: avg solve time < 500ms for simple auctions

### Step 5: Validate Competitiveness
1. Run shadow comparison for 24h after fixes
2. Target: > 10% of auctions within 10% of winner
3. If met → enable live submissions

---

## File Reference

| File | What to Change |
|------|---------------|
| `solver-engine/src/routes/solve.rs` | Remove pool enrichment (L120-147), fix metrics placement (L293) |
| `solver-engine/src/monitoring/mod.rs` | Add `solutions_held` counter |
| `solver-engine/src/routes/dashboard.rs` | Display actual vs held counts |
| `solver-engine/src/settlement.rs` | Fix `encode_settlement()` - real trades, not empty array |
| `solver-engine/src/solver/mod.rs` | Add early exit after Phase 2 |
| `.env` (VPS) | `SIMULATION_ENABLED=false` (stopgap) |
