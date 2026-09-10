# Sprint: Win Auctions

**Date:** 2026-04-08
**Goal:** Go from 44% ACCURATE / 0 wins to actually winning CoW auctions
**Baseline:** 5,575 auctions, 100% submission, 44.1% ACCURATE, 1.30x median, 0 wins

---

## The Data Tells Us Exactly What's Wrong

From 546 auctions with winner data:
- 34 auctions at 0.95-1.00x - we're 0-5% below the winner
- 9 auctions at 1.00-1.05x - we BEAT the winner's score but still didn't win
- 69 auctions at 1.10-1.30x - 10-30% inflated, driver re-scores us down
- We route the SAME order (`0x11b2ecc9...`) on every auction
- All 4,425 submitted solutions use "direct" strategy, never "aggregator"
- Response time: 10.8s (winners submit in <5s)
- helixbox-solve wins 196/241 ACCURATE auctions (81%)

---

## Fix 1: Parallel Aggregators [EASY - 30 min]

**Problem:** 6 aggregators run sequentially at 3s timeout each = 10s wasted.
Response time is 10.8s when it should be 3.5s.

**File:** `solver-engine/src/liquidity/aggregator/mod.rs` - `best_quote()`

**Current:** Sequential loop with `tokio::time::timeout(3s)` per aggregator.

**Fix:** Spawn all 6 in parallel, collect results:
```rust
let futures: Vec<_> = aggregators.iter().map(|agg| {
    let name = agg.name().to_string();
    async move {
        match tokio::time::timeout(Duration::from_secs(3), 
            agg.get_quote(sell_token, buy_token, sell_amount, chain_id)
        ).await {
            Ok(Ok(quote)) => Some((name, quote)),
            _ => None,
        }
    }
}).collect();
let results = futures::future::join_all(futures).await;
```

**But:** `Aggregator` is not `Send` because of the `Mutex` in rate limiter.
Workaround: use `tokio::join!` macro with explicit arms for each aggregator,
or restructure rate limiters to use `tokio::sync::Mutex`.

**Simpler approach:** Just reduce per-aggregator timeout from 3s to 1s.
6 x 1s = 6s sequential. Most failing aggregators (0x 429, KyberSwap 403)
fail within 200ms anyway - the 3s timeout only hurts when they hang.

**Expected result:** Response time 10.8s -> 6-7s.

---

## Fix 2: Order Diversity [MEDIUM - 1 hour]

**Problem:** We route `0x11b2ecc9...` on every single auction out of ~980 orders.
The assembler sorts by surplus and always picks the same "best" order.

**File:** `solver-engine/src/solver/assembler.rs`

**Root cause:** The assembler finds all routable orders, sorts by surplus,
and picks the top 1 (MAX_DIRECT_TRADES=1). The same order has the highest
surplus every auction because it's a standing limit order with stale pricing.

**Fix options:**

A. **Submit top 3 instead of top 1:** Set MAX_DIRECT_TRADES=3 and let the
   driver pick the best. More trades = the driver has options.
   Risk: more gas deducted per trade.

B. **Dedup by checking if order changed:** Track which order UID we submitted
   last time. If it's the same, also include the 2nd-best order as an
   alternative solution.

C. **Score each order individually:** Instead of one solution with 1 trade,
   submit up to 3 separate solutions (each with 1 different trade). The
   driver picks the best-scoring one.

**Recommended:** Option C - submit 3 solutions with different orders.
The driver re-scores all of them and picks the winner.

---

## Fix 3: Score Inflation [HARD - 2-3 hours]

**Problem:** 1.30x median = our score is 30% higher than what the driver computes.
The driver trusts our clearing prices but re-derives the surplus differently.

**What the driver does:**
```
driver_score = sum_over_trades(
    executed_sell * clearing_price[sell] - executed_buy * clearing_price[buy]
) - gas_cost
```

**What we likely do wrong:**
1. Our UDCP clearing prices might not be uniform - the driver normalizes them
2. We might include surplus from the interaction (pool output > order limit)
   that the driver attributes differently
3. Gas cost calculation might differ (we use estimated gas, driver uses actual)

