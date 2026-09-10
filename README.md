# CoW Solver

A Rust solver competing in [CoW Protocol](https://cow.fi) batch auctions on Arbitrum. Receives roughly 470 auctions per hour, routes trades through on-chain DEX pools and off-chain market makers, and submits solutions to earn surplus revenue.

[![Tests](https://github.com/314159DD/cow-solver/actions/workflows/test.yml/badge.svg)](https://github.com/314159DD/cow-solver/actions/workflows/test.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white)

**Status:** Shadow competition on Arbitrum, not yet registered for live settlement.
**Why it exists:** a solo project to learn the CoW solver protocol end to end and to measure how far a single-person solver gets against funded teams. The competition numbers below are the honest answer: close, not winning. The postmortems in `plan/` are the part worth reading.
**Network:** Arbitrum One (chain 42161)

---

## How it works

Every 8 seconds the CoW driver sends a batch of trade orders. The solver finds the most profitable execution routes and submits a solution; the highest-scoring solution across all competing solvers wins and settles on-chain.

```
CoW Driver -> POST /solve -> 5-Phase Solver -> Best Solution -> Driver validates -> On-chain settlement
```

**Solving phases:**
1. **CoW Matching** - match opposite-direction orders directly (zero gas, pure surplus)
2. **Direct Routing** - best single pool per order (Uniswap V2 + V3 math)
3. **Multi-hop** - route through intermediary tokens (WETH, USDC)
4. **Graph Search** - Yen's K-shortest paths across all indexed pools
5. **Aggregators** - query 0x, Bebop, Paraswap and OKX for market maker quotes, used whenever they beat internal routing

Phases run under a shared time budget; later phases are skipped once the deadline is close so the solver always returns whatever it has found so far.

## Data sources

| Source | What | Freshness | Cost |
|--------|------|-----------|------|
| Pool indexer | 1,395 Uniswap V2 + V3 pools from on-chain events | Real-time (WebSocket) | $0 |
| The Graph | V3 tick data for the top 100 pools | 30 seconds | Free tier |
| 0x API | Market maker quotes (v2 allowance-holder) | Per-request | Free tier |
| Bebop | Private market maker quotes (PMMv3) | Per-request | Free |
| Paraswap | Price oracle across 21 DEX sources | Per-request | Free |
| OKX DEX | Aggregator quotes (100+ sources) | Per-request | Free with registration |

Additional DEX integrations (Balancer V2, Curve, Camelot V2/V3, Trader Joe, GMX V2, Wombat, DODO) and aggregators (Odos, KyberSwap, OpenOcean, 1inch) exist in the codebase but were, as of the last recorded status update, either not yet wired into the live pipeline or gated on external rate limits. See [`plan/architecture/aggregators.md`](plan/architecture/aggregators.md) and [`plan/STATUS.md`](plan/STATUS.md) for the current picture.

## Competition performance

From shadow data (roughly 1,800 auctions analyzed):
- **Closest miss:** -0.1% (nearly identical to the winning score)
- **Typical range:** +20% to -60% versus winners
- **Bottleneck:** V3 concentrated-liquidity data - solvers with full V3 tick data find 2-10x more surplus

Top competitors observed in shadow data: `helixbox-solve` (37% of wins), `rizzolver` (23%), `zeroex-solve` (13%, closest competitor at a -19.9% average gap).

A documented production incident is tracked in [`plan/STATUS.md`](plan/STATUS.md): a v2.2 change made the aggregator phase run sequentially at a 5-second timeout per provider, which occasionally exceeded the outer request budget and caused an otherwise-valid Phase 1 solution to be discarded. The v2.3 fix gives the aggregator phase its own sub-timeout so earlier-phase solutions are always preserved.

---

## Quick start

```bash
# Configure
cp .env.example .env
# Set RPC_URL to an Alchemy (or Infura) Arbitrum endpoint

# Docker (recommended)
docker compose up -d
curl http://localhost:8000/health

# Or build from source
cargo build --release -p solver-engine
cargo run -p solver-engine
```

## Environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `RPC_URL` | Yes | Ethereum JSON-RPC endpoint (Arbitrum by default) |
| `CHAIN_ID` | No (default 1, use 42161 for Arbitrum) | Target chain |
| `SOLVER_PORT` | No (default 8000) | HTTP port for the solver engine |
| `THEGRAPH_API_KEY` | No | The Graph API key for V3 tick data |
| `ZEROX_API_KEY` | No | 0x API v2 key |
| `OKX_API_KEY` | No | OKX DEX aggregator key |
| `DASHBOARD_TOKEN` | No | Dashboard access token |
| `TELEGRAM_BOT_TOKEN` / `TELEGRAM_CHAT_ID` | No | Telegram alerts (silently skipped if unset) |

Full list with comments in [`.env.example`](.env.example).

## Testing

487 test functions across the workspace (473 `#[test]`, 14 `#[tokio::test]`), covering pool math, routing, scoring, the deterministic-output guarantee, and auction deserialization:

```bash
cargo test --workspace
```

There is also an integration suite (`tests/determinism.rs`) that runs the same auction ten times and checks the output is byte-identical, which matters because CoW Protocol scores solutions that must be reproducible.

CI (`.github/workflows/test.yml`) runs the full workspace test suite on every push and pull request to `main`.

## Project structure

```
solver-engine/src/
├── main.rs                      # Axum HTTP server, background task spawning
├── routes/
│   ├── solve.rs                 # POST /solve - main auction handler
│   └── dashboard.rs             # Live dashboard UI
├── solver/
│   ├── mod.rs                   # 5-phase orchestrator with time budgets
│   ├── direct.rs                # V2 + V3 single-pool routing
│   ├── graph.rs                 # Yen's K-shortest paths
│   ├── split.rs                 # Split orders across pools
│   ├── cow_matching.rs          # Coincidence of Wants
│   ├── scoring.rs               # Surplus scoring
│   └── assembler.rs             # Solution assembly + UDCP pricing
├── liquidity/
│   ├── uniswap_v2.rs             # V2 constant-product math
│   ├── uniswap_v3.rs             # V3 tick-traversal math (~1,600 lines)
│   ├── aggregator/                # 0x, Bebop, Paraswap, OKX, and others
│   └── (9 more DEX modules: Balancer V2, Curve, Camelot V2/V3, Trader Joe, GMX V2, Wombat, DODO, Sushiswap)
├── pool_indexer.rs               # Pool cache + event-based refresh
├── subgraph.rs                   # The Graph V3 tick data fetcher
├── ws_monitor.rs                 # WebSocket real-time events
├── competition.rs                # Shadow comparison vs real winners
├── settlement.rs                 # GPv2Settlement ABI encoding
└── monitoring/                   # Metrics, alerts, revenue tracking
shared/            Shared RPC client and utilities used by solver-engine and benchmarks
benchmarks/         Criterion benchmarks for pool sync, routing and solve throughput
scripts/            Local test harness, health checks, driver configs
data/               Sample auction fixture used by tests
plan/               Architecture docs, roadmap, and sprint/incident history
```

## Documentation

See [`plan/`](plan/) for architecture docs, roadmap, and research findings:
- [Architecture overview](plan/architecture/README.md) - system diagram and module map
- [Solving pipeline](plan/architecture/solving-pipeline.md) - how the 5-phase engine works
- [Pool data](plan/architecture/pool-data.md) - all data sources explained
- [Scoring](plan/architecture/scoring.md) - revenue model and projections
- [Competition](plan/architecture/competition.md) - shadow comparison system
- [Roadmap](plan/ROADMAP.md) - current progress
- [Status / incident log](plan/STATUS.md) - most recent deployment history

## Resources

- [CoW Solver onboarding](https://docs.cow.fi/cow-protocol/tutorials/solvers/onboard)
- [Competition rules](https://docs.cow.fi/cow-protocol/reference/core/auctions/competition-rules)
- [Reference implementation](https://github.com/cowprotocol/services)
- [CoW full docs (LLM-optimized)](https://docs.cow.fi/llms-full.txt)

## License

MIT. See [LICENSE](LICENSE).
