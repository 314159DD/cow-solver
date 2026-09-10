# Scoring Bug Postmortem - What We Tried and Why It Failed

**Date:** 2026-04-02 to 2026-04-03
**Problem:** Scores inflated by 1000x-10,000,000x vs winners (45B gwei vs 20K gwei)
**Status:** Still fixing as of 2026-04-03 00:20 UTC

## The Root Cause Chain

The scoring inflation has THREE layers, each masking the next:

### Layer 1: Per-pair UDCP prices (FIXED)
- `compute_udcp()` sets `price[token] = total_out` per pair
- Shared tokens (USDC in WETH→USDC and USDC→WBTC) get overwritten
- HashMap `extend()` keeps last value → inconsistent global prices
- **Fix:** Use reference_prices from auction tokens as clearing prices
- **Commit:** `3e87447` - Remove two-pass solving
- **Result:** Scores dropped from 4.6e19 to 1.28e18

### Layer 2: rescore_solutions used solution's own prices (FIXED)
- Even after assembler was fixed, cow_matching/fallback/graph still built
  solutions with per-pair UDCP prices
- `rescore_solutions` used each solution's `prices` map → inflated
- **Fix:** Replace ALL solution prices with full reference price map before scoring
- **Commit:** `6408a9e` - Case-insensitive lookup + no fallback
- **Result:** No change - case mismatch caused fallback to old prices

### Layer 2.5: Case mismatch in price replacement (FIXED)
- Solution used checksummed addresses (0xAf88...), ref_prices had lowercase
- Lookup failed → fallback kept broken per-pair price
- **Fix:** Store both original and lowercase keys, no fallback (drop unknown tokens)
- **Commit:** `f050667` - Both keys, no fallback
- **Result:** Scores still ~1.28e18

### Layer 3: Clearing prices ≠ execution reality (FIXING)
- Reference prices imply exchange rate X
- AMM pools deliver exchange rate Y (different, often worse)
- `compute_cow_score` derived `bought = executed × cp_sell / cp_buy` from prices
- This gave MORE output than the AMM actually produces → fake surplus
- Example: price says user gets 158K, AMM gives 153M (wrong pool reserves)
- **Fix attempt 1:** Use AMM output directly from interactions → FAILED because
  AMM output itself was from stale/mismatched pools (153M vs 158K limit)
- **Fix attempt 2:** Clamp to `min(amm_output, price_implied_output)` - CURRENT

## What We Tried (Chronological)

### Attempt 1: Cap surplus at 10% of executed (FAILED)
- `capped_surplus = surplus.min(executed / 10)`
- Why it failed: The raw surplus was 100x+ the executed amount from wrong
  clearing prices. 10% of a massive number is still massive.
- Lesson: **Caps are band-aids. Fix the root cause.**

### Attempt 2: Use interaction amounts instead of clearing prices (FAILED)
- Matched interactions to trades by token pair
- Why it failed: Multiple trades sharing same pair → ALL interactions summed
  for each trade → N× cross-trade inflation
- Lesson: **Can't match interactions by token pair when multiple trades share it**

### Attempt 3: Back to clearing prices with 10% cap (FAILED)
- Same as attempt 1 with different cap logic
- Same result - clearing prices were the problem, not the cap
- Lesson: **Stop trying to cap. Fix the prices.**

### Attempt 4: Disable rescore entirely (PARTIAL)
- Used assembler's `normalize_surplus` as final score
- Scores = 5.7e17 (better than 4.6e19 but still 1000x too high)
- Why still wrong: `normalize_surplus` uses AMM output which can be wrong
- Lesson: **Any scoring based on stale AMM output will be inflated**

### Attempt 5: Reference prices as clearing prices (PARTIAL)
- Set solution.prices = reference_prices from auction tokens
- Re-enabled rescore_solutions
- Why still inflated: Other strategies (cow, fallback, graph) still had
  per-pair prices → rescore used those broken prices
- Lesson: **Must fix ALL code paths, not just assembler**

### Attempt 6: Replace ALL solution prices in rescore (PARTIAL)
- rescore_solutions now replaces solution.prices before scoring
- Why still inflated: Case mismatch (checksummed vs lowercase) → fallback
  to old broken prices for some tokens
- Lesson: **Case sensitivity kills. Always normalize.**

### Attempt 7: Case-insensitive + no fallback (PARTIAL)
- Both original and lowercase keys in ref_prices
- Drop tokens without reference prices entirely
- Scores dropped to 1.28e18 - still 1000x too high
- Why: `compute_cow_score` derives bought from prices, but reference prices
  imply different rate than AMM delivers. The GAP = fake surplus.
- Lesson: **Reference prices ≠ AMM execution. The gap IS the fake surplus.**

### Attempt 8: Use AMM output from interactions, index-matched (PARTIAL)
- Match trade[i] to interaction[i] (same build order)
- Use interaction.output_amount directly
- Why still inflated: AMM output = 153M when limit = 158K. The AMM itself
  is computing wrong output (stale reserves or wrong pool matched)
- Lesson: **AMM output is only trustworthy if pool reserves are fresh**

### Attempt 9: Clamp to min(AMM, price-implied) (CURRENT)
- `bought = min(amm_output, price_implied_output)`
- Price-implied acts as sanity ceiling
- **Status: Deployed, awaiting results**

## Key Lessons (DO NOT REPEAT)

1. **Caps don't work.** If the underlying data is wrong, capping the output
   is just hiding the problem. Fix the data source.

2. **Case sensitivity is lethal.** Token addresses can be checksummed or
   lowercase. ALWAYS normalize to lowercase before comparison.

3. **Per-pair prices are fundamentally broken for multi-pair solutions.**
   Shared tokens get different prices. Must use ONE global price vector.

4. **Reference prices ≠ execution prices.** Reference says 1 WETH = 2700 USDC.
   AMM says 1 WETH = 2650 USDC (slippage). The 50 USDC gap becomes fake surplus
   if you score from reference but execute on AMM.

5. **AMM output is only honest if reserves are fresh.** Stale reserves →
   AMM computes output that doesn't match on-chain reality → fake surplus.

6. **The driver scores from its own prices.** Our score must match what the
   driver computes from our clearing prices. If we submit reference prices as
   clearing, the driver derives the same numbers → honest comparison.

7. **rescore_solutions MUST replace prices.** Any code path (cow_matching,
   fallback, graph, split, router) that builds a solution with custom prices
   will get those prices used by rescore if not replaced first.

## What SHOULD Work (Theory)

The correct scoring chain:
1. Route through AMM pools → get actual output amounts
2. Set clearing prices = reference_prices (globally consistent)
3. Score = min(amm_output, price_implied_output) - limit_buy
4. Normalize: score × native_price / 1e18

This binds scoring to BOTH:
- AMM reality (can't promise more than pools deliver)
- Price reality (can't claim more surplus than market rates allow)

The lower of the two is the honest surplus.

## Winner Score Reference

From competition tracker data:
- Typical winner: 1e12-1e16 wei (1-10M gwei)
- Top winner: ~1e16 wei (10M gwei ≈ 0.01 ETH ≈ $18)
- Our target: same range
- Current (after all fixes): TBD - awaiting deploy of attempt 9
