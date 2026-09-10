# Scoring - How Solutions Are Ranked and Revenue Generated

**What this is:** How the CoW Protocol decides which solver wins an auction, and how much money we make.

## The CoW Auction

Every ~8 seconds, CoW batches all pending orders and runs an auction. Solvers compete by submitting solutions. The highest-scoring valid solution wins and gets settled on-chain.

## Score Formula

```
score = Σ (surplus_per_order × reference_price) - gas_cost
```

**Surplus:** How much better we fill an order vs its limit price.
- User wants to sell 1 ETH for at least 2,700 USDC
- We find a route that gives them 2,710 USDC
- Surplus = 10 USDC

**Reference price:** Converts surplus to a common unit (ETH) for comparison across token pairs.

**Gas cost:** Estimated cost to execute the solution on-chain.

## Our Current Score Behavior

Our scores are currently **near-constant at ~5.5M gwei** because:
1. Same limit orders appear in every auction batch (they persist for hours/days)
2. Our pool reserves change slowly (10s-30s refresh cycle)
3. Same orders + same pools = same surplus = same score

**Per-order cap:** We cap each order's normalized surplus at 5e15 wei. This prevents stale pool data from producing astronomical scores. The cap will be removed once we get fresh V3 data from the driver.

**Global cap:** Total solution score capped at 1e16 wei as a safety net.

## Revenue Model

When a solver wins an auction, it earns approximately:
```
payment = min(protocol_fee, surplus_captured)
```

- Protocol fee = ~2 basis points (0.02%) of trade volume
- For a $10,000 trade: fee = $2
- For a $100 trade: fee = $0.02 (often unprofitable after gas)

**CIP-74 insight:** Small orders are structurally unprofitable. We prioritize large orders.

## Competition Reality

From our shadow comparison data (1,787 auctions):

| Scenario | Our Score | Winner | Gap |
|----------|----------|--------|-----|
| Comparable auction | 5.5M gwei | ~1M gwei | +450% (inflated) |
| Big auction | 5.5M gwei | 128M gwei | -95.7% (outclassed) |
| Closest miss | 5.5M gwei | 5.5M gwei | -0.1% (nearly won) |

The constant score means we overshoot small auctions and get outclassed by big ones. Variable scores (from fresh V3 data) will fix both.

## Key Files

| File | Role |
|------|------|
| `solver/scoring.rs` | Score computation per order and per solution |
| `solver/assembler.rs` | `normalize_surplus()` - per-order normalization with cap |
| `solver/mod.rs` | `finalize_solutions()` - global score cap |
| `competition.rs` | Shadow comparison against real winners |

## Revenue Projections

| Scenario | Win Rate | Monthly Revenue |
|----------|----------|----------------|
| Current (V2 only, stale data) | 0% | $0 |
| With V3 tick data | 3-8% | $500-2,000 |
| + RFQ aggregators | 5-12% | $1,000-4,000 |
| + Liquorice PMM | 8-15% | $2,000-8,000 |

These are estimates based on settlement rates and per-win averages from our shadow data.
