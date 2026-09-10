# CRITICAL: Scoring Formula Rewrite

**Priority:** BLOCKER - nothing else matters until this is fixed
**Created:** 2026-04-01
**Status:** Not started

---

## The Problem In Plain English

Our solver finds good routes (fresh V3 data from the driver, 257 pools per auction). But when we calculate "how much surplus did we find?", our number is completely wrong - sometimes 7x too high, sometimes 177x too high. We've been putting caps on the number to force it into a reasonable range, but that's like putting a speed limiter on a car with a broken speedometer. The car still doesn't know how fast it's going.

The CoW driver **re-calculates the score itself** using its own formula. Our self-reported score doesn't determine whether we win - the driver's calculation does. But our wrong score means:
1. We can't compare ourselves to competitors honestly
2. We pick the wrong solution internally (we think solution A is better than B, but the driver disagrees)
3. Our dashboard numbers are meaningless

---

## What Our Formula Does (Wrong)

**File:** `solver-engine/src/solver/assembler.rs`, function `normalize_surplus()`

```
For each order we fill:
  1. surplus_raw = output_amount - buy_amount_limit  (in buy-token atoms)
  2. normalized = surplus_raw × reference_price(buy_token) / 1e18
  3. CAPPED at some arbitrary number (currently 2e14)

Total score = sum of normalized across all orders - gas_cost
```

### What's wrong with each step:

**Step 1 - surplus_raw:** This part is mostly correct. We compute how much more the user gets vs their limit price.

**Step 2 - reference_price multiplication:** This is where the 7x-177x inflation comes from. The `reference_price` values in the auction payload are NOT simple "ETH per token atom" values. They include decimal scaling factors that our formula doesn't account for correctly. 

We verified this on 2026-03-30: for a real auction, our formula produced 7.18e15 while the winner scored 9.96e14 for the SAME order. That's 7.2x off. The ratio varies per token pair because different tokens have different decimal scaling in their reference prices.

**Step 3 - the cap:** This is pure band-aid. We've changed this cap ~15 times (1e14, 5e14, 1e15, 5e15, 2e14...). No value works because the underlying number is wrong. Low cap = we always lose. High cap = we always look inflated. There is no correct cap because the formula itself is wrong.

---

## What CoW Protocol Actually Computes

**Source:** `cowprotocol/services/crates/driver/src/domain/competition/solution/scoring.rs`

The reference implementation computes score per trade as:

```
For SELL orders:
  limit_buy = ceil(executed_amount × limit_buy_amount / limit_sell_amount)
  bought = ceil(executed_amount × custom_price_sell / custom_price_buy)
  surplus = bought - limit_buy  (in buy-token atoms)
  
  score = (surplus + protocol_fees) × native_price(buy_token)

For BUY orders:
  surplus is computed in sell-token units first
  then converted to buy-token units using the ORDER's limit price
  (NOT the clearing price - this is a deliberate design choice per Draft CIP)
  
  score = (surplus_in_buy_tokens + protocol_fees) × native_price(buy_token)
```

### Key differences from our formula:

1. **`custom_price_sell / custom_price_buy`** - the CoW driver uses per-trade custom clearing prices, not the uniform clearing prices. These include fee adjustments. We use the raw clearing prices from our solution.

2. **Protocol fees are ADDED to the score** - the CoW formula is `surplus + fees`, not just `surplus`. We completely ignore protocol fees. This alone could account for a significant portion of the discrepancy.

3. **Buy order handling** - for buy orders, surplus is converted using the limit price ratio, not the clearing price. We don't distinguish buy vs sell orders in our normalization.

4. **`native_price` vs `reference_price`** - these might be the same values but applied differently. The CoW code calls `.in_eth()` which may have a different scaling than our `× ref_price / 1e18`.

5. **Ceiling division** - the CoW code uses ceiling division (`ceil()`) in multiple places. We use floor division everywhere. This creates small but consistent discrepancies that compound across trades.

---

## Evidence of the Problem

