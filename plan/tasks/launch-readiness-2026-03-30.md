# Launch Readiness Assessment - 2026-03-30

## Executive Summary

The solver is **NOT ready for live competition**. It can submit structurally valid solutions
and compete in shadow mode, but has fundamental blockers that prevent winning auctions.

## What Works

| Component | Status | Evidence |
|-----------|--------|----------|
| Auction reception | Working | 976 orders/auction, 8s cadence |
| Order parsing | Working | All order fields parsed correctly |
| Direct routing (Phase 1) | Working | Finds 5 profitable routes per auction |
| Multi-hop routing (Phase 2) | Working | Explores intermediaries |
| Score computation | Approximate | 1.2M gwei vs winners at 10K-2M gwei (same order of magnitude) |
| Competition comparison | Working | 4 auctions compared: +24%, -81%, -93%, +3613% gaps |
| Dashboard | Working | Real metrics, earnings projections, honest labels |
| Pool indexer | Working | 1395 pools cached, hot-pool refresh every 30s |
| JIT refresh | Working | 5 solution pools refreshed with current reserves |
| Settlement ABI encoding | Implemented | Real GPv2Trade structs with order data |
| EBBO validation | Wired | 500bps tolerance, filtering active |
| Token approvals | Wired | Prepended in assembler |

## What Doesn't Work

### BLOCKER 1: Cannot validate solutions locally
- Settlement eth_call requires **authorized solver address** (on-chain registration + bonding)
- Also requires **valid order signatures** from the order creators
- We have neither → eth_call always reverts → all solutions held
- **Workaround**: disable eth_call sim, submit and let driver validate
- **Real fix**: register as solver on-chain (requires bonding ~1 ETH on Arbitrum)

### BLOCKER 2: Scores are nearly constant (~1.23M gwei)
- Same ~5 stale limit orders matched every auction
- Per-order cap at 1e14 creates artificial ceiling
- Real competition scores vary 10x-1000x between auctions
- **Root cause**: we route through same cached pools with same stale orders
- **Fix needed**: better order selection based on freshness/opportunity

### BLOCKER 3: Solver not registered with CoW Protocol
- To compete in real auctions, solver must be:
  1. Registered on-chain with the settlement contract
  2. Bonded (deposit ~1 ETH as stake against misbehavior)
  3. Approved by CoW DAO governance (or use permissionless bonding pool)
- Without registration, our solutions are silently dropped by the driver
- See: https://docs.cow.fi/cow-protocol/tutorials/solvers/onboard

### BLOCKER 4: Driver sends 0 liquidity pools
- Confirmed: `driver_pools=0` every auction
- We fall back to cached pools (stale reserves)
- Real competitive solvers either:
  a. Receive fresh pools from their driver instance (requires custom driver setup)
  b. Source liquidity independently (what we do, but stale)
  c. Use private market maker relationships (RFQ)
- **Fix**: either run our own driver with pool fetching, or improve our indexer freshness

## Competition Performance (Honest Numbers)

From 4 compared auctions:
- **Best result**: +24.1% above winner (score inflated by stale data)
- **Typical result**: -81% to -93% below winner (we find less surplus)
- **Earnings potential if competitive**: ~$2/auction on settled auctions
- **Projected revenue at current performance**: $0/month (not submitting)
- **Projected if submitting and competitive**: $60-200/month (very rough)

## Cost Analysis

| Resource | Current | Optimized |
|----------|---------|-----------|
| Alchemy RPC | $141/month (pay-as-you-go) | ~$16/month or $0 on free tier |
| VPS | Fixed cost | Fixed cost |
| Bonding stake | $0 (not registered) | ~$1,800 (1 ETH) |

## Critical Path to Live

### Phase 1: Shadow Competition (Current - 1 day to complete)
1. Disable eth_call sim (DONE)
2. Submit solutions to driver for its validation
3. Run shadow comparison for 24h to establish baseline
4. Measure: what % of our solutions does the driver accept?

### Phase 2: Solver Registration (1-2 weeks)
1. Review CoW solver onboarding: https://docs.cow.fi/cow-protocol/tutorials/solvers/onboard
2. Bond ~1 ETH through the bonding pool
3. Get solver address whitelisted
4. Requires governance approval or use permissionless pool

### Phase 3: Competitive Routing (2-4 weeks)
1. Fix scoring to match CoW Protocol exactly (7x factor identified)
2. Improve order selection (don't route same stale orders every time)
3. Add V3 concentrated liquidity support (most volume is V3)
4. Consider running own driver for fresh pool data

### Phase 4: Revenue Optimization (Ongoing)
1. Private market maker quotes (RFQ integration)
2. Multi-order CoW matching optimization
3. Cross-DEX arbitrage capture
4. Settlement internalization (gas savings)

## Recommendation

**Do NOT go live yet.** The solver submits solutions but they will be rejected because:
1. We're not registered as a solver
2. Our routes use stale pool data that won't execute

**Next step**: Run in shadow mode for 24h with eth_call sim disabled. Track how the
competition comparison gaps trend. If consistently within ±50% of winners, proceed
to solver registration. If gaps are consistently >200%, focus on routing quality first.
