# Tech Stack Decision Record - CoW Solver

**Date:** 2026-03-24
**Status:** Accepted

---

## Decision 1: Rust over Python

**Choice:** Rust

**Alternatives considered:** Python (with solver-template-py)

**Rationale:**

| Factor | Rust | Python |
|--------|------|--------|
| Solve speed | 10-50x faster for routing math | Adequate for basic routing |
| Memory | Predictable, no GC pauses | GC pauses during solve window |
| Concurrency | Rayon + Tokio (true parallelism) | GIL limits parallelism |
| Ecosystem | alloy/ethers-rs are mature | web3.py is mature |
| CoW reference code | Services repo is Rust | Template exists but is outdated |
| Production readiness | Single binary, small Docker image | Requires runtime + deps |
| Development speed | Slower to write | Faster to prototype |

**Key reason:** The 30-second auction window is a hard constraint. Rust lets us evaluate more routes in less time. The CoW Protocol reference implementation (driver, solver-engine) is written in Rust, so we benefit from reading their code directly.

**Risk:** Slower initial development. Mitigated by starting with the simplest strategies first and iterating.

**Note:** The Python solver template (`cowprotocol/solver-template-py`) is explicitly marked as outdated and non-functional. It is not a viable starting point.

---

## Decision 2: Axum over Actix-Web

**Choice:** Axum (via Tokio)

**Alternatives considered:** Actix-Web, Warp

**Rationale:**
- Axum is built on Tokio (same runtime as alloy), avoiding executor conflicts
- Simpler API than Actix (no actor model overhead for a single-endpoint server)
- Tower middleware ecosystem (tracing, timeout, compression)
- Growing community adoption; Actix growth has plateaued
- We only have 2-3 endpoints - framework choice is not critical

---

## Decision 3: alloy over ethers-rs

**Choice:** alloy (v0.9+)

**Alternatives considered:** ethers-rs (v2)

**Rationale:**
- alloy is the successor to ethers-rs (same team, Paradigm/Alloy Labs)
- ethers-rs is in maintenance mode; alloy is actively developed
- alloy has better ABI encoding (sol! macro for type-safe contract calls)
- Native support for batch RPC calls and multicall
- Better compatibility with recent EIPs and Arbitrum-specific features

**Risk:** alloy's API is still evolving (breaking changes between minor versions). Pin to a specific version and update deliberately.

---

## Decision 4: RPC Provider - Alchemy (free tier to start)

**Choice:** Alchemy Free Tier (300 compute units/second)

**Alternatives considered:** Infura, QuickNode, Llamanode, self-hosted Geth/Reth

**Rationale:**
- Free tier is sufficient for development and early competition (estimated ~100-200 calls/minute)
- Easy upgrade to Growth plan ($49/month) when volume increases
- Good Arbitrum support (required first chain for onboarding)
- Archive data access (needed for some pool discovery queries)
- Fallback: add Infura as secondary provider for redundancy

**Budget plan:**
| Phase | Provider | Cost |
|-------|----------|------|
| Development | Alchemy Free | $0 |
| Shadow competition | Alchemy Free | $0 |
| Production (low volume) | Alchemy Growth | $49/month |
| Production (high volume) | Alchemy + Infura | $100-200/month |

**Optimization:** Use multicall/batch RPC to minimize call count. Cache pool addresses (permanent). Only refresh reserves per-auction.

---

## Decision 5: DEX Integration Priority

**Choice:** Ordered by volume and impact on fill rate.

| Priority | DEX | Why | Sprint |
|----------|-----|-----|--------|
| P0 | Uniswap V3 | Highest volume, best prices for major pairs | Sprint 1 |
| P0 | Uniswap V2 | Simplest math, good for long-tail tokens | Sprint 1 |
| P0 | Sushiswap | Same interface as V2, additional liquidity | Sprint 1 |
| P1 | Balancer V2 | Strong for weighted pools and LSTs | Sprint 3 |
| P1 | Curve | Dominant for stablecoins | Sprint 3 |
| P2 | 1inch Fusion | Aggregator as fallback | Post-Sprint 3 |
| P2 | 0x / Paraswap | Additional aggregator sources | Post-Sprint 3 |
| P3 | Ambient/CrocSwap | Concentrated liquidity alternative | Future |

**Rationale:** Uniswap V2/V3 + Sushiswap cover ~70% of Ethereum DEX volume. Adding Balancer and Curve brings coverage to ~85%. Aggregators (1inch, 0x) are not direct liquidity sources but can serve as fallback routing.

---

## Decision 6: Target Chain Priority

**Choice:** Arbitrum first, then Mainnet.

| Chain | Priority | Why |
|-------|----------|-----|
| Arbitrum | First (required) | CoW DAO bonding pool requires starting on L2 |
| Ethereum Mainnet | Second | Highest volume, highest revenue potential |
| Gnosis Chain | Third | Low gas, good for learning, xDAI-based |
| Base | Fourth | Growing volume, low gas |

**Rationale:** CoW Protocol requires new solvers joining via the DAO bonding pool to start on Arbitrum. This is non-negotiable. Mainnet is the ultimate target for revenue but requires proving competence on L2 first.

---

## Decision 7: Data Persistence - None (Stateless)

**Choice:** No database. Solver is stateless.

**Rationale:**
- The solver receives a fresh auction every ~30 seconds with all needed data
- Pool addresses are discoverable on-chain (no need to persist)
- Revenue tracking can be computed from logs
- Stateless = simpler deployment, easier scaling, no migration headaches

**Exception:** Metrics and revenue logs are written to disk (JSON log files) for analysis. This is append-only and not a database dependency.

---

## Decision 8: Deployment - Docker on Railway or VPS

**Choice:** Docker, deployed to Railway initially, VPS (Hetzner) for production.

**Alternatives considered:** Kubernetes, AWS ECS, bare metal

**Rationale:**
- The solver is a single binary with no complex orchestration needs
- Railway provides easy deployment with GitHub integration ($5-20/month)
- For production, a Hetzner CX22 ($4.50/month) or CX32 ($7.50/month) provides more control
- Docker ensures reproducible builds and easy rollback
- No need for Kubernetes complexity with a single service

---

## Decision 9: Testing Strategy

**Approach:** Three-tier testing.

| Tier | What | How |
|------|------|-----|
| Unit tests | Math (routing, pricing, gas), data models | `cargo test` with mocked pool data |
| Integration tests | RPC calls, pool discovery, solution encoding | Against Ethereum mainnet fork (Anvil) |
| Competition tests | Full solve cycle against real auctions | Shadow competition + benchmark suite |

**Tools:**
- `cargo test` for unit and integration
- Anvil (from Foundry) for local Ethereum fork
- Historical auction data for benchmarking (downloaded from CoW API)

---

## Decision 10: Logging and Observability

**Choice:** `tracing` crate with JSON output + Prometheus metrics.

**Stack:**
| Component | Tool |
|-----------|------|
| Structured logging | tracing + tracing-subscriber (JSON) |
| Metrics | prometheus-client crate, /metrics endpoint |
| Alerting | Log-based initially, webhook (Discord/Telegram) later |
| Dashboard | Grafana (optional, post-production) |

**Rationale:** tracing is the de facto Rust logging standard. JSON output enables future integration with any log aggregation service. Prometheus metrics are industry standard and supported by Railway/Grafana.
