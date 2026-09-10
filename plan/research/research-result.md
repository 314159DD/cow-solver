# CoW Solver Win Rate Research: Ranked Recommendations

**Research date:** 2026-03-31 | Sources: The Graph, Goldsky, Alchemy, cowprotocol/services, Bebop API, Tycho/PropellerHeads, Odos, CoW Forum

---

## Executive Summary

You are losing because competitors have accurate V3 tick data and you have none. This is confirmed by the -95% gap on large trades (which flow through V3 almost exclusively) vs. -0.1% on small trades (which V2 can price accurately). The fastest path to 5%+ win rate is getting real V3 tick data from a subgraph in the next 2–3 days, then adding RFQ and WebSocket subscriptions. The longer-term moves involve the OKX/Bebop aggregator patterns that CoW Protocol's own official solver uses.

---

## Ranked Recommendations

---

### #1 - Uniswap V3 Tick Data via The Graph (Decentralized Network)

**What it is:** The Graph hosts the official Uniswap V3 Arbitrum subgraph on its decentralized network. The subgraph ID is `FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM` and it is 100% indexed. The CoW Protocol reference implementation (`cowprotocol/services`) already has a complete, production-tested implementation of exactly this query pattern in `crates/liquidity-sources/src/uniswap_v3/graph_api.rs`.

**What you get:** `sqrtPrice`, `tick`, `liquidity`, and `liquidityNet` per initialized tick - everything your V3 math needs. Queries are paginated with `first`/`where: { id_gt: $lastId }` and run against a specific block number.

**The query pattern (copied directly from the reference implementation):**
```graphql
query Ticks($block: Int, $pool_ids: [ID], $pageSize: Int, $lastId: ID) {
  ticks(
    block: { number: $block }
    first: $pageSize
    where: { id_gt: $lastId, liquidityNet_not: "0", pool_: { id_in: $pool_ids } }
  ) { id tickIdx liquidityNet poolAddress }
}
```

**Implementation effort:** 2–4 days. The Rust data structures and pagination logic are already in the CoW reference code - you can port `graph_api.rs` almost verbatim. You already have all V3 math.

**Latency:** The subgraph lags ~2–5 blocks (Arbitrum blocks are ~250ms, so ~0.5–1.25 seconds). Refresh on each block or every 5 seconds via polling.

**Cost:** Free tier (requires an API key from The Graph Studio at `studio.thegraph.com`). Free tier has rate limits (~100K queries/month) - sufficient for your 379 pools if you fetch only pools that traded in the last block rather than all pools on every request. Growth beyond that is ~$4/1M queries (pay-per-query).

**Query URL:**
```
https://gateway.thegraph.com/api/{api-key}/subgraphs/id/FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM
```

**Expected win rate impact:** +5–10 percentage points. This is the fix for 80% of your losses. With correct V3 math you should price large trades within 1–2% of competitors, making you competitive on a meaningful fraction of auctions.

**Links:**
- Graph Explorer: `thegraph.com/explorer/subgraphs/FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM`
- Reference implementation: `github.com/cowprotocol/services/blob/main/crates/liquidity-sources/src/uniswap_v3/graph_api.rs`
- The Graph Studio (API keys): `studio.thegraph.com`

---

### #1b - Goldsky as Subgraph Fallback / Alternative

**What it is:** Goldsky is a specialized blockchain data provider with its own subgraph infrastructure. Their free Starter tier allows 3 always-on subgraphs and unlimited community subgraph access. The Uniswap V3 Arbitrum subgraph is available as a community subgraph you can fork and host at no cost.

**Why useful:** The Graph's decentralized network can occasionally be slow or have indexer availability issues. Goldsky runs separate infrastructure - using both provides redundancy. Their API is compatible with the same GraphQL queries.

**Implementation effort:** 1 day incremental (same query code, different URL).

**Cost:** Free tier: 3 always-on custom subgraphs, rate-limited to 20 req/10s. Scale tier: ~$37/month per worker for higher throughput.

**Links:** `goldsky.com`, `goldsky.com/pricing`

---

### #2 - Alchemy WebSocket Subscriptions for Sub-Second Pool Updates

