# Sprint: Competitive Routing

**Created:** 2026-03-30
**Goal:** Win real auctions by routing through V3 pools with fresh data
**Cost target:** $0/month (Alchemy free tier, 30M CU)

## Current State

- Score: constant ~1.2M gwei (same 2 stale limit orders through V2 pools)
- Win rate: 0% genuine wins (all "wins" are inflated constant score > small auctions)
- Pool data: V2 only, 30s-stale, from background indexer
- V3 support: 85% implemented (math, routing, encoding done - indexing missing)

## Alchemy Cost Plan

| Component | Interval | CU/month | Purpose |
|-----------|----------|----------|---------|
| Block number | 60s | 1.1M | Chain head tracking |
| V2 hot pools (10) | 90s | 7.5M | Top V2 pools we route through |
| V3 hot pools (15) | 90s | 11.2M | Top V3 pools (where the volume is) |
| JIT solution pools (5) | per settle | 4.7M | Fresh reserves before submission |
| **Total** | | **24.5M** | **82% of free tier** |

Drop the all-pool slow refresh entirely. Only refresh pools we actually route through.

---

## Task 1: V3 Pool State in Indexer

**What:** Add sqrtPriceX96, tick, liquidity to PoolSnapshot. Fetch via slot0() call.
**Why:** V3 pools need more state than V2 (which only needs reserve0/reserve1).
**Files:**
- `pool_indexer.rs` - extend PoolSnapshot, add V3 refresh function
- `pool_indexer.rs:cached_as_liquidity()` - export V3 pools as ConcentratedLiquidity

**Cost:** Included in hot pool refresh (15 V3 pools × 90s = 11.2M CU/month)

**Implementation:**
```rust
pub struct PoolSnapshot {
    // ... existing V2 fields ...
    pub pool_type: PoolType,  // V2 or V3
    // V3-specific (None for V2 pools)
    pub sqrt_price_x96: Option<u128>,
    pub tick: Option<i32>,
    pub v3_liquidity: Option<u128>,
    pub fee_tier: Option<u32>,
}
```

Refresh V3 pools via `slot0()` selector: `0x3850c7bd`
Response: (sqrtPriceX96, tick, observationIndex, ...) - parse first 2 words.

---

## Task 2: Variable Order Selection

**What:** Stop routing the same 2 stale limit orders. Pick orders that are most likely to execute.
**Why:** Our constant score comes from always selecting the same orders.
**Files:**
- `solver/assembler.rs` - smarter order scoring before routing
- `solver/mod.rs` - order pre-filtering

**Cost:** $0 (algorithm change, no RPC)

**Implementation:**
1. Score orders by "actionability" before routing:
   - Market orders: priority 0 (fresh, just submitted)
   - Limit orders expiring < 1 hour: priority 1 (active traders)
   - Limit orders with small sell_amount: priority 2 (easy to fill)
   - Limit orders expiring > 24h: priority 3 (stale, likely no longer wanted)
2. Route top 20 by priority (not all 970+)
3. Select best 2 routes by surplus from those 20

---

## Task 3: Hot Pool Refresh Architecture

**What:** Replace slow all-pool refresh with targeted hot-pool-only refresh.
**Why:** We refresh 1395 pools but route through 5. Waste of CU.
**Files:**
- `pool_indexer.rs` - restructure refresh cycles
- `routes/solve.rs` - mark solution pools as hot after each auction

**Cost:** Saves 50M+ CU/month vs current approach

**Implementation:**
- Remove SLOW_REFRESH_SECS cycle entirely (currently refreshes ALL 1395 pools)
- Keep hot refresh cycle at 90s (not 30s - saves 2x CU)
- Hot pool set: top 10 V2 + top 15 V3 pools (auto-detected from recent solutions)
- JIT refresh: 5 solution pools right before submission (already implemented)
- One-time startup refresh: all discovered pools

---

## Task 4: RFQ Integration (0x API)

**What:** Query 0x API for quotes on large orders. Free tier = 1M calls/month.
**Why:** Professional market makers often beat DEX pool prices for large orders.
**Files:**
- `liquidity/aggregator/mod.rs` - already has 0x client scaffolding
- `solver/mod.rs` Phase 5 - aggregator/RFQ query (already scaffolded)

**Cost:** $0 Alchemy CU (uses 0x's own API). Free tier: 1M calls/month.

**Implementation:**
1. Enable Phase 5 (currently runs but finds no aggregator sources)
2. Configure 0x API key in .env
3. For orders > 0.1 ETH value: query 0x for quote
4. Compare 0x quote vs our internal routing - use better one
5. If 0x beats us: use their calldata as a Custom interaction

---

## Priority Order

1. **Task 3: Hot pool architecture** (2 hours) - saves CU, enables free tier switch
2. **Task 1: V3 indexer** (3-4 hours) - unlocks V3 routing with real data
3. **Task 2: Order selection** (1-2 hours) - varies scores, finds real opportunities
4. **Task 4: RFQ** (2-3 hours) - competitive edge on large orders

## Expected Outcome

After all 4 tasks:
- Score varies per auction (not constant)
- Routes through V3 pools (where 80%+ of volume is)
- Focuses on actionable orders (market orders, soon-expiring limits)
- Queries market makers for large order quotes
- Cost: $0/month (Alchemy free tier)
- Projected win rate: 5-15% on settled auctions (vs 0% today)
