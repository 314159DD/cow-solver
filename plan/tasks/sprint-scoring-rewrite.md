# Sprint: Scoring Rewrite (v5 )

**Created:** 2026-04-03
**Priority:** BLOCKER

## Root Cause

Per-pair UDCP prices → shared tokens overwritten → inconsistent global prices → 80x score inflation.

---

## Phase 1 - VALID BASELINE

### 1.1 Reference prices as clearing prices

```rust
for (addr, token_info) in &auction.tokens {
    if let Some(ref_price) = &token_info.reference_price {
        all_prices.insert(addr.to_lowercase(), ref_price.clone());
    }
}
```

### 1.2 Re-enable rescore_solutions

### 1.3 Bind prices to feasible execution - with global consistency check

When AMM limits us, adjust the PRICE to match reality. BUT: after ANY price adjustment, immediately revalidate ALL trades touching that token - not just the one that triggered the adjustment.

```rust
let price_buy = sell_amount * price[sell_token] / price[buy_token];
let amm_buy = route.output_amount;

if amm_buy < price_buy {
    // Adjust buy_token price UP to match AMM reality
    let new_price = sell_amount * price[sell_token] / amm_buy;
    price[buy_token] = new_price;

    // CRITICAL: revalidate ALL trades using this token
    // (not just the one that triggered the adjustment)
    if any_trade_invalidated_by_price_change(buy_token, &price, &all_trades) {
        // This adjustment breaks other trades - revert and skip THIS trade instead
        price[buy_token] = original_price;
        skip_trade();
    }
}
```

This prevents the drift problem: adjusting price for trade A silently breaking trade B.

### 1.4 Constraints (no circular dependency)

```rust
let executed_buy = sell_amount * price[sell] / price[buy];

if executed_buy < limit_buy { skip_trade(); }

let lhs = sell_amount * price[sell];
let rhs = executed_buy * price[buy];
if lhs < rhs { skip_trade(); }
```

### 1.5 Skip-trade awareness - don't silently fragment solutions

When `skip_trade()` fires during price binding, it can fragment a solution (keep 4 trades, drop 1). This might create a worse global result than rebuilding without that trade entirely.

```rust
if must_skip_trade {
    warn!(order = %order.uid, "Trade skipped during price binding");
    // Check: does removing this trade reduce total solution value significantly?
    let remaining_value = total_score_without(trade);
    let original_value = total_score;
    if remaining_value < original_value * 70 / 100 {
        // Losing >30% of solution value - rebuild from scratch without this order
        return rebuild_solution_excluding(order);
    }
    // Otherwise just skip this one trade
}
```

At minimum: always log skipped trades so we can see how often this happens and whether it correlates with losses.

### 1.6 Remove fake scoring paths

Kill `compute_score()` and all `route.surplus → Score::Solver` paths. Keep AMM math for routing/feasibility only.

### 1.7 Sanity check (relative to winners)

```rust
let avg_winner = competition::avg_recent_winner_score();
if avg_winner > 0 && score > avg_winner * 10 {
    warn!("Score exceeds 10x avg winner");
}
```

---

## Phase 2 - PRICE IMPROVEMENT

### 2.1 Directional intelligence BEFORE grid search

Don't blindly try both directions. Compute a surplus gradient first:

```rust
for token in solution_tokens {
    let base = price[token];

    // Probe: does increasing price increase total surplus?
    price[token] = base * 10001 / 10000; // +0.01% probe
    let score_up = recompute_total_score(&price, &trades, &tokens);
    price[token] = base; // reset

    // Determine direction
    let direction = if score_up > current_score { 1 } else { -1 };

    // Only search in the profitable direction
    let deltas = if direction > 0 { [10, 25, 50, 100] } else { [-10, -25, -50, -100] };
    // ... search within deltas
}
```

This cuts search space in half and converges faster.

### 2.2 Iterative global passes (3 rounds)