**Investigation steps:**
1. Take one of the 34 auctions at 0.95-1.00x
2. Decompress the solution_json
3. Manually compute: `executed_sell * price[sell] - executed_buy * price[buy]`
4. Compare to our `our_score_wei`
5. The difference = what the driver disagrees with

**File:** `solver-engine/src/solver/assembler.rs` - `rescore_solutions()`
and `normalize_surplus()`

This is the hardest fix but the most impactful. If we can get from 1.30x to
1.05x, the 34 near-win auctions become actual wins.

---

## Fix 4: Aggregator Strategy [MEDIUM - 1 hour]

**Problem:** Phase 5 (aggregators) runs on every auction but never beats
the direct strategy. All 4,425 submissions are "direct".

**Investigation:**
1. The agg_solver queries top 5 orders by value
2. Each query hits all active aggregators
3. If aggregator quote > our internal route, it should win
4. But if our internal route is already inflated (Fix #3), the
   aggregator can't beat our inflated score even with a better route

**Likely root cause:** The direct solver produces inflated scores.
The aggregator produces honest scores. The inflated direct score always wins
the internal comparison. But the driver would prefer the aggregator's score
because it's more accurate.

**Fix:** After Fix #3 (deflating scores), aggregators might naturally win.

**Alternative:** Submit BOTH solutions - one from direct, one from aggregator.
Let the driver pick. This way we don't need to compare internally.

**File:** `solver-engine/src/solver/mod.rs` - Phase 5 result handling

---

## Fix 5: Competition Tracker Coverage [EASY - 30 min]

**Problem:** Only 546/5,575 (10%) auctions have winner data.

**File:** `solver-engine/src/competition.rs`

**Likely causes:**
1. `queue_lookup` only fires when `best_score_wei > 0` (line 408 in solve.rs)
   - the 1,150 empty auctions never get queued. That accounts for 20%.
2. The remaining 80% loss: competition tracker polls at 30s delay, but if
   many auctions queue up, the sequential polling (500ms per API call) can't
   keep up. 5,575 auctions / 2 per second = 46 minutes of backlog.
3. Rate limiting from CoW API (shouldn't be an issue at 2 req/s)

**Fix:** 
1. Also queue auctions with score=0 for competition lookup (learn from losses)
2. Increase polling parallelism: batch 5 lookups in parallel instead of 1
3. Reduce poll delay from 30s to 15s (Arbitrum settles fast)

---

## Implementation Order

```
Fix 1 (parallel/faster aggregators) → 30 min
    ↓ deploy, verify response time drops
Fix 5 (competition tracker) → 30 min  
    ↓ deploy, verify coverage increases
Fix 2 (order diversity) → 1 hour
    ↓ deploy, collect data, verify score variety
Fix 3 (score inflation) → 2-3 hours
    ↓ this is the big one - needs investigation first
Fix 4 (aggregator strategy) → 1 hour
    ↓ may resolve naturally after Fix 3
```

Total: ~5-6 hours of focused work.

---

## Success Criteria

| Metric | Current | Target |
|--------|---------|--------|
| Submission rate | 100% | 100% (maintain) |
| Response time | 10.8s | <5s |
| ACCURATE | 44% | >60% |
| Median ratio | 1.30x | 0.95-1.05x |
| Would-win | 0 | >10 per day |
| Competition coverage | 10% | >50% |
| Unique orders routed | 1 | 3+ per session |

---

## What This Looks Like When We Win

An auction arrives. 980 orders, 554 pools. Phase 1 finds 3 good orders in
300ms. We submit 3 solutions (one per order). The driver re-scores all 3,
picks the best, and our score is within 5% of other solvers. We win because
our submission was fast (3.5s vs 5s) and our routing found the same optimal
pool the winner would have used.

Estimated win rate: 2-5% of auctions (10-25 wins per day on Arbitrum).
At 867K gwei average score = ~$0.50 per win in surplus capture.
10-25 wins/day x $0.50 = $5-12.50/day = $150-375/month from CoW alone.

Not life-changing from one chain. But proof the system works - then we
scale across chains and protocols (the matrix).
