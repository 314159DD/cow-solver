# Sprint: Data-Driven Scoring - Stop Guessing, Start Measuring

**Created:** 2026-04-03
**Priority:** CRITICAL - prerequisite for any further scoring work
**Goal:** Build the tooling to compare our scores against winners trade-by-trade, then use that data to make ONE targeted fix.

## Why This Sprint Exists

We've made 9 scoring fix attempts based on theory. Each fixed one layer but revealed the next. We need to stop guessing and start measuring.

The CoW competition API gives us the winning solution's score for every auction. Our replay DB stores our score. We need to connect these and compare at the trade level to find the exact multiplier and which trades cause it.

---

## Part 1: Auction Deep-Dive CLI Tool

**New binary: `src/bin/audit.rs`**

A CLI tool that takes an auction ID and shows a side-by-side comparison of our solution vs the winner.

```
cargo run --bin audit -- --auction-id 6787407
```

Output:
```
Auction #6787407 (964 orders, 251 driver pools)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Our solution:
  Score: 1,457,617,350 gwei (1.46 ETH)
  Trades: 8
  Strategy: direct

Winner (sector-solve):
  Score: 989,243 gwei (0.00099 ETH)
  Ratio: our score is 1474x higher

Per-trade breakdown:
  Trade 1: 0xa00b... (sell 199923 TokenA → TokenB)
    Our surplus:    51,749,742 (buy token atoms)
    Our score:      25,161,779 gwei
    From AMM:       false (CoW match fallback)
    Issue:          ⚠ CoW match using clearing price derivation

  Trade 2: 0x3500... (sell 100000000 TokenC → TokenD)
    Our surplus:    0
    Our score:      0
    From AMM:       true (clamped to price-implied)
    Status:         ✓ Honest zero

  ... (all trades)

Diagnosis:
  - 98% of our score comes from 1 CoW match trade with from_amm=false
  - That trade uses clearing price derivation (not AMM output)
  - Clearing price gives 133M output vs 81M limit = 52M fake surplus
  - Winner likely scores this trade at ~0 or doesn't include it
```

### Implementation:
1. Read auction from replay.db (we store the full JSON)
2. Re-run our scoring on the stored solution
3. Fetch competition data from CoW API for the same auction
4. Compare total score + per-trade if possible
5. Categorize each trade: from_amm vs CoW match vs fallback
6. Flag trades where our surplus is >10x the total winner score

---

## Part 2: Score Comparison Log (Automatic)

**In `routes/solve.rs`, after competition data arrives:**

For every auction where we have both our score AND the winner's score, log a structured comparison:

```rust
info!(
    auction_id,
    our_score_wei,
    winner_score_wei,
    ratio = our_score_wei / winner_score_wei.max(1),
    our_trades = our_trade_count,
    from_amm_trades = amm_count,
    cow_match_trades = cow_count,
    cow_match_score_pct = cow_match_score * 100 / our_score_wei.max(1),
    "Score comparison"
);
```

This runs automatically and builds a dataset over time. After 100 auctions we'll see the pattern clearly.

---

## Part 3: Score Breakdown in Dashboard

**New section in dashboard: "Score Anatomy"**

For the last 10 compared auctions, show:

| Auction | Our Score | Winner | Ratio | AMM Trades | CoW Trades | CoW % of Score |
|---------|-----------|--------|-------|------------|------------|----------------|
| 6787407 | 1.46 ETH  | 989K   | 1474x | 7 (score: 0.001 ETH) | 1 (score: 1.46 ETH) | 99.9% |

This immediately tells us: "99.9% of our inflated score comes from CoW match trades scored via clearing price fallback."

---

## Part 4: Winner Solution Fetcher

**Enhance `competition.rs` to fetch the winner's FULL solution, not just score.**

The CoW API at `https://api.cow.fi/arbitrum_one/api/v1/solver_competition/{auction_id}` returns:
- All solutions submitted by all solvers
- Each solution's score, trades, and solver name
- The winning solution's details

Store in replay.db:
- `winner_solution_json` - full winner solution for offline analysis
- `winner_trades_count` - how many trades the winner included
- `winner_orders` - which order UIDs the winner filled

This lets us answer: "did the winner even include this order?" If not, we're filling orders that winners skip - which means we should skip them too.

---

## Part 5: Trade-Level Scoring Comparison

**The most valuable analysis:** For auctions where we can see the winner's trades:

```
Our trade on order 0xa00b: surplus = 52M, score = 25e15
Winner trade on order 0xa00b: surplus = 0 (not included)

Our trade on order 0x3500: surplus = 0, score = 0
Winner trade on order 0x3500: surplus = 420, score = 136e9
```

This tells us exactly:
- Which orders winners fill that we don't (missed opportunities)
- Which orders we fill that winners skip (wasted compute, fake surplus)
- Per-order score comparison (where our math diverges)

---

## Part 6: Automated Diagnosis Report

**Cron-style: every 100 auctions, generate a diagnosis:**

```
Last 100 auctions analysis:
  Avg our score: 1.2 ETH
  Avg winner score: 0.001 ETH
  Avg ratio: 1200x

  Score sources:
    AMM-based trades: 3% of total score (honest)
    CoW match trades: 97% of total score (inflated)

  Recommendation: Fix CoW match scoring. AMM-based scoring is realistic.
```

Store in `data/diagnosis/` for historical tracking.

---

## Implementation Order

| Part | Effort | Value | Blocks |
|------|--------|-------|--------|
| Part 2 (auto comparison log) | 30 min | HIGH | Nothing - adds data immediately |
| Part 3 (dashboard score anatomy) | 1 hour | HIGH | Visual pattern recognition |
| Part 1 (audit CLI) | 2 hours | HIGHEST | Deep-dive on specific auctions |
| Part 4 (winner solution fetch) | 2 hours | HIGH | Trade-level comparison |
| Part 5 (trade-level compare) | 2 hours | HIGHEST | Exact diagnosis |
| Part 6 (auto diagnosis) | 1 hour | MEDIUM | Trend tracking |

**Start with Part 2** - it's 30 minutes and immediately starts collecting the data we need. Then Part 1 for deep-dives on outliers.

---

## Files to Create/Modify

| File | Action |
|------|--------|
| `solver-engine/src/bin/audit.rs` | NEW - auction deep-dive CLI |
| `solver-engine/src/routes/solve.rs` | Add score comparison logging |
| `solver-engine/src/routes/dashboard.rs` | Add "Score Anatomy" section |
| `solver-engine/src/competition.rs` | Enhance to fetch winner solution details |
| `solver-engine/src/replay.rs` | Add winner_solution_json column |
| `Cargo.toml` | Add `[[bin]] name = "audit"` |

---

## What This Enables

After 24 hours of data collection:
- We know EXACTLY which trade types cause inflation (AMM vs CoW match vs fallback)
- We know which orders winners fill vs skip
- We know the per-trade multiplier between our score and winner's score
- We make ONE targeted fix based on data, not theory
- We verify the fix by watching the ratio drop in real-time on the dashboard

**No more guessing. No more 9-attempt fix cycles. Data in, fix out.**
