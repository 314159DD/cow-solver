# Sprint: Get to 5%+ Win Rate

**Created:** 2026-03-31
**Source:** Research handoff results + correction audit
**Goal:** Genuine wins on testnet, confident enough to go live

---

## The #1 Fix: Ask CoW Team to Configure Driver Liquidity

**This is not a code change. It's a message to Tamir.**

The CoW driver already has V3 liquidity fetching built in. It just needs config:

```toml
[[liquidity.uniswap-v3]]
preset = "uniswap-v3"
graph-url = "https://gateway.thegraph.com/api/{api-key}/subgraphs/id/FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM"
max_pools_to_initialize = 100

[[liquidity.balancer-v2]]
preset = "balancer-v2"
```

This gives us: fresh V3 tick data + Balancer V2 pools per auction.
Implementation effort: 0 code changes. 30 minutes for CoW team.
Expected impact: +5-10% win rate.

**Message to send Tamir:**
"Hey Tamir, following up on the liquidity config - could you add
[[liquidity.uniswap-v3]] and [[liquidity.balancer-v2]] blocks to
our driver config? The Uniswap V3 Arbitrum subgraph ID is
FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM. We already parse
concentratedLiquidity format and have full V3 tick-traversal math
ready to use. This is the single biggest improvement we need."

---

## Phase 1: Immediate (Day 1)

### Task 1.1: Send the message to Tamir (above)
- Owner: PM
- Effort: 5 minutes
- Impact: Unlocks everything else

### Task 1.2: Get The Graph API key as backup
- Sign up at studio.thegraph.com
- Get API key for Uniswap V3 Arbitrum subgraph
- This is our fallback if the driver config takes time
- Owner: PM
- Effort: 15 minutes

### Task 1.3: Contact Liquorice for API key
- Visit liquorice.tech
- Request solver API access
- This gives us private market maker quotes (how top solvers win)
- Owner: PM
- Effort: 15 minutes

---

## Phase 2: Code Changes (Day 1-3)

### Task 2.1: Build subgraph V3 tick fetcher (backup for driver)
- Query The Graph for V3 tick data directly
- Port pattern from cowprotocol/services graph_api.rs
- Refresh every 5 seconds for active pools
- Effort: 2-3 days
- Impact: Same as driver config but self-sourced

### Task 2.2: WebSocket pool monitoring
- Replace eth_getLogs polling with Alchemy WebSocket eth_subscribe
- Subscribe to Sync + Swap events for our pool set
- Goes from 10s stale to sub-second
- Fits Alchemy free tier (100 WS connections, 1000 subscriptions each)
- Effort: 2-3 days
- Impact: +1-3% win rate on fast-moving markets

### Task 2.3: Integrate Bebop RFQ
- Free API, no key needed
- GET https://api.bebop.xyz/router/arbitrum/v1/quote
- Compare Bebop price vs our routing, take better one
- Effort: 3-5 days
- Impact: +2-4% on mainstream pairs (WETH/USDC, ARB/USDC)

### Task 2.4: Integrate Paraswap as price oracle
- Free API, 21 liquidity sources including Hashflow RFQ
- Use as sanity check against our routing
- GET https://api.paraswap.io/prices?network=42161
- Effort: 1-2 days
- Impact: Better order selection, EBBO compliance validation

---

## Phase 3: Optimization (Week 2)

### Task 3.1: Integrate Liquorice PMM (if API key received)
- Purpose-built for CoW solvers
- Unified access to multiple market makers
- Effort: 3-5 days
- Impact: +3-5% on large orders

### Task 3.2: Prioritize large orders (CIP-74)
- Small orders are structurally unprofitable post CIP-74
- Focus routing effort on orders > 0.1 ETH value
- Effort: 1 day
- Impact: Better resource allocation

### Task 3.3: Remove per-order score cap (once V3 data is live)
- Currently capped at 5e14 per order to prevent stale inflation
- With fresh V3 data, surplus is accurate - cap can go
- Effort: 5 minutes
- Impact: Honest scores that match competition

---

## Expected Outcome

| Milestone | Win Rate | Revenue Estimate |
|-----------|----------|-----------------|
| Current (V2 only, stale data) | 0% | $0/month |
| After driver V3 config | 3-8% | $500-2000/month |
| + WebSocket + Bebop RFQ | 5-12% | $1000-4000/month |
| + Liquorice PMM | 8-15% | $2000-8000/month |

---

## Key Facts for Decision Making

- EBBO requires matching Uniswap V3 prices. V2-only may violate rules.
- Arbitrum auction deadline is 40 blocks (10 seconds). Our 10s polling = borderline.
- CIP-74 changed economics: prioritize large orders over small.
- Top competitors (helixbox, rizzolver) likely use Liquorice or similar PMM aggregator.
- Our closest competitive loss was -0.1% - the routing logic works, we just need data.