```rust
for _iteration in 0..3 {
    for token in solution_tokens {
        // Directional probe + search (2.1)
        // After each adjustment, revalidate ALL affected trades
    }
}
```

### 2.3 Soft scaling instead of hard reverts

When a price adjustment makes one trade infeasible but improves 5 others, don't fully revert. Scale down:

```rust
if new_executed > amm_output {
    // Don't fully revert - try half the adjustment
    let half_adjustment = (new_price - base_price) / 2;
    price[token] = base_price + half_adjustment;

    // Re-check
    if still_infeasible() {
        // Scale down again or revert
        price[token] = base_price;
    }
}
```

This turns binary success/fail into continuous optimization. A 0.3% improvement that works is better than a 0.5% improvement that gets reverted.

---

## Phase 3 - SOLUTION VARIANTS

### 3.1 Four variant types (not three)

```rust
let mut all_candidates = Vec::new();

// Variant 1: Pure AMM routing
all_candidates.push(solve_amm_only(&auction));

// Variant 2: Pure CoW matching (internalization)
all_candidates.push(solve_cow_only(&auction));

// Variant 3: Hybrid (CoW match + AMM remainder)
all_candidates.push(solve_hybrid(&auction));

// Variant 4: Partial fills (fill profitable portion only)
all_candidates.push(solve_partial_fills(&auction));
```

### 3.2 Partial fill exploration (high ROI, currently missing entirely)

For large orders or high-slippage pairs, filling 100% may be unprofitable but 50% could be highly profitable:

```rust
for order in &auction.orders {
    for fraction in [100, 75, 50, 25] {
        let partial_sell = order.sell_amount * fraction / 100;
        let route = solve_order_with_amount(order, partial_sell, liquidity);
        if route.surplus_per_unit > best.surplus_per_unit {
            best = route; // Better surplus rate at partial fill
        }
    }
}
```

**Why this matters:** Large orders push deep into the AMM curve where slippage kills surplus. Filling 50% at 2x the surplus rate = same total surplus at half the gas. Winners do this heavily.

**Floor filter - don't spam dust:**

```rust
// Minimum notional value to avoid wasting compute on meaningless partial fills
const MIN_PARTIAL_SELL_WEI: u128 = 1_000_000_000_000_000; // ~$3 at current ETH prices

if partial_sell_value < MIN_PARTIAL_SELL_WEI {
    continue; // Skip dust - not worth the candidate slot
}
```

---

## Phase 4 - COMPETITION LEARNING

Every loss is signal:

```
consistently 3% below → price improvement too weak (Phase 2)
consistently 50% below → routing gap
winning small, losing large → need partial fills (Phase 3.2)
losing to specific solver → study their pattern
```

---

## Implementation Priority

| Phase | Effort | Impact |
|-------|--------|--------|
| Phase 1 (baseline) | 3 hours | CRITICAL - honest scores, first wins |
| Phase 2 (price improvement) | 2 hours | HIGHEST - 10x win rate |
| Phase 3.1 (variants) | 1 hour | HIGH - unlocks internalization |
| Phase 3.2 (partial fills) | 2 hours | HIGH - unlocks large order wins |

---

## Files to Change

| File | Change |
|------|--------|
| `solver-engine/src/solver/assembler.rs` | Reference prices, price-execution binding, remove fake scoring |
| `solver-engine/src/solver/mod.rs` | Re-enable rescore, constraints, price improvement, forced variants |
| `solver-engine/src/solver/scoring.rs` | Keep compute_cow_score (correct with consistent prices) |
| `solver-engine/src/solver/direct.rs` | Add partial fill support (fraction parameter) |

---

## Expected Outcomes

| Metric | Current | After Phase 1 | After Phase 2+3 |
|--------|---------|---------------|-----------------|
| Score range | 45B gwei (fake) | 1K-10M gwei | 1K-10M gwei (optimized) |
| Win rate | 0% | 0.5-2% | 3-10% |
| Gap to winner | +1,000,000% | -5% to -50% | -1% to -10% |
