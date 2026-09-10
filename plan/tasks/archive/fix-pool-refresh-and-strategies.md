# Fix Plan: Pool Refresh + Strategy Scoring

**Priority:** CRITICAL - solver is submitting fiction, not real solutions
**Created:** 2026-03-28

---

## Problem Summary

The solver submits the same score (84.7e15 wei) every auction because pool reserves
are stuck at zero. Only 1 order gets solved on 1 pool. Score is 110x higher than
real winners. All strategies except `direct` produce 0 valid solutions.

---

## Fix 1: Pool Refresh Batching (CRITICAL)

**File:** `solver-engine/src/pool_indexer.rs` → `refresh_pools()`

**Bug:** Sends ALL 1,395 pools as a single JSON-RPC batch. Alchemy rejects oversized
batches silently (413 or timeout). Reserves stay at 0 forever.

**Fix:** Chunk the batch into groups of 50 pools with a small delay between chunks.

```rust
// In refresh_pools(), replace single batch with chunked batches:
for chunk in addresses.chunks(50) {
    // build batch of 50 eth_calls
    // send batch
    // parse results
    // 200ms delay between chunks
}
```

**Verification:** After deploy, check logs for "Pool reserves loaded from RPC" messages.
Should see hundreds of pools getting real reserves within 30 seconds of startup.

**Cost impact:** No change - same total CUs, just spread across multiple HTTP requests.

---

## Fix 2: Log Refresh Failures Explicitly (CRITICAL)

**File:** `solver-engine/src/pool_indexer.rs` → `refresh_pools()`

**Bug:** RPC errors are logged at warn level but the function returns silently.
No way to tell from logs whether reserves actually updated.

**Fix:** Add info-level logging:
- Log number of pools refreshed with non-zero reserves after each batch
- Log if refresh_pools was called but 0 pools got non-zero reserves
- Log in the main loop when fast/slow cycle actually fires

---

## Fix 3: Strategy Price Key Format (HIGH)

**Files:**
- `solver-engine/src/solver/cow_matching.rs` (lines 186-191)
- `solver-engine/src/solver/router.rs` (lines 333-340)
- `solver-engine/src/solver/split.rs` (lines 688-694)
- `solver-engine/src/solver/mod.rs` (lines 438-445, graph strategy)

**Bug:** These strategies build clearing_prices maps with placeholder keys like
`"match_0x1234..."` or `"route_0x1234...out"` instead of actual token addresses.
CoW driver expects `{ "0xTokenAddress": "price_value" }`.

Even if these strategies scored higher than direct, their solutions would be rejected
by the CoW driver because the price keys don't match any token.

**Fix:** Use actual token addresses (sell_token, buy_token) as clearing_prices keys,
same format as `direct` strategy uses via the assembler.

---

## Fix 4: Simulation Attribution (MEDIUM)

**File:** `solver-engine/src/routes/solve.rs` (lines 169-183)

**Bug:** Only the BEST solution gets simulated. Attribution records "SimPassed" only
for the winning strategy. Dashboard shows other strategies as "0 sim passed" even
though they might produce valid solutions - they just score lower.

**Fix:** Either:
- (a) Simulate all candidate solutions (costs more compute but gives honest metrics), or
- (b) Change dashboard to show "not tested" instead of "0 passed" for non-winning strategies

Option (b) is cheaper and more honest. The funnel should show:
Generated → Best? → Simulated → Submitted → Won

---

## Fix 5: Score Sanity Check (MEDIUM)

**File:** `solver-engine/src/solver/assembler.rs` or `solver-engine/src/solver/mod.rs`

**Bug:** We submit scores 110x higher than winners. CoW won't execute these on-chain
(they'd fail settlement). Submitting fantasy scores wastes our reputation with CoW.

**Fix:** After computing our score, compare against a sanity bound. If we know typical
winner scores are 1e14-1e16 wei, flag anything above 1e17 as suspicious.

Log a warning: "Score {X} exceeds sanity bound - likely stale reserves"

Don't filter (we still want to submit in shadow mode), just make it visible so we
know when scores become realistic.

---

## Verification Checklist (after deploying all fixes)

1. **Pool reserves loading:** `docker logs cow-solver | grep "reserves loaded"` → should see 100+ pools
2. **Scores varying:** Recent Auctions should show DIFFERENT scores per auction, not the same number
3. **Gap shrinking:** Competition tracker Gap should drop from +11,000% toward +100% or lower
4. **Multiple strategies submitting:** Strategy funnel should show multihop/graph with sim passes
5. **RPC calls increasing:** Should see 50-200 RPC calls (not 5) reflecting real refresh activity

---

## Priority Order

1. **Fix 1** (batching) - unlocks everything. Without fresh reserves, nothing else matters.
2. **Fix 2** (logging) - needed to verify Fix 1 worked.
3. **Fix 3** (price keys) - enables other strategies to produce valid solutions.
4. **Fix 5** (sanity check) - makes the dashboard honest about score quality.
5. **Fix 4** (attribution) - nice-to-have for dashboard accuracy.

---

## Expected Outcome

After Fix 1+2: Scores should vary per auction, Gap should drop from 11,000% to
hundreds or low thousands of percent. After Fix 3: multiple strategies should
contribute solutions. After all fixes: we should be able to honestly evaluate whether
our solver is competitive enough for staging.
