# External Aggregators - Price Sources Beyond On-Chain Pools

**What this is:** We query external APIs for swap quotes that may beat our internal routing. These include DEX aggregators and private market makers (RFQ).

## Why We Need Them

Our internal routing only uses pools we've indexed (~1,400 V2/V3 pools). External aggregators access 100+ liquidity sources including private market makers, cross-chain bridges, and proprietary pools. They often find better prices, especially for large orders.

## Active Aggregators

### 0x (v2 API)
- **Endpoint:** `GET https://api.0x.org/swap/allowance-holder/quote`
- **Auth:** API key in `0x-api-key` header + `0x-version: v2`
- **Cost:** Free Standard plan (5 req/s)
- **Strength:** Deep liquidity on mainstream pairs. Our closest competitor (zeroex-solve) likely uses this.
- **Current status:** Working but most quotes return "no liquidity" for obscure token pairs.
- **File:** `liquidity/aggregator/zerox.rs`

### Bebop
- **Endpoint:** `GET https://api.bebop.xyz/router/arbitrum/v1/quote`
- **Auth:** None required (free public API)
- **Cost:** Free
- **Strength:** Private market maker quotes (PMMv3). Often beats on-chain by 0.1-0.5% on large trades.
- **File:** `liquidity/aggregator/bebop.rs`

### Paraswap
- **Endpoint:** `GET https://api.paraswap.io/prices?network=42161`
- **Auth:** None required
- **Cost:** Free
- **Strength:** 21 liquidity sources including Hashflow RFQ, Curve, Balancer, DODO, Camelot. Best coverage.
- **Use:** Price oracle (compare against our routing), not for execution.
- **File:** `liquidity/aggregator/paraswap.rs`

### OKX DEX
- **Endpoint:** `GET https://www.okx.com/api/v5/dex/aggregator/quote`
- **Auth:** API key in `Ok-Access-Key` header
- **Cost:** Free with registration
- **Strength:** 100+ sources. Same pattern used by CoW Protocol's own reference solver.
- **Status:** Ready, needs OKX_API_KEY in .env
- **File:** `liquidity/aggregator/okx.rs`

## Pending Integrations

### Liquorice - PMM Aggregator Built for CoW Solvers

**What it is:** Liquorice (`liquorice.tech`) is a private market maker (PMM) aggregator specifically designed for CoW Protocol solvers. Instead of integrating with Bebop, Hashflow, and 10 other market makers individually, Liquorice gives you all of them through one API call.

**How it works:** When you query Liquorice, it fans out to multiple professional market makers simultaneously and returns the best quote. These market makers have deep off-chain inventory and often beat on-chain DEX pools by 0.1-0.5%, especially on large trades ($10K+). The quotes come with signed commitments, so the market maker is obligated to fill at the quoted price.

**Why it matters for us:** The research found that Liquorice is literally wired into the CoW driver codebase (`example.toml` has a `[liquidity-sources-notifier.liquorice]` section). Top-performing solvers like helixbox-solve and rizzolver likely use Liquorice or a similar service to access private liquidity at scale. This is the difference between our current 0% win rate and a potential 8-15% win rate.

**How it fits in the architecture:** Liquorice operates as a "liquidity sources notifier" - the driver notifies Liquorice when it settles a trade, allowing Liquorice's market makers to provide inventory. It's tighter than a simple RFQ: the driver mediates between the solver (us), the market makers (via Liquorice), and the settlement contract.

**Current status:** Account created at `app.liquorice.tech`. Waiting for Discord secret code to activate API access. Once received, we add the API key to `.env` and integrate as another aggregator in Phase 5.

**Expected impact:** +3-5% win rate on large orders. Combined with V3 data and the other aggregators, this is the path from 0% to 8-15% genuine win rate.

## How Phase 5 Works

Phase 5 of the solving pipeline queries aggregators for the top 3 orders by value:

```
Top 3 orders (by sell_amount × reference_price)
   │
   ├──▶ 0x quote (250ms)
   ├──▶ Bebop quote (250ms)
   ├──▶ Paraswap quote (250ms)
   └──▶ OKX quote (250ms)
   │
   ▼
Best quote across all aggregators
   │
   ▼
Compare vs our internal routing
   │
   ▼
Use whichever gives more surplus
```

Aggregator quotes that beat internal routing get included as `Custom` interactions in the solution (the aggregator's calldata is used for execution).

## Key Files

| File | Role |
|------|------|
| `liquidity/aggregator/mod.rs` | Aggregator enum, rate limiter, `build_from_env()`, `best_quote()` |
| `liquidity/aggregator/zerox.rs` | 0x v2 API client |
| `liquidity/aggregator/bebop.rs` | Bebop RFQ client |
| `liquidity/aggregator/paraswap.rs` | Paraswap price oracle |
| `liquidity/aggregator/okx.rs` | OKX DEX API client |
| `liquidity/aggregator/oneinch.rs` | 1inch (exists but not active) |
| `solver/mod.rs` | Phase 5 aggregator strategy |
