# Research Handoff: Improving CoW Solver Win Rate

**Date:** 2026-03-31
**For:** Researcher
**From:** Engineering team
**Priority:** High - this directly determines our revenue

---

## What We Built

A CoW Protocol solver on Arbitrum that competes in batch auctions to earn surplus revenue from DEX trades. It receives ~470 auctions/hour, finds swap routes through on-chain liquidity pools, and submits solutions to the CoW driver.

**Dashboard:** [our solver dashboard URL]
**Code:** `/opt/cow-solver/` on VPS

---

## Current Performance (Honest Numbers)

| Metric | Value | Target |
|--------|-------|--------|
| Auctions received | ~470/hour | Good |
| Solutions submitted | 100% | Good |
| Score range | ~5.5M gwei (constant) | Should vary 100K-50M per auction |
| Genuine win rate | **0%** | 5-15% |
| Closest miss | **-0.1%** (nearly won) | Shows logic works |
| Biggest loss | -95% (big auctions) | Need V3 data to compete |
| Revenue (if live today) | **$0/month** | $500-5000/month goal |

---

## Why We're Losing

### Problem 1: No Fresh V3 Pool Data (80% of the problem)

Uniswap V3 concentrated liquidity handles ~80% of Arbitrum trading volume. V3 pools concentrate money in specific price ranges → tighter spreads → more surplus to capture.

**We have:** 379 V3 pools discovered, V3 math implemented (full tick-traversal), routing code ready.

**We're missing:** Tick-level data (which price ranges have liquidity and how much). Without it, our V3 swap price estimates are 20-50% off for large trades. This is why competitors find 2-10x more surplus than us.

**The CoW driver CAN send us this data** (we asked, they said they'll configure it). But we also need a fallback/independent source.

### Problem 2: Stale Pool Reserves

Our pool data updates every 10 seconds via on-chain event monitoring. Competitors likely have sub-second data. On fast-moving markets, 10-second-old reserves mean our swap output calculations are slightly wrong → slightly less surplus → we lose to solvers with fresher data.

### Problem 3: Limited to V2-Style Routing

We find routes through Uniswap V2, SushiSwap, and other constant-product pools well. But the winning solvers (helixbox-solve, rizzolver, zeroex-solve) likely also use:
- V3 concentrated liquidity with full tick data
- Private market maker quotes (RFQ)
- Balancer V2 weighted/stable pools
- Curve StableSwap pools
- Cross-DEX arbitrage

---

## What We Need You To Research

### 1. V3 Tick Data Sources for Arbitrum

How do independent solvers get Uniswap V3 tick-level data on Arbitrum? Specifically we need for each V3 pool:
- `sqrtPriceX96` (current price)
- `tick` (current tick index)
- `liquidity` (active liquidity)
- `liquidityNet` for each initialized tick (how much liquidity enters/exits at each tick boundary)

**Research questions:**
- Is The Graph's Uniswap V3 subgraph available on Arbitrum? What's the latency? Free tier limits?
- Are there other subgraph providers (Goldsky, Satsuma, Alchemy Subgraphs)?
- Can we get tick data directly from RPC calls? What's the cost per pool?
  - `tickBitmap(int16)` returns a 256-bit bitmap of initialized ticks
  - `ticks(int24)` returns liquidityNet for a specific tick
  - Fetching all ticks for one pool = ~50-200 RPC calls
- Are there any V3 data APIs (not subgraphs) that provide this? Free or paid?
- How do projects like 1inch Pathfinder, Paraswap, or Odos source their V3 data?

### 2. Real-Time Pool State Monitoring

We currently use `eth_getLogs` to catch Uniswap V2 `Sync` and V3 `Swap` events every 10 seconds. This gives us ~10-second freshness. How can we get faster?

**Research questions:**
- Alchemy WebSocket subscriptions (`eth_subscribe`) - can we subscribe to specific pool addresses for real-time events? What's the cost?
- Are there any Arbitrum-specific real-time data feeds? (Chainlink, Pyth, etc.)
- How does Alchemy's "Subscription API" compare to polling `eth_getLogs`?
- Are there services that provide real-time decoded DEX events? (e.g., Defined.fi, Dune, Nansen)
- What latency do other solver teams achieve? (Any public info from solver forums, Discord, blog posts?)

### 3. Market Maker / RFQ Integration

Private market makers often provide better prices than on-chain pools for large orders. We have 0x API integration (v2, Standard plan) but most orders return "no liquidity."

**Research questions:**
- What RFQ (Request for Quote) providers work on Arbitrum?
  - 0x (we have this - works for mainstream pairs only)
  - Hashflow
  - 1inch Fusion
  - Bebop
  - Any others?
- Are there private market maker APIs that CoW solvers specifically use?
- How does the CoW Protocol's own "coincidence of wants" matching work with external MM liquidity?
- Are there solver-specific RFQ networks or aggregators?

### 4. Existing Solver Implementations / Reference Code

**Research questions:**
- What open-source CoW solver implementations exist?
  - `cowprotocol/services` (the reference implementation) - how does it source liquidity?
  - Any community solvers on GitHub?
- Are there blog posts or documentation from existing solvers about their architecture?
  - helixbox-solve, rizzolver, zeroex-solve - any public info about how they work?
- CoW Protocol Discord or forum - are there solver-specific channels with technical discussions?
- Any academic papers or technical reports about batch auction solving strategies?

### 5. Alternative Approaches

**Research questions:**
- Could we use a DEX aggregator (1inch, Paraswap, Odos) as our routing engine instead of building our own? What are the tradeoffs?
- Are there "solver-as-a-service" platforms that provide infrastructure?
- Could we run a lightweight Arbitrum full node locally for zero-latency pool state? What's the hardware requirement and cost?
- Are there MEV-related tools (Flashbots, MEV-Share) that could give us an edge in CoW auctions?

---

## Technical Context (For Understanding Our Codebase)

- **Language:** Rust
- **Pool indexer:** Background task polls Alchemy RPC, caches reserves for 1,395 pools (1,000 V2 + 379 V3)
- **V3 math:** Full implementation in `liquidity/uniswap_v3.rs` (1,600 lines) - tick traversal, sqrtPrice math, all 4 fee tiers
- **Routing:** Direct single-pool, multi-hop (WETH/USDC intermediary), graph-based (Yen's K-shortest paths)
- **Budget:** Alchemy free tier (30M compute units/month). Currently using ~25M CU/month.
- **The driver sends us `liquidity: []`** - zero pools. We source everything ourselves.

## Key Competitors (From Our Shadow Data)

| Solver | Wins (out of 30 settled) | Notes |
|--------|--------------------------|-------|
| helixbox-solve | 11 | Most frequent winner |
| rizzolver | 7 | Wins big auctions |
| zeroex-solve | 4 | Our closest competitor (-19.9% gap) |
| extquasimodo-solve | 2 | |
| sector-solve | 2 | |

---

## Deliverable

A ranked list of actionable recommendations with:
1. What the solution is
2. Estimated implementation effort
3. Cost (if any recurring fees)
4. Expected impact on win rate
5. Links to relevant tools, APIs, documentation

Focus on what gets us from 0% to 5%+ genuine win rate fastest and cheapest.
