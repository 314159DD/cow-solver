## Honest Audit First

**Round 1 resource count:** ~18 distinct URLs, ~12 meaningful sources. Blind spots were real and significant.

**Round 2 added:** 14 new URLs, 10 meaningful findings - several of which materially change the recommendations. Here's what was missing and why it matters.

---

## Addendum: What Round 1 Missed

---

### 🔴 Critical Finding #1 - You're Misdiagnosing Why `liquidity: []` Happens

**This changes the entire #1 recommendation.** Round 1 told you to build a subgraph fetcher yourself. That's correct, but it missed the simpler path sitting right in front of you.

The driver binary - the CoW driver running on your VPS - **already has a complete Uniswap V3 liquidity fetching pipeline built in**. Reading `crates/driver/src/boundary/liquidity/mod.rs` and `config.rs` makes this explicit. The driver supports `uni_v2`, `swapr`, `bal_v2`, `uni_v3`, and `zeroex` collectors, all configured via a `[[liquidity.uniswap-v3]]` block in your `driver.toml`.

The reason you get `liquidity: []` is simply that this block **is not present in your driver configuration**. Adding it makes the driver fetch tick data from The Graph and push it to you per-auction in the `liquidity` array of every `/solve` request you receive - `sqrtPrice`, `tick`, `liquidity`, and the full `liquidityNet` map, serialized as `ConcentratedLiquidityPool` objects.

The exact config to add (from `example.toml`):
```toml
[[liquidity.uniswap-v3]]
preset = "uniswap-v3"
graph-url = "https://gateway.thegraph.com/api/{api-key}/subgraphs/id/FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM"
max_pools_to_initialize = 100
```

**Implementation effort: 30 minutes**, not 2–4 days. You get the subgraph integration, the on-chain event update layer, and the reorg-safe checkpoint pattern for free - it's all already running in the driver.

The driver also ships with built-in Balancer V2 support via the same mechanism:
```toml
[[liquidity.balancer-v2]]
preset = "balancer-v2"
graph-url = "https://gateway.thegraph.com/api/{api-key}/subgraphs/id/{balancer-v2-arbitrum-subgraph-id}"
```

**Do this before anything else.** It is the single highest-leverage action available and it was invisible in the first pass.

**Links:** `github.com/cowprotocol/services/blob/main/crates/driver/src/boundary/liquidity/mod.rs`, `github.com/cowprotocol/services/blob/main/crates/driver/example.toml`

---

### 🔴 Critical Finding #2 - Liquorice: A CoW-Native PMM Aggregator Baked Into the Driver

Round 1 missed this entirely. In `example.toml`, there is a `[liquidity-sources-notifier.liquorice]` section with `base-url = "https://api.liquorice.tech/"` and an `api-key` field. Liquorice (`liquorice.tech`) is a DeFi inventory and lending service that explicitly describes its solver integration as:

> *"With just one integration, gain seamless access to multiple private market makers. Liquorice's offchain service collects quotes from all PMMs and selects the best one for your needs."*

It is also described as providing unified access to CoW Swap, Uniswap X, 1inch Fusion, and Bebop. It is a **PMM aggregator purpose-built for CoW solvers** and is literally already wired into the driver codebase as a notifier. The driver calls Liquorice when it settles a trade, allowing Liquorice PMMs to compete for fill.

This is likely how top solvers like helixbox-solve and rizzolver access private market maker liquidity at scale - through a service like this rather than one-by-one integrations with individual PMMs.

**Action:** Contact `api.liquorice.tech` / the Liquorice team for an API key. This is the PMM aggregation path that Round 1's Bebop and OKX recommendations were only partial proxies for.

**Links:** `liquorice.tech`, driver `example.toml` `[liquidity-sources-notifier.liquorice]` section

---

### 🔴 Critical Finding #3 - The Driver's Update Mechanism Is a Hybrid, Not Just Subgraph

Round 1 described the subgraph approach but missed the full update architecture revealed in `pool_fetching.rs`. The reference implementation uses a two-layer system:

**Layer 1 (baseline/checkpoint):** Fetches all pool states including full `liquidityNet` tick maps from the subgraph at a "reorg-safe" block (current block minus `MAX_REORG_BLOCK_COUNT`). This is the expensive but complete snapshot.

**Layer 2 (real-time delta):** Subscribes to on-chain `Swap`, `Mint`, and `Burn` events via the event indexer. On each `Swap` event, it updates `sqrtPriceX96`, `tick`, and `liquidity` directly in memory. On `Mint`/`Burn`, it updates `liquidityNet` by adding/subtracting from the affected tick boundaries. No subgraph re-query needed between checkpoints.

This means the reference implementation gets **per-block-accurate V3 state** at very low RPC cost after the initial subgraph bootstrap. The `append_events` function in `pool_fetching.rs` shows exactly how the tick map is incrementally maintained. If you build your own V3 fetcher rather than using the driver-provided liquidity, replicate this two-layer pattern.

---

### 🟠 Important Finding #4 - EBBO Rules Define Your Minimum Compliance Requirements

Round 1 did not read the competition rules page. The official CoW Protocol competition rules define "Baseline Liquidity" - the minimum set of protocols your prices must match to avoid EBBO violations and potential slashing.

**For Arbitrum specifically:**
- Protocols: **Uniswap v2/v3, Sushiswap, Swapr, Balancer v2, Pancakeswap**
- Base tokens: **WETH, USDC, USDT, DAI, GNO**

This means you are not just competing to win - you are required by social consensus rules to produce prices at least as good as these protocols can offer. Submitting V2-only solutions on orders that route better through V3 is not just a losing strategy, it may be an EBBO violation. This adds urgency to the V3 fix beyond pure win-rate.

**Links:** `docs.cow.fi/cow-protocol/reference/core/auctions/competition-rules`

