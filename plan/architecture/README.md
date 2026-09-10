# CoW Solver - Architecture Overview

## What Is This?

A competitive solver for CoW Protocol on Arbitrum. It receives batched trade orders, finds the best execution routes through DEX liquidity pools, and submits solutions to earn surplus revenue. Think of it as an algorithmic market maker that competes against 8-12 other solvers every 8 seconds.

## System Diagram

```
  CoW Autopilot (auction source)
         │
         ▼
  CoW Driver (pre-processes auction, can send liquidity)
         │
         │ POST /solve (every 8 seconds)
         ▼
┌─────────────────────────────────────────────────┐
│              OUR SOLVER ENGINE                    │
│                                                   │
│  ┌─────────────────────────────────────────┐     │
│  │  Solving Pipeline (5 phases)            │     │
│  │  Phase 1: CoW matching + Direct routing │     │
│  │  Phase 2: Multi-hop via intermediaries  │     │
│  │  Phase 3: Split routing (if time)       │     │
│  │  Phase 4: Graph search (if time)        │     │
│  │  Phase 5: External aggregators (always) │     │
│  └────────────────────┬────────────────────┘     │
│                       │                           │
│  ┌────────┐  ┌────────┴────────┐  ┌───────────┐ │
│  │ Pool   │  │ Score + UDCP    │  │ External  │ │
│  │ Cache  │  │ Pricing         │  │ APIs      │ │
│  │        │  │                 │  │ 0x,Bebop  │ │
│  │ V2+V3  │  │ normalize()    │  │ Paraswap  │ │
│  │ 1,400  │  │ cap, gas adj   │  │ OKX       │ │
│  │ pools  │  │                 │  │           │ │
│  └───┬────┘  └─────────────────┘  └───────────┘ │
│      │                                           │
│  ┌───┴──────────────────────────────────┐       │
│  │  Data Sources                         │       │
│  │  • WebSocket (real-time events)       │       │
│  │  • The Graph subgraph (V3 ticks)      │       │
│  │  • eth_getLogs polling (fallback)     │       │
│  │  • JIT refresh (per-solution)         │       │
│  └───────────────────────────────────────┘       │
│                                                   │
│  ┌───────────────────────────────────────┐       │
│  │  Monitoring                            │       │
│  │  • Dashboard (live web UI)             │       │
│  │  • Competition tracker (shadow)        │       │
│  │  • Replay DB (auction history)         │       │
│  │  • Telegram alerts                     │       │
│  └───────────────────────────────────────┘       │
└─────────────────────────────────────────────────┘
         │
         │ Response: { solutions: [...] }
         ▼
  CoW Driver validates + submits to chain
```

## Module Map

### Core Solving
| Module | Purpose | Doc |
|--------|---------|-----|
| `solver/` | 5-phase solving engine | [solving-pipeline.md](solving-pipeline.md) |
| `solver/scoring.rs` | Score computation | [scoring.md](scoring.md) |

### Data
| Module | Purpose | Doc |
|--------|---------|-----|
| `pool_indexer.rs` | Pool cache + event polling | [pool-data.md](pool-data.md) |
| `subgraph.rs` | V3 tick data from The Graph | [pool-data.md](pool-data.md) |
| `ws_monitor.rs` | WebSocket real-time events | [pool-data.md](pool-data.md) |
| `pool_discovery.rs` | Discovers pools at startup | [pool-data.md](pool-data.md) |

### External APIs
| Module | Purpose | Doc |
|--------|---------|-----|
| `liquidity/aggregator/` | 0x, Bebop, Paraswap, OKX | [aggregators.md](aggregators.md) |

### Monitoring
| Module | Purpose | Doc |
|--------|---------|-----|
| `routes/dashboard.rs` | Web dashboard | [dashboard.md](dashboard.md) |
| `competition.rs` | Shadow comparison | [competition.md](competition.md) |
| `monitoring/` | Metrics counters | - |
| `replay.rs` | Auction history DB | - |

### Infrastructure
| Module | Purpose |
|--------|---------|
| `routes/solve.rs` | POST /solve handler (entry point) |
| `models/` | Data structures (auction, solution, order, liquidity) |
| `settlement.rs` | GPv2Settlement ABI encoding |
| `simulation.rs` | Structural validation |
| `submission.rs` | Submit/hold policy |
| `triage.rs` | Fast auction classification |
| `freshness.rs` | Data freshness scoring |
| `gas/` | Gas cost estimation |
| `interactions/` | DEX swap calldata encoding |
| `validation/ebbo.rs` | EBBO compliance checking |

## Detailed Docs

- **[Solving Pipeline](solving-pipeline.md)** - How the 5-phase engine works
- **[Pool Data](pool-data.md)** - How we source and refresh liquidity data
- **[Scoring](scoring.md)** - Score computation, caps, and revenue model
- **[Competition](competition.md)** - Shadow comparison against real winners
- **[Aggregators](aggregators.md)** - External price sources overview
- **[Dashboard](dashboard.md)** - Command center web UI explained

## External Providers

Each external service has its own doc: **[providers/](providers/README.md)**

| Provider | What | Page |
|----------|------|------|
| Alchemy | RPC + WebSocket | [providers/alchemy.md](providers/alchemy.md) |
| The Graph | V3 tick data | [providers/thegraph.md](providers/thegraph.md) |
| CoW Protocol | Auction driver | [providers/cow-protocol.md](providers/cow-protocol.md) |
| 0x | DEX aggregator | [providers/zerox.md](providers/zerox.md) |
| Bebop | Private MM RFQ | [providers/bebop.md](providers/bebop.md) |
| Paraswap | Price oracle | [providers/paraswap.md](providers/paraswap.md) |
| OKX | DEX aggregator | [providers/okx.md](providers/okx.md) |
| Liquorice | PMM aggregator | [providers/liquorice.md](providers/liquorice.md) |
