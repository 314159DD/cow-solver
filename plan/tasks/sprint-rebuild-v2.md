# Sprint: The Rebuild - 0x-Powered Solver v2

**Created:** 2026-04-03
**Priority:** CRITICAL - replaces all previous scoring/routing work
**Context:** After 9 failed scoring attempts and weeks of debugging AMM math, research revealed that winning solvers (OKX, BitGet) don't compute their own AMM math at all. They call external aggregator APIs and format the response as CoW solutions. We're doing the same.

## Why We're Rebuilding

Our custom AMM math produces outputs 100-1000x above reality. We've tried 9 fixes. The research report revealed:

1. **Clearing prices are execution rates, not reference prices** (we kept getting this wrong)
2. **The driver completely recomputes our score** from our clearing prices (our internal score is irrelevant)
3. **Winning solvers use aggregator APIs** (0x, OKX, BitGet) - not custom pool math
4. **The cowprotocol/services repo is the reference** - we should have forked it, not built from scratch

## The New Architecture

```
CoW Driver → POST /solve → Our Solver
                              │
                    ┌─────────▼──────────┐
                    │ Parse auction       │
                    │ Select top orders   │
                    └─────────┬──────────┘
                              │
                    ┌─────────▼──────────┐
                    │ For each order:     │
                    │  Query 0x Swap API  │  ← NEW: 0x does ALL routing
                    │  Get: route, output │
                    │  Get: swap calldata │
                    └─────────┬──────────┘
                              │
                    ┌─────────▼──────────┐
                    │ Build solution:     │
                    │  prices = execution │  ← Clearing prices from 0x output
                    │  trades = filled    │
                    │  interactions = 0x  │  ← Swap calldata from 0x
                    └─────────┬──────────┘
                              │
                    ┌─────────▼──────────┐
                    │ Simulate via        │
                    │ Alchemy eth_call    │  ← Validate before submitting
                    └─────────┬──────────┘
                              │
                    Return SolveResponse
```

**What 0x handles for us:** Pool discovery, reserve management, V2/V3 math, split routing, RFQ quotes from private MMs, slippage estimation, optimal route finding across 150+ AMMs.

**What we handle:** Order selection, solution formatting, CoW-specific scoring, submission policy, dashboard/monitoring.

---

## Phase 1: 0x Integration (THE CORE - 4-6 hours)

### 1.1 New module: `solver-engine/src/aggregator/zerox.rs`

```rust
/// Query 0x Swap API for a single order.
/// Returns: (buy_amount, swap_calldata, gas_estimate, route_sources)
pub async fn get_quote(
    sell_token: &str,
    buy_token: &str,
    sell_amount: u128,
    chain_id: u64,
) -> Result<ZeroXQuote, String>
```

**API call:**
```
GET https://api.0x.org/swap/allowance-holder/quote
  ?sellToken={sell_token}
  &buyToken={buy_token}
  &sellAmount={sell_amount}
  &chainId=42161
  &slippageBps=50
Headers:
  0x-api-key: {ZEROX_API_KEY}
  0x-version: v2
```

**Response contains:**
- `buyAmount` - exact output amount (this is our execution reality)
- `transaction.data` - encoded swap calldata (this becomes our interaction)
- `transaction.to` - target contract
- `gas` - gas estimate
- `route.fills[]` - which pools/sources were used

### 1.2 New solve pipeline: `solver-engine/src/solver/zerox_solver.rs`

```rust
pub async fn solve_via_zerox(auction: &AuctionInstance) -> Option<Solution> {
    // 1. Select top N orders by value (reference_price × sell_amount)
    // 2. For each order, query 0x in parallel (tokio::spawn)
    // 3. Filter: only include orders where 0x output > limit
    // 4. Build clearing prices from execution amounts:
    //    prices[sell_token] = buy_amount (from 0x)
    //    prices[buy_token] = sell_amount
    // 5. Build trades + custom interactions (swap calldata from 0x)
    // 6. Return assembled solution
}
```

### 1.3 Wire into solver orchestrator

In `solver/mod.rs`, add Phase 5 (or replace existing aggregator phase):
```rust
// Phase 5: 0x Aggregator (the real routing engine)
if let Some(zerox_sol) = zerox_solver::solve_via_zerox(&auction).await {
    all_candidates.push(zerox_sol);
}
```

### 1.4 Environment variables

```
ZEROX_API_KEY=<from dashboard.0x.org>
ZEROX_ENABLED=true
ZEROX_MAX_ORDERS=5          # max orders to quote per auction
ZEROX_SLIPPAGE_BPS=50       # 0.5% slippage tolerance
ZEROX_TIMEOUT_MS=3000       # per-quote timeout
```

---

## Phase 2: Fix Clearing Prices for ALL Strategies (2 hours)

Even for non-0x strategies (CoW matching, direct), clearing prices must be execution rates:

### 2.1 Clearing prices = execution amounts

For every trade in every solution:
```rust
// prices[sell_token] = total_buy_output  (what you get)
// prices[buy_token]  = total_sell_input   (what you pay)
```

This is what the original UDCP computed. The shared-token problem is solved by:
- Single-pair solutions: no conflict possible
- Multi-pair: normalize via the first pair's shared token price

### 2.2 DO NOT use reference_prices as clearing prices (EVER)

From the research: "They are NOT reference prices from the auction."
Reference prices are ONLY for the final ETH conversion in scoring.

### 2.3 The driver recomputes our score - our internal score only matters for candidate ranking

