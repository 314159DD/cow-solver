# Sprint: Score Optimization - Make Our Solutions Worth Picking

**Goal:** Maximize `score = surplus - gas` per the CoW scoring formula.
**Context:** Shadow competition live. Infrastructure complete. Now purely about economic quality.

---

## What CoW Actually Scores (from docs)

```
score(order) = (surplus(order) + protocol_fees(order)) × reference_price(buy_token)
```

- **Sell order surplus** = `output_received - (sell_amount / limit_price)` = extra buy tokens beyond limit
- **Buy order surplus** = `(sell_limit × limit_price) - actual_sell` = sell tokens saved
- **reference_price** = externally-provided price of the buy token in ETH (from the auction payload)

The **solution score** = sum of all order scores. Winner = highest solution score.

### Critical insight we're currently missing

**We do NOT multiply surplus by `reference_price(buy_token)`.**

Our current scoring: `surplus = output - limit` (raw token units, no normalization)

If an order buys USDC (worth ~0.00037 ETH per atom), and another buys WETH (worth ~1 ETH per atom), we're treating 1 unit of USDC surplus the same as 1 unit of WETH surplus. This is fundamentally wrong and explains why our scores look inflated - high-decimal low-value tokens produce huge raw surplus numbers that mean nothing.

---

## Priority 1: Fix Score Formula (CRITICAL - this alone could 10x our competitive position)

**File:** `solver-engine/src/solver/assembler.rs` (line 155-200)

### Current (broken)
```rust
total_surplus += route.surplus;  // raw buy-token units, not normalized
let net_score = total_surplus.saturating_sub(total_gas_cost);
```

### Correct (per CoW formula)
```rust
for route in &profitable {
    // Get reference price for the buy token from auction.tokens
    let ref_price = auction.tokens
        .get(&order.buy_token)
        .and_then(|t| t.reference_price.as_ref())
        .and_then(|p| p.parse::<f64>().ok())
        .unwrap_or(0.0);

    // score(order) = surplus_in_buy_token × reference_price
    // reference_price is "atoms of ETH per atom of buy_token"
    // So: score_in_eth_atoms = surplus × ref_price
    let surplus_normalized = (route.surplus as f64 * ref_price) as u128;
    total_score += surplus_normalized;
}
let net_score = total_score.saturating_sub(total_gas_cost);
```

### Also fix in:
- `solver-engine/src/solver/direct.rs` - `solve_auction()` line 139-145 (uses raw `total_surplus`)
- `solver-engine/src/solver/split.rs` - `build_split_solution()` line 643-699 (uses raw `total_surplus`)
- `solver-engine/src/solver/router.rs` - wherever it sums surplus
- `solver-engine/src/solver/graph.rs` - same

**Every strategy must use the same normalized scoring.**

### Expected impact
- Scores become comparable across token pairs
- USDC surplus of 1,000,000 (6 dec) × ref_price ≈ 370,000,000,000 (in ETH atoms) = realistic
- WETH surplus of 1,000,000 × ref_price ≈ 1,000,000,000,000,000,000 = realistic
- Split won't dominate just because it deals with high-decimal tokens

---

## Priority 2: Use Clearing Prices, Not Approximations

**File:** `solver-engine/src/solver/direct.rs` (line 105-132)

### Current (broken)
```rust
let sell_price = "1000000000000000000"; // hardcoded 1e18
let buy_price = (ratio * 1e18) as u128;  // approximate from route
prices.entry(order.sell_token.clone()).or_insert_with(|| sell_price.to_string());
```

Each order inserts prices independently. If two orders trade the same pair, the first one's price sticks and the second is ignored. This violates UDCP.

### Correct
Clearing prices must be computed AFTER all routes are determined, not per-order. The flow should be:

