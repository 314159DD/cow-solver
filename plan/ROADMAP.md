# CoW Solver - Roadmap

**Last Updated:** 2026-04-07
**Status:** v2.2 - 5 competitive improvements implemented. 0.99x on matching auctions. Deploying with Odos enabled for data collection.

---

## Phase 1: Foundation (Sprint 1) ✅ COMPLETE
- Rust workspace, data models, Axum HTTP server
- Basic V2/V3 pool math, sample auction data

## Phase 2: Core Solver (Sprint 2) ✅ COMPLETE
- CoW matching, direct routing, multi-hop, split routing
- UDCP pricing, Balancer V2, Curve integration

## Phase 3: Optimization (Sprint 3) ✅ COMPLETE
- EBBO compliance, scoring, parallel solving
- Time budgets, JIT liquidity

## Phase 4: Production (Sprint 4) ✅ COMPLETE
- Structured logging, Telegram alerts, Prometheus metrics
- Gas oracle, chain config, fallback strategy

## Phase 5: Satellite Systems ✅ COMPLETE
- Pool indexer (1,395 pools), pool discovery
- Replay DB, accounting, revenue tracking
- Freshness scoring, submission policy
- Settlement ABI encoding

## Phase 6: Shadow Competition ✅ COMPLETE
- Connected to CoW driver on Arbitrum
- Competition tracker (shadow comparison against real winners)
- Dashboard with real-time metrics + earnings projections
- ~1,787 auctions processed, competition data flowing

## Phase 7: Competitive Routing ✅ COMPLETE
- [x] V3 subgraph tick fetcher (The Graph, 89 pools, 30K ticks)
- [x] WebSocket real-time pool events (sub-second freshness)
- [x] Bebop RFQ integration (free, Arbitrum)
- [x] Paraswap price oracle (21 sources)
- [x] OKX DEX API integration (CoW reference pattern)
- [x] CIP-74 large order prioritization
- [x] New order detection + priority
- [x] V3 Mint/Burn event handling
- [x] Two-pass JIT refresh (fresh reserves for solution pools)

## Phase 8: Competitive Edge (v2.2) ✅ COMPLETE
- [x] Internalization - `internalize: true` on trusted token interactions (saves ~100-150K gas)
- [x] CoW AMM baseline check - filter bad CoW matches before proposing
- [x] High-competition pair deprioritization - avoid WETH/USDC where aggregator-solvers dominate
- [x] CIP-67 liquid pair filter - prevent solution rejection from fairness filter
- [x] Protocol fee bonus estimation - env-var gated, estimates driver's fee component
- [x] Odos budget limiter - rolling 24h/1h windows to protect free tier quota

## Phase 9: Data Collection & Tuning 🔄 IN PROGRESS
- [ ] Deploy v2.2 to VPS with Odos enabled (`AGG_ENABLED=true`)
- [ ] Run 2-4 hours, collect replay data with Odos routing
- [ ] Pull replay DB, measure Odos impact on INFLATED auctions
- [ ] If ACCURATE >40%: enable `PROTOCOL_FEE_BONUS=true`
- [ ] If scoring >1.0x: register as solver (KYC + bonding)

## Phase 10: Go Live (Next)
- [ ] 24h shadow run with >5% genuine win rate
- [ ] Register as solver (KYC + bonding ~1 ETH)
- [ ] Enable live competition
- [ ] Monitor P&L, tune scoring caps

## Phase 11: Revenue Optimization (Future)
- [ ] Liquorice PMM integration (when API key received)
- [ ] Intelligence layer APIs (pending research report)
- [ ] Goldsky subgraph redundancy
- [ ] Local Arbitrum node (when revenue justifies cost)
- [ ] Cross-DEX arbitrage capture