So internal scoring can stay approximate. What matters is that our clearing prices
accurately reflect execution reality. The driver will compute the correct score from them.

---

## Phase 3: Replace Simulation with Alchemy (1-2 hours)

### 3.1 Use `alchemy_simulateAssetChanges`

We already pay Alchemy. Their simulation API costs ~100-400 CU per call (vs thousands for getReserves batches).

```rust
/// Simulate a settlement via Alchemy's simulation API
pub async fn simulate_settlement_alchemy(
    calldata: &str,
    rpc_url: &str,
) -> (bool, u64, Option<String>) {
    // POST to Alchemy RPC with method "alchemy_simulateAssetChanges"
    // Returns: asset changes, gas used, success/revert
}
```

### 3.2 Only simulate solutions with score > 0

Don't waste CUs simulating empty or obviously bad solutions.

---

## Phase 4: Reduce RPC Costs (1-2 hours)

### 4.1 Kill pool indexer background refresh

With 0x handling routing, we don't need 1395 pools refreshed every 30s-5min.
Keep pool indexer for graph topology only - refresh once on startup and every 30 min.

### 4.2 Use DexScreener for pool discovery instead of RPC

```
GET https://api.dexscreener.com/latest/dex/tokens/{tokenAddress}
```
Free, no API key, returns pool addresses with reserves.
Replace the RPC-based factory enumeration entirely.

### 4.3 Reduce gas oracle refresh

From every 60s to every 300s. Gas prices don't change that fast on Arbitrum.

**Target:** <5M Alchemy CU/month (down from 22M in 3 days)

---

## Phase 5: Keep Existing Infrastructure (0 hours - already built)

These systems are solid and stay as-is:

- ✅ Dashboard + Score Anatomy + Auto-diagnosis
- ✅ Competition tracker (winner comparison)
- ✅ Replay DB (auction storage)
- ✅ Audit CLI
- ✅ Telegram alerts
- ✅ Monitoring + Prometheus metrics
- ✅ Settlement ABI encoding (for simulation)
- ✅ Pool discovery (for graph topology, not routing)

---

## Phase 6: Pyth Price Streaming (2-4 hours, Phase 2 from report)

### 6.1 Add Pyth SSE client

```rust
/// Stream real-time prices from Pyth Network
/// GET https://hermes.pyth.network/v2/updates/price/stream?ids[]={priceId}
pub async fn run_pyth_stream() {
    // Subscribe to WETH, USDC, USDT, WBTC, ARB price feeds
    // Update global price cache on each update
    // Use for sanity checks: if 0x output differs >5% from Pyth price, flag it
}
```

Free, no API key, sub-second updates.

---

## Implementation Order

| Phase | Effort | Impact | RPC Cost Impact |
|-------|--------|--------|-----------------|
| Phase 1 (0x integration) | 4-6h | **CRITICAL** - first real wins | Minimal (0x API, not RPC) |
| Phase 4 (reduce RPC costs) | 1-2h | **HIGH** - stops $70/3-day bleeding | Massive reduction |
| Phase 2 (fix clearing prices) | 2h | HIGH - all strategies honest | None |
| Phase 3 (Alchemy simulation) | 1-2h | HIGH - validate before submit | Slight increase (100-400 CU/sim) |
| Phase 6 (Pyth streaming) | 2-4h | MEDIUM - better price reference | None |

**Total: 10-16 hours to a fundamentally different solver.**

---

## What This Gets Us

| Metric | Current | After Rebuild |
|--------|---------|---------------|
| AMM math | Custom, broken | 0x handles it (150+ AMMs, proven) |
| Routing | Single pool, stale reserves | 0x split routing + RFQ from MMs |
| Scoring | 1000x inflated | Execution-rate clearing prices (honest) |
| Simulation | Broken settlement encoding | Alchemy API (actual on-chain validation) |
| RPC cost | $70/3 days | <$5/month |
| Win rate | 0% | 1-5% (realistic target with aggregator) |
| Pool math bugs | 9 failed fixes | 0 (0x handles all math) |

---

## What We're NOT Doing

- NOT forking cowprotocol/services (40-80 hours, overkill when 0x works)
- NOT running our own Nitro node (Phase 3 from report, later optimization)
- NOT building Anvil sidecar (later, when we need sub-ms simulation)
- NOT integrating Bebop/Hashflow directly (0x aggregates their liquidity)

---

## Files to Create/Modify

| File | Action |
|------|--------|
| `solver-engine/src/aggregator/zerox.rs` | **NEW** - 0x Swap API client |
| `solver-engine/src/solver/zerox_solver.rs` | **NEW** - 0x-based solve pipeline |
| `solver-engine/src/solver/mod.rs` | Add 0x phase to orchestrator |
| `solver-engine/src/pool_indexer.rs` | Reduce refresh to 30-min |
| `solver-engine/src/gas/oracle.rs` | Reduce refresh to 300s |
| `solver-engine/src/main.rs` | Add Pyth stream task |
| `solver-engine/src/aggregator/mod.rs` | **NEW** - aggregator module registry |
| `.env` | Add ZEROX_API_KEY |

---

## The Mental Model Shift

**Before:** We are an AMM math engine that competes on routing quality.
**After:** We are a solution FORMATTER that leverages 0x's routing and competes on order selection, CoW matching, and submission strategy.

The winning insight from the research: OKX and BitGet solvers in the CoW repo do exactly this - call an external API, format the response. They win auctions. We will too.