1. Collect all routes
2. Group by token pair direction
3. For each group, pick one price (the weighted average, or the best pool's execution price)
4. All orders in that group get that price

**UDCP enforcement** is partially in `assembler.rs` already (`compute_clearing_prices`). Verify it's being used for ALL strategies, not just the assembler path.

### Expected impact
- Valid UDCP prices → solutions won't be rejected
- Consistent pricing across orders → CoW driver accepts our solution

---

## Priority 3: Honest Gas Estimation

**File:** `solver-engine/src/gas/mod.rs`

### Current state
Gas estimation uses static constants per pool type. On Arbitrum, gas = L2 execution + L1 calldata.

### Improvements needed
1. **L1 calldata cost** - our gas oracle (B.3) fetches ArbGasInfo every 15s. Verify it's being used in the scoring path, not just cached.
2. **Per-interaction gas** - different DEXs have very different gas costs:
   - Uniswap V2 swap: ~60k gas
   - Uniswap V3 swap: ~120k gas
   - Camelot swap: ~80k gas
   - Multi-hop (2 swaps): ~200k gas
   - Split (3-way): ~300k gas
3. **Solution overhead** - settlement contract call overhead, token approvals, etc.

If we overestimate gas, we reject profitable solutions. If we underestimate, we submit solutions that are less profitable than we think (CoW driver may still accept but our rank drops).

### Expected impact
- Accurate net_score = accurate ranking
- Don't submit when gas > surplus (wastes solver bond)

---

## Priority 4: Multi-Strategy Fair Competition

### Problem
Split dominates because it produces higher raw surplus numbers (more pool interactions = more output), but:
1. It also costs more gas (multiple swaps)
2. Its surplus may be inflated if not normalized by reference_price

### Fix (after Priority 1 is done)
Once all strategies use normalized scoring:
1. Run all strategies in parallel (already done via iterative deepening)
2. Each produces a `Solution` with a properly normalized `Score::Solver { score }`
3. The phase orchestrator picks the highest `net_score = surplus_normalized - gas_cost`
4. If direct beats split after proper normalization, direct wins

### Additional: Strategy-specific tuning
- **Direct**: Should win on simple single-pool orders (ETH/USDC, ETH/WBTC)
- **Split**: Should win on large orders where single-pool price impact is high
- **Multi-hop**: Should win on exotic pairs (e.g. TOKEN_A → WETH → USDC)
- **CoW matching**: Should win when two opposite orders exist (zero gas, pure surplus)

If after normalization, split still wins 100%, that's fine - it means split genuinely produces better economic outcomes. But we should log the comparison.

---

## Priority 5: Expand Pool Coverage

### Current: 20 pools (SushiSwap V2 + Camelot V2)
### Target: 100+ pools across all major Arbitrum DEXs

| DEX | Type | Status | Action |
|-----|------|--------|--------|
| SushiSwap V2 | ConstantProduct | ✅ 10 pools | Add more pairs |
| Camelot V2 | ConstantProduct | ✅ 10 pools | Add more pairs |
| Uniswap V3 | ConcentratedLiquidity | ❌ | Add factory discovery + tick math |
| Balancer V2 | WeightedProduct | ❌ | Add vault query + weighted math |
| Curve | StableSwap | ❌ | Add registry + stable math |
| GMX V2 | Oracle-based | ❌ | Lower priority |

More pools = more routing options = higher surplus potential.

### Factory-based discovery
Instead of hardcoding pool addresses:
```rust
// For each factory, call getPair(tokenA, tokenB) for top 20 token pairs
let pairs = factory.getPair(WETH, USDC).call().await;
```

This was planned in B.1 but only partially implemented (hardcoded bootstrap pools).

---

## Priority 6: Fork-Based Simulation

### Current: Structural validation only (checks `!trades.is_empty()`)
### Target: Anvil fork simulation

Verify the solution would actually execute on-chain:
- Correct output amounts
- No reverts
- Actual gas used (not estimated)

This prevents submitting solutions that look good on paper but would revert, which damages solver reputation with CoW.

### Implementation
1. Run a local Anvil instance forked from Arbitrum
2. Before submitting, simulate the settlement transaction
3. If it reverts, don't submit
4. If gas differs significantly from estimate, adjust score

**Deferred to next sprint** - score normalization is higher leverage right now.

---

## Implementation Order

| # | Task | Files | Effort | Impact |
|---|------|-------|--------|--------|
| 1 | Normalize scores by reference_price | assembler.rs, direct.rs, split.rs, router.rs, graph.rs | 3h | **Critical** - fixes the entire scoring model |
| 2 | Fix UDCP price computation | direct.rs, assembler.rs | 2h | High - prevents rejection |
| 3 | Wire gas oracle into scoring | assembler.rs, gas/mod.rs | 1h | High - accurate net_score |
| 4 | Log strategy comparison per auction | solver/mod.rs | 1h | Medium - debugging |
| 5 | Add more bootstrap pools | pool_indexer.rs, config/ | 2h | Medium - more routing options |
| 6 | Factory pool discovery | pool_indexer.rs, shared/rpc.rs | 4h | High - 5x pool coverage |
| 7 | Anvil fork simulation | simulation.rs | 8h | Medium - deferred |

**Items 1-3 are the sprint. Items 4-7 are next sprint.**

---

## Success Criteria

- [ ] All strategies use `surplus × reference_price(buy_token)` for scoring
- [ ] Score values are in ETH-normalized units (comparable across token pairs)
- [ ] Gas cost subtracted from normalized surplus (not raw surplus)
- [ ] Dashboard shows normalized scores (should see realistic ETH-denominated values)
- [ ] Multiple strategies compete on the same auction (not always split)
- [ ] Shadow competition gap to winner decreases by >50%
