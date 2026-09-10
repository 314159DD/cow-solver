# Pool Data - How We Source and Refresh Liquidity

**What this is:** Everything about how we know what's available to trade against. Pool data = reserves, prices, and liquidity positions on DEXes.

## The Problem

Our solver needs to know the current state of DEX pools (how much token A and B are in each pool) to calculate swap outputs accurately. Stale data = wrong prices = solutions that revert on-chain = 0% win rate.

## Data Sources (3 layers)

### Layer 1: CoW Driver (freshest - per block)
The CoW driver CAN send us pool data in every auction payload as `liquidity: [...]`. Currently configured to send `liquidity: []` (empty). We've asked the CoW team to enable `[[liquidity.uniswap-v3]]` and `[[liquidity.balancer-v2]]` in the driver config. When enabled, this gives us per-block-accurate V3 tick data with zero effort on our side.

**Status:** Waiting on CoW team response.

### Layer 2: The Graph Subgraph (V3 tick data - every 30s)
We query the official Uniswap V3 Arbitrum subgraph for:
- Top 100 pools by TVL
- Full tick data (liquidityNet per initialized tick)
- sqrtPriceX96, tick, liquidity per pool

This gives our V3 routing engine real tick-traversal data instead of the single-tick approximation (which is 20-50% off for large swaps).

**Refresh:** Every 30 seconds. Full tick refresh every 5 minutes.
**Cost:** Free tier (The Graph Studio API key).
**File:** `subgraph.rs`

### Layer 3: On-Chain Events (real-time - sub-second)
Two mechanisms run in parallel:

**WebSocket Monitor** (`ws_monitor.rs`):
- Connects to Alchemy WSS endpoint
- Subscribes to V2 Sync + V3 Swap/Mint/Burn events
- Updates pool cache in real-time as trades happen on-chain
- Cost: $0 (WebSocket subscriptions free on Alchemy)

**eth_getLogs Polling** (`pool_indexer.rs`):
- Fallback: polls every 10 seconds for recent events
- Catches events the WebSocket might miss
- Also handles the startup bootstrap

### Layer 4: JIT Refresh (per-auction - 5 solution pools)
Right before submitting a solution, we fetch fresh reserves for the specific 5-10 pools we're routing through. One batched RPC call.

**File:** `pool_indexer.rs` (`jit_refresh()`)

## Pool Types

| Type | Count | Data Source | Freshness |
|------|-------|-------------|-----------|
| Uniswap V2 | ~1,000 | Pool indexer + events | Real-time (WS) |
| Uniswap V3 | ~379 | Subgraph + events | 30s (subgraph) + real-time (WS) |
| SushiSwap | included in V2 | Same as V2 | Same |
| Camelot V2/V3 | included | Same | Same |
| Balancer V2 | pending | Needs driver config | Not yet |
| Curve | registered | RPC-based | Startup only |

## Key Files

| File | Role |
|------|------|
| `pool_indexer.rs` | Central pool cache, event polling, JIT refresh |
| `subgraph.rs` | The Graph V3 tick data fetcher |
| `ws_monitor.rs` | WebSocket real-time event subscriptions |
| `pool_discovery.rs` | Discovers pools from factory contracts (startup) |
| `models/liquidity.rs` | Pool data structures (ConstantProduct, ConcentratedLiquidity) |

## Pool Cache Architecture

```
                ┌─────────────┐
                │  Subgraph   │ (V3 ticks every 30s)
                └──────┬──────┘
                       │
┌───────────┐    ┌─────▼──────┐    ┌──────────────┐
│ WebSocket │───▶│ Pool Cache │◀───│ eth_getLogs   │
│ (real-time)│   │ (DashMap)  │    │ (10s fallback)│
└───────────┘    └──────┬──────┘    └──────────────┘
                       │
              ┌────────▼────────┐
              │ cached_as_      │
              │ liquidity()     │
              │ (exports to     │
              │  solver)        │
              └─────────────────┘
```

## Cost

| Component | CU/month | Cost |
|-----------|----------|------|
| Event polling (10s) | ~19M | Free tier |
| Subgraph queries | ~0.5M | Free tier |
| JIT refresh | ~5M | Free tier |
| WebSocket | 0 | Free |
| **Total** | ~25M | **$0/month** (30M free tier) |