---

### 🟠 Important Finding #5 - CIP-74 Changed the Economics - Small Orders Are Now Structurally Unprofitable

The CIP-74 retrospective (Feb 2026) reveals something important for your strategy. The reward cap per order was changed from 0.01 ETH (~$40) to a dynamic cap tied to protocol fees (2 bps of volume, recently reduced to 0.3 bps for correlated assets). The forum post "Second price auction is broken for small orders since CIP-74" explicitly states:

> *"To remain profitable post CIP-74, solvers must introduce fees to break even"*

The practical implication: **prioritize large orders**. Your "closest miss" at -0.1% and your "biggest loss" at -95% on big auctions represent two very different problems. The -0.1% misses on mid-size orders are recoverable with V3 data. The -95% losses on big orders require V3 data AND private market maker quotes. The economics now favor a strategy that focuses on large trades, where the 2 bps fee generates meaningful reward and RFQ providers are most competitive.

The core team acknowledged this is causing solver ecosystem strain and proposed "Performance and Consistency Rewards" as a fix (currently in draft CIP status).

---

### 🟡 Useful Finding #6 - Paraswap as an Aggregator Quote Source (No API Key, Richer Than Round 1 Suggested)

Round 1 mentioned Paraswap but didn't verify it. The live API test confirms: Paraswap's Arbitrum adapter list includes **21 liquidity sources** - UniswapV2, UniswapV3, SushiSwap, BalancerV2, Curve (V1 and V2), WooFiV2, Hashflow, AugustusRFQ (their private RFQ network), Camelot, Ramses, DODO, and more. No API key required for price queries.

```
GET https://api.paraswap.io/prices
  ?srcToken=<addr>&destToken=<addr>
  &amount=<amt>&srcDecimals=6&destDecimals=18
  &side=SELL&network=42161
```

Returns `priceRoute.destAmount` and identifies the best route. Unlike Odos (which requires POST), this is a simple GET. This is genuinely useful as a price-checking oracle and gives you access to Hashflow and Curve pricing without needing separate integrations.

**The key difference from Odos:** Paraswap returns the best price across 21 sources (including private RFQ via Hashflow and AugustusRFQ) and is already proven working on Arbitrum today.

**Links:** `api.paraswap.io/adapters/list?network=42161`, `api.paraswap.io/prices`

---

### 🟡 Useful Finding #7 - Hashflow Is Behind Cloudflare Auth, Not Freely Accessible

Round 1 listed Hashflow as a potential integration. Testing `api.hashflow.com` hits a Cloudflare auth wall - it requires partnership credentials, not just registration. Hashflow is accessible through Paraswap (which includes it as an adapter) and was also recently removed from several aggregators due to changing their business model. Don't prioritize a standalone Hashflow integration; get it through Paraswap instead.

---

### 🟡 Useful Finding #8 - The Auction Deadline on Arbitrum Is 40 Blocks

Found in the competition rules doc. The deadline for submitting solutions on Arbitrum is **40 blocks** after the auction opens. At ~250ms per block, that's 10 seconds total. Your 10-second polling interval therefore means you are sometimes submitting with data that is exactly at the stale-data boundary. Any auction where the market moved in that final block before your poll caught it is a loss. This confirms the urgency of WebSocket subscriptions.

---

### 🟡 Useful Finding #9 - Codex API (Powered by Defined.fi) for Real-Time DEX Data

Round 1 mentioned Defined.fi as a possibility but didn't verify it. The Codex API (`docs.codex.io`) is the underlying blockchain data API that powers Defined. It provides GraphQL subscriptions for real-time DEX events and pool state, with support for Arbitrum. This is more oriented toward analytics than trading infrastructure, but their WebSocket subscriptions could supplement Alchemy for raw swap event delivery. Free tier available with API key signup. Less relevant than Alchemy WebSockets for this specific use case, but worth knowing it exists as a fallback.

---

### 🟡 Useful Finding #10 - The `liquidity-sources-notifier` Architecture Explains The Real Architecture

The driver notifies Liquorice (and potentially other PMMs) when it settles a trade, allowing them to provide collateral/inventory. This is not just an RFQ pattern - it's an inventory management loop. The winning solvers are likely participants in this ecosystem: they receive the auction, query Liquorice for PMM quotes, win by combining the best PMM price with on-chain routing for any remaining amount, then the settlement notification closes the loop. Round 1 described RFQ as "fire a quote in parallel" - the actual architecture is tighter than that, with the driver actively mediating.

---

## Revised Priority Order

With the new findings, the updated sequence is:

**Day 1 (30 minutes):** Add `[[liquidity.uniswap-v3]]` to your driver config with The Graph Arbitrum subgraph URL. Also add `[[liquidity.balancer-v2]]`. This is the single most leveraged action possible.

**Day 2–3:** Contact Liquorice for API key access. This is your path to PMM aggregation.

**Week 1:** Upgrade pool event monitoring to WebSocket (critical given the 40-block / 10-second deadline constraint).

**Week 2:** Add Paraswap as a price oracle for order routing validation; integrate Bebop for direct RFQ fills on high-value mainstream pairs.

**Month 2:** Pursue OKX DEX API integration (same pattern as CoW's own solver); evaluate Tycho for Arbitrum when available.

---

## Resource Count Summary

| Round | URLs checked | Meaningful findings |
|-------|-------------|---------------------|
| Round 1 | ~18 | ~12 |
| Round 2 | ~32 | ~10 new |

The most important things Round 1 missed were all in the cowprotocol/services codebase itself - the driver config, the boundary liquidity module, and the Liquorice integration. The second most important was reading the official competition rules page, which revealed EBBO requirements and the 40-block deadline. External API research was largely adequate in Round 1; internal CoW infrastructure research was not.