### Test 1: Same order, different scores (2026-03-30)
- Auction 6720966, order selling token 0x0c06... for WETH
- Winner (dcentralab-solve): score 1,812,509,937,870,664
- Our formula: 7,178,961,085,910,546 (7.2x higher)
- The 7.2x factor was consistent across multiple comparable auctions

### Test 2: Fresh V3 data, still wrong (2026-04-01)
- Auction 6770187, winner (rizzolver): score 452,123,922,653,168
- Our uncapped score: 80,329,234,744,957,549 (177x higher)
- With 30 trades, per-trade we're ~6x too high
- The inflation INCREASED because we now find more routes (fresh V3 data = more trades = more compounding error)

### Test 3: Cap hunting doesn't work
- Cap at 1e14: constant 1.2M gwei, +21% on small auctions, -95% on big ones
- Cap at 5e14: constant 5.5M gwei, all inflated
- Cap at 5e15: constant 100M gwei, everything capped
- Cap at 2e14: constant 3.6M gwei, -91% on real auctions
- No single cap value produces correct results because the per-trade error varies by token pair

---

## The Fix

### Step 1: Port the exact scoring function from CoW reference code

File to port: `cowprotocol/services/crates/driver/src/domain/competition/solution/scoring.rs`

The function `compute_score()` takes trades and native_prices, returns the score. Port this into our `solver/scoring.rs`. It's ~200 lines of Rust.

Key functions to port:
- `Trade::score()` - per-trade score with fee handling
- `Trade::surplus_over()` - surplus computation with ceiling division
- `score_buy_order()` - special buy-order handling via limit price conversion
- Protocol fee computation (surplus fee, volume fee, price improvement fee)

### Step 2: Use the ported function instead of normalize_surplus()

Replace the current flow:
```
assembler.rs → normalize_surplus() → per-order cap → sum → global cap
```

With:
```
scoring.rs → compute_score(trades, native_prices) → no cap needed
```

### Step 3: Remove ALL caps

Once the formula matches, the scores will be naturally correct. No per-order cap, no global cap. The scores should match winners within ~5% (accounting for different routes, not different math).

### Step 4: Validate

For each settled auction:
1. Fetch winner's score from competition API
2. Compute our score using the new formula
3. Compare: if we filled the SAME orders through the SAME pools, our score should match within 1%
4. If the gap is > 5%, there's still a formula bug

---

## What NOT To Do

- **Do not adjust the cap again.** There is no correct cap value.
- **Do not add more aggregators.** The routes are fine. The score is wrong.
- **Do not change pool refresh timing.** Data freshness is solved (257 driver pools per auction).
- **Do not refactor the pipeline.** The architecture is fine. Only `normalize_surplus()` and the scoring in `assembler.rs` needs to change.

---

## Files To Change

| File | Change |
|------|--------|
| `solver/scoring.rs` | Add `compute_score_cow()` ported from CoW reference |
| `solver/assembler.rs` | Replace `normalize_surplus()` calls with `compute_score_cow()` |
| `solver/assembler.rs` | Remove per-order cap (the `.min(2e14)` lines) |
| `solver/mod.rs` | Remove global solution cap in `finalize_solutions()` |
| `solver/cow_matching.rs` | Use new scoring for CoW match surplus |
| `solver/split.rs` | Use new scoring for split solutions |

---

## Reference Links

- CoW scoring source: `github.com/cowprotocol/services/blob/main/crates/driver/src/domain/competition/solution/scoring.rs`
- Our current formula: `solver-engine/src/solver/assembler.rs` line ~55 (`normalize_surplus`)
- The 7.2x analysis: conversation from 2026-03-30 (traced real auction scoring math)
- CoW full docs: `docs.cow.fi/llms-full.txt`

---

## Expected Outcome

After this rewrite:
- Scores match winners within 5% when routing through same pools
- No caps needed - scores are naturally correct
- Dashboard shows honest competition data
- Win rate becomes meaningful (we win when our ROUTES are better, not when our MATH is wrong)
- Revenue projections become trustworthy
