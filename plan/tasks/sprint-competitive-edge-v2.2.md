# Sprint: Competitive Edge v2.2

**Date:** 2026-04-07
**Goal:** Close the last 1% gap to winning auctions

---

## Background

After v2.1 achieved 0.90x-0.99x on matching auctions (21% ACCURATE), research
identified 5 specific improvements to close the gap. All 5 implemented.

Research report: `plan/research/theonepercentgap.md`

---

## Tasks - All Complete

### 1. Internalization Module ✅
- **NEW:** `solver-engine/src/solver/internalization.rs`
- **MOD:** `solver-engine/src/solver/mod.rs` - added `pub mod internalization;`
- **MOD:** `solver-engine/src/solver/assembler.rs` - calls `try_internalize_interactions()` after building interactions
- Checks `TokenInfo.trusted` and `available_balance` (fields were parsed but never used)
- 6 unit tests added

### 2. CoW AMM Baseline Filter ✅
- **MOD:** `solver-engine/src/solver/cow_matching.rs` - added `filter_by_amm_baseline()`
- **MOD:** `solver-engine/src/solver/mod.rs` - calls filter after `find_cows()`
- Routes each order through DirectSolver, compares CoW surplus vs AMM surplus
- Keeps match only if CoW beats AMM for BOTH orders

### 3. High-Competition Pair Deprioritization ✅
- **MOD:** `solver-engine/src/triage.rs` - added `is_high_competition_pair()` + `HIGH_COMPETITION_TOKENS`
- **MOD:** `solver-engine/src/solver/assembler.rs` - weighted priority in Step 2.6 sort
- WETH/USDC/USDT pairs get -100 penalty; limit orders +50; partially fillable +30

### 4. CIP-67 Liquid Pair Filter ✅
- **MOD:** `solver-engine/src/solver/cow_matching.rs` - skip in `find_cows()` loop
- Prevents CoW matches on high-competition pairs that would trigger CIP-67 rejection

### 5. Protocol Fee Bonus ✅
- **MOD:** `solver-engine/src/solver/scoring.rs` - `estimate_protocol_fee_bonus()`
- **MOD:** `solver-engine/src/solver/assembler.rs` - adds bonus after gas subtraction
- Market 15%, Limit 30%, Liquidity 0%
- **Gated by `PROTOCOL_FEE_BONUS=true` env var** - default OFF

### 6. Odos Budget Limiter ✅ (from earlier session)
- **MOD:** `solver-engine/src/liquidity/aggregator/mod.rs` - `BudgetLimiter` struct
- **MOD:** `solver-engine/src/liquidity/aggregator/odos.rs` - budget check in `get_quote()`
- Rolling 24h/1h windows, configurable via env vars
- Rate limit bumped to 1000ms (1 RPS for free tier)

---

## Test Results

**474 tests passing**, 4 pre-existing failures (unrelated).

### Harness Results (50 settled auctions)

| Metric | VPS v2.1 | v2.2 | Change |
|--------|----------|------|--------|
| ACCURATE | 8 (16%) | 13 (26%) | +62% |
| LOW+VERY_LOW | 14 (28%) | 5 (10%) | -64% |
| 0.99x auctions | ~3 | 11 | +267% |

Individual auction improvements:
- 6831816: 77.8x → 0.99x
- 6831843: 233x → 2.72x
- 6831698: 0.06x → 0.68x

### What didn't change
- ~867K gwei constant on INFLATED auctions = pre-existing routing issue
- These auctions need Odos aggregator routing to find the winner's route
- Odos was disabled (`AGG_ENABLED=false`) during this test

---

## Next: Deploy & Collect Data

1. Deploy v2.2 to VPS with `AGG_ENABLED=true` (Odos enabled)
2. Run 2-4 hours to collect fresh replay data
3. Pull replay DB, re-test locally
4. Measure: did Odos convert INFLATED → ACCURATE?
5. If yes: enable `PROTOCOL_FEE_BONUS=true` and push for 1.0x+
