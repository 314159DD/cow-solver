# CoW Solver - Product Vision

**Updated:** 2026-03-31

## What

An optimization algorithm (solver) that competes in CoW Protocol batch auctions on Arbitrum to fill DeFi trade orders. Earns revenue by finding better execution paths than competing solvers - pure algorithmic competition, no users, no marketing.

## Why

- $87B trading volume on CoW Protocol in 2025 (2x YoY)
- Pure coding skill play - revenue scales with routing quality
- Batch auctions level the playing field vs HFT (no speed arms race)
- Failure mode is "earn $0" not "lose everything"
- Infrastructure cost: ~$0-50/month (Alchemy free tier + VPS)

## Current State

**Shadow competition on Arbitrum.** Receiving ~470 auctions/hour, submitting solutions, comparing against real winners. Not yet registered for live competition (requires KYC + 1 ETH bond).

### What Works
- 5-phase solving pipeline (CoW matching → direct → multi-hop → graph → aggregators)
- 1,395 pools indexed (V2 + V3), V3 tick data from The Graph subgraph
- 4 external price sources (0x, Bebop, Paraswap, OKX)
- Real-time pool updates via WebSocket + eth_getLogs events
- Dashboard with honest competition data and earnings projections

### Bottleneck
V3 concentrated liquidity data. Real winners route through V3 (80% of volume). We have V3 math and tick data from The Graph, but waiting for the CoW driver to send fresh per-block V3 data (a config change on their side).

## Revenue Projections

| Scenario | Monthly Revenue | Requires |
|----------|----------------|----------|
| Current (V2 only) | $0 | Nothing - this is where we are |
| With V3 data from driver | $500-2,000 | CoW team enables driver config |
| + Liquorice PMM | $2,000-8,000 | Liquorice API key |
| Mature (6+ months) | $5,000-20,000 | Continuous routing optimization |

## Tech Stack

- **Language:** Rust (2024 edition)
- **Runtime:** Tokio async + Axum HTTP server
- **RPC:** Alchemy (free tier, 30M CU/month)
- **V3 Data:** The Graph subgraph (free tier)
- **Aggregators:** 0x, Bebop, Paraswap, OKX (all free tier)
- **Deployment:** Docker on VPS, Arbitrum One

## Go-Live Path

1. CoW team enables V3 driver liquidity (waiting)
2. Achieve >5% genuine win rate in shadow
3. KYC + onboarding call with CoW team
4. Bond ~1 ETH
5. Enable live competition