**What it is:** Alchemy supports `eth_subscribe` over WebSocket (wss://) for `logs` events filtered by contract address and topic. Instead of polling `eth_getLogs` every 10 seconds, you subscribe once and receive events in real time as each Arbitrum block is produced.

**Relevant subscription setup:**
```json
{
  "method": "eth_subscribe",
  "params": ["logs", {
    "address": ["<pool_address_1>", "<pool_address_2>", ...],
    "topics": ["0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"]
  }]
}
```
The topic above is the Uniswap V3 `Swap` event signature. For V2 `Sync` events the topic is `0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1`.

**What you get:** Event delivery within the same Arbitrum block they occur in (~250ms block time). Update your local pool reserve cache immediately on receipt. This brings you from ~10-second staleness to ~1-second or better.

**Alchemy free tier limits:** 100 WebSocket connections, 1,000 unique subscriptions per connection. With 1,395 pools you would need 2 connections with ~700 subscriptions each - this fits in the free tier.

**CU cost:** WebSocket subscriptions consume compute units only when events fire, not for the persistent connection. At 470 auctions/hour with Arbitrum's typical trade density, estimated CU usage for WebSocket log subscriptions covering your pool set is roughly 3–5M CU/month additional - well within your 30M free budget.

**Implementation effort:** 2–3 days (refactoring the pool indexer to use WebSocket instead of polling, adding reconnect logic).

**Expected win rate impact:** +1–3 percentage points on fast-moving markets. Primarily helps you stop losing on "stale data" auctions where the price moved 0.1–0.5% in the last 10 seconds.

**Links:**
- `docs.alchemy.com/reference/subscription-api`
- `docs.alchemy.com/reference/eth-subscribe`

---

### #3 - Bebop RFQ Integration (Free, No API Key Required)

**What it is:** Bebop is a DeFi liquidity aggregator backed by institutional market makers. Their public REST API is live on Arbitrum with no authentication required. You just verified live it returns quotes with two route types (`PMMv3` - a private market maker quote - and `JAMv2`). The endpoint pattern is:

```
GET https://api.bebop.xyz/router/arbitrum/v1/quote
  ?buy_tokens=<address>
  &sell_tokens=<address>
  &sell_amounts=<amount>
  &taker_address=<your_solver_address>
  &approval_type=Standard
```

**What you get:** Real market maker prices that are often better than on-chain pools, especially for mainstream pairs (ETH/USDC, WETH/USDC, etc.). The response includes `buyTokens[].amount` which is the actual output amount. You compare this to your best on-chain route and submit whichever gives more surplus.

**Integration:** For a CoW solve request, when you get an order for a supported pair, fire off a Bebop quote in parallel with your on-chain routing. If Bebop returns a better price, construct your solution using the Bebop fill (the order data in `toSign` gives you a signed maker commitment). The settlement would use Bebop's settlement contract.

**Implementation effort:** 3–5 days (HTTP client for Bebop API + logic to compare Bebop vs on-chain quote + construct the settlement interaction from Bebop's `toSign` payload).

**Cost:** Free public API. They offer authenticated plans with better pricing and higher rate limits - worth requesting once you see volume.

**Expected win rate impact:** +2–4 percentage points for mainstream pairs. Private market maker liquidity often beats on-chain pools by 0.1–0.5% on large trades, exactly the margin you need to win.

**Note:** The official CoW Protocol solver (in `crates/solvers/src/infra/dex/`) integrates OKX and BitGet as external DEX aggregators. Bebop operates on a similar model but is specifically strong on Arbitrum for major pairs. Hashflow is another RFQ provider on Arbitrum worth exploring but requires a partnership agreement.

**Links:** `api.bebop.xyz/router/arbitrum/v1/quote` (live), `docs.bebop.xyz`

---

### #4 - OKX DEX API Integration (The Pattern CoW's Own Solver Uses)

**What it is:** CoW Protocol's official solver implementation (`crates/solvers/src/infra/dex/okx/`) integrates the OKX DEX API as a price source and liquidity provider. OKX DEX aggregates across ~100+ liquidity sources on Arbitrum including their own private market making. The OKX Web3 DEX API returns swap quotes with full routing paths.

**Why this matters:** If `zeroex-solve` (your closest competitor at -19.9%) is using 0x's aggregator which pulls from many sources, and your scores are within 20%, OKX could be the additional source that bridges the gap. The official CoW solver proving this pattern validates it as production-appropriate.

**API:** `https://www.okx.com/api/v5/dex/aggregator/quote` (Arbitrum = chainId 42161). Requires OKX API key (free to obtain).

**Implementation effort:** 4–6 days (OKX API integration + comparing quotes vs. on-chain).

**Cost:** OKX API is free with registration. Rate limits apply.

**Expected win rate impact:** +2–3 percentage points, primarily on mid-large orders where aggregator routing outperforms your V2 routing.

**Links:** `github.com/cowprotocol/services/tree/main/crates/solvers/src/infra/dex/okx`

---

### #5 - Direct RPC V3 Tick Data as Immediate Fallback

**What it is:** While you wait for the subgraph to be integrated, you can fetch tick data directly via RPC using multicall. The V3 pool contract has `tickBitmap(int16 wordPosition)` which returns a 256-bit bitmap of which ticks are initialized, and `ticks(int24 tick)` which returns `liquidityGross`, `liquidityNet`, and fee growth values. The pool's `slot0()` returns `sqrtPriceX96`, `tick`, and `observationIndex`.

**Approach:** For each V3 pool per auction:
1. Call `slot0()` - 1 RPC call per pool (current price + tick)
2. Call `tickBitmap()` for the 5–10 words around the current tick - ~5–10 calls per pool
3. Call `ticks()` for each initialized tick found - variable, typically 10–50 calls per pool

**Total per pool:** ~20–60 `eth_call` requests. At 40 CU each = 800–2400 CU per pool. For 379 V3 pools: ~300K–900K CU per full refresh. At your current rate (25M CU/month) a full refresh every 30 minutes would add ~15M CU - borderline. A smarter approach is to refresh only pools involved in each specific auction order, cutting this dramatically.

**Implementation effort:** 3–5 days (multicall batching + tick bitmap decoding).

**Cost:** Fits in free Alchemy tier if targeted to auction-relevant pools only. Would blow the budget if refreshing all 379 pools frequently.

**Expected win rate impact:** +3–7 percentage points (same as subgraph approach, but with higher RPC cost and more implementation complexity - use the subgraph instead, this is a fallback).

**Links:** Uniswap V3 pool ABI at `docs.uniswap.org/contracts/v3/reference/core/interfaces/pool/IUniswapV3PoolState`

---

### #6 - Tycho (PropellerHeads) - Future-Proof Liquidity Infra (Watch List for Now)

**What it is:** Tycho is an open-source, Rust-native liquidity indexer and simulation framework by PropellerHeads. It streams real-time protocol state deltas (new blocks, pool state changes) over WebSocket, provides a unified `get_amount_out()` interface across all protocols, handles reorgs automatically, and processes updates in under 100ms. It natively supports `UniswapV3State`, `BalancerV2`, `Curve`, Uniswap V4, and many more - all through the same API.

**Critical caveat: Tycho currently supports Ethereum, Base, and Unichain only. Arbitrum is not yet supported.** Their supported chains page confirms this explicitly as of today.

**Why to watch:** Tycho is clearly the direction the industry is moving. PropellerHeads is active (150+ stars, commits through March 31, 2026), and Arbitrum support is likely coming. When it arrives, integrating Tycho would solve your V3 data problem, your update latency problem, and give you Balancer V2 and Curve for free.

**Action now:** File an issue or reach out to PropellerHeads (`propellerheads.xyz`) asking about Arbitrum support timeline.

**Implementation effort (when available):** 1–2 weeks to integrate `tycho-simulation` into your Rust codebase.

**Cost:** Tycho has a hosted webstream service with an API key; pricing is contact-based but has been historically free for early adopters.

**Links:** `docs.propellerheads.xyz/tycho`, `github.com/propeller-heads/tycho-indexer`, `github.com/propeller-heads/tycho-simulation`

---

### #7 - Balancer V2 + Curve StableSwap Integration (RPC-based)

**What it is:** Balancer V2 weighted and stable pools can be priced via the Vault contract. Curve StableSwap pools have an invariant computable from on-chain state. Both are missing from your current pool set.

**Balancer V2 on Arbitrum:** Vault address `0xBA12222222228d8Ba445958a75a0704d566BF2C8`. Call `getPoolTokens(poolId)` to get balances. The CoW Protocol reference code has complete Balancer V2 support in `crates/liquidity-sources/src/balancer_v2/`.

**Curve on Arbitrum:** The major Curve pools are 2pool (USDC/USDT) and tricrypto. Curve pools expose `get_dy(i, j, dx)` directly.

**Implementation effort:** 1–2 weeks for both.

**Cost:** RPC calls only - fits in your Alchemy budget.

**Expected win rate impact:** +1–2 percentage points on stablecoin-heavy orders.

---

### #8 - Odos API as Routing Aggregator

**What it is:** Odos is a DEX aggregator live on Arbitrum (confirmed: chainId 42161 in their chains list). They aggregate V2, V3, Balancer, Curve, and ~100 more sources in a single quote call. Instead of building your own routing, you call Odos and use their output amount as your solution's claimed surplus.

**API:** `POST https://api.odos.xyz/sor/quote/v2` with JSON body containing `chainId`, `inputTokens`, `outputTokens`, `userAddr`. Returns `outAmounts` and execution calldata.

**Tradeoff:** You would be trusting Odos's routing rather than running your own. The upside is massive coverage immediately. The downside is Odos's routing isn't optimized for the CoW surplus metric - it optimizes for the taker getting maximum output, not for solver surplus. You also don't control the execution path, which complicates CoW settlement encoding.

**Better use:** Use Odos quotes as a price oracle to validate or sanity-check your own routing, rather than as the execution path.

**Implementation effort:** 1–2 days for price comparison, 1–2 weeks to build execution integration.

**Cost:** Free API, no key required.

**Links:** `api.odos.xyz`, `docs.odos.xyz`

---

### #9 - Local Arbitrum Node (Skip for Now)

**What it is:** Running a local Arbitrum full node would give you sub-100ms pool state access with zero RPC cost.

**Hardware:** Arbitrum One full node requires ~2 TB SSD (fast NVMe), 32 GB RAM, 8 cores. Monthly cost: ~$200–400/month on a dedicated server or cloud (e.g., Hetzner AX101 at ~€150/month), or significantly more on AWS/GCP.

**Latency benefit:** ~50–100ms vs. Alchemy's ~150–300ms for Arbitrum. This matters for the very last percentile of score differences.

**Verdict:** The latency advantage is real but modest compared to getting V3 data right. Run this only after your V3 data and RFQ integrations are working and you're winning 3–5%+ of auctions. The $200–400/month cost is only justified if you're generating meaningful revenue.

---

## Implementation Roadmap

**Week 1 (do this immediately):**
1. Get a Graph Studio API key and point your V3 pool fetcher at the Uniswap V3 Arbitrum subgraph - port the query pattern from `cowprotocol/services/graph_api.rs`. Target: V3 tick data flowing within 48 hours.
2. Switch your existing pool indexer from HTTP `eth_getLogs` polling to Alchemy WebSocket `eth_subscribe` for `Sync` and `Swap` events.

**Week 2:**
3. Integrate Bebop API for RFQ quotes on mainstream pairs. Compare Bebop vs. on-chain routing per solve request and take the better price.
4. Register for an OKX API key and port the OKX DEX integration pattern from the CoW reference solver.

**Week 3–4:**
5. Add Balancer V2 pool support using the reference implementation as a template.
6. Register for Goldsky and set it as your subgraph failover.

**Month 2:**
7. Monitor Tycho for Arbitrum support - integrate when available (this will solve latency, V3 data, and Balancer/Curve in one shot).

---

## Answers to Specific Research Questions

**Is The Graph's Uniswap V3 subgraph available on Arbitrum?** Yes, fully indexed (100%), 10M queries in past 30 days, on the decentralized network. Latency is ~2–5 blocks (~0.5–1.5s). Free tier available via Graph Studio API key.

**Are there other subgraph providers?** Goldsky (free Starter tier, 3 subgraphs). Satsuma was acquired by Goldsky. Alchemy Subgraphs is a separate product from their RPC service but focused on Ethereum mainnet.

**Can we get tick data directly from RPC?** Yes - `slot0()`, `tickBitmap()`, `ticks()` calls as described in #5. High RPC cost if done naively; viable if targeted per-auction.

**How do projects like 1inch source their V3 data?** Combination of their own full nodes (they run private node infra) and subgraphs. At your scale, the subgraph is the right answer.

**Alchemy WebSocket subscriptions?** Fully supported, covered in #2. The `logs` subscription with address + topic filter is the right approach.

**What RFQ providers work on Arbitrum?** Bebop (confirmed working, no key needed), OKX DEX (free key). Hashflow requires a partnership. 1inch Fusion is a taker-facing product not a solver-facing API. 0x Standard API (which you have) covers mainstream pairs.

**Are there CoW-specific RFQ networks?** The driver can inject liquidity, but yours sends `liquidity: []`. The CoW solver competition doesn't have a separate RFQ network - each solver sources its own. Private market maker integrations (0x, Bebop, OKX) are what winning solvers use.

**What open-source CoW solver implementations exist?** The primary reference is `cowprotocol/services` (Rust, MIT + GPL licensed). The `crates/solvers/` crate contains solver implementations. Currently implemented: OKX and BitGet as external aggregator solvers. The `liquidity-sources` crate has V2, V3, Balancer V2, and Swapr implementations. No other full solver implementations found publicly.

**Are helixbox-solve, rizzolver, zeroex-solve open-source?** No public GitHub repositories found. ZeroEx-solve is almost certainly built on 0x's infrastructure. Rizzolver and Helixbox appear to be proprietary.

**What latency do other solver teams achieve?** No public benchmarks found. Based on Tycho's documentation ("under 100ms plus network latency"), competitive solvers are likely operating at 100–500ms update latency. Your 10-second lag is catastrophically behind.

**DEX aggregator as routing engine tradeoffs:** Odos and 1inch return executable routes optimized for the taker, not for the CoW auction's surplus metric. You can use them for price comparison but would need to re-encode the execution for CoW settlement. Significant additional work to do correctly. Not recommended as a first step.

**MEV tools (Flashbots, MEV-Share)?** Not relevant to CoW Protocol auctions. CoW settles through a single solver who won the batch auction - MEV-Share is for Ethereum public mempool, not applicable here.