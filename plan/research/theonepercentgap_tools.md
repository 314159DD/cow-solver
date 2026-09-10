# CoW Protocol Solver on Arbitrum - Competitive Intelligence: APIs, Platforms & Services

> **Research date:** April 7, 2026. URLs, pricing tiers, and rate limits change frequently - verify before integrating.

---

## 1. DEX Aggregator APIs

### 1.1 KyberSwap Aggregator

| Field | Detail |
|---|---|
| **URL** | `https://aggregator-api.kyberswap.com/{chain}/api/v1/routes` + `POST /route/build` |
| **What it gives** | Two-step: GET route summary → POST calldata. Returns `encodedSwapData` ready for the KyberSwap router. Supports split routes, RFQ sources, gas estimation. Chain identifier for Arbitrum: `arbitrum` |
| **Pricing** | Free with `x-client-id` header; stricter limits without it. Enterprise plan via BD contact |
| **Rate limits** | Not publicly documented; explicitly states tighter limits without client ID. Recommend >5 RPS business contact |
| **Latency** | Route API designed to be "performant and real-time"; cache <5–10 seconds per docs |
| **Arbitrum support** | ✅ Yes (`chainId: 42161`, path param `arbitrum`) |
| **Integration** | REST API. GET `/routes` → POST `/route/build` → submit calldata. SDK available |
| **Calldata returned** | ✅ Yes - `encodedSwapData` + `routerAddress` in `POST /route/build` response |
| **Used by top CoW solvers** | Likely yes - KyberSwap appears in known solver codebases as a liquidity source |
| **Notes** | Includes RFQ sources (1inch Limit Order, etc.) in V1 routes. `excludeRFQSources` flag if you want pure AMM only. `enableGasEstimation` flag runs `eth_estimateGas` internally |

---

### 1.2 0x Swap API (v2)

| Field | Detail |
|---|---|
| **URL** | `https://api.0x.org/swap/allowance-holder/quote` (v2) |
| **What it gives** | Single-call quote returning unsigned Ethereum transaction object (`.transaction.to`, `.transaction.data`, `.transaction.gas`). Pulls from 150+ AMMs + private RFQ market makers (0x_RFQ). Returns `tokenMetadata` including buy/sell tax BPS - critical for fee-on-transfer detection |
| **Pricing** | Free tier: ~5 RPS. Paid plans: custom pricing via `https://0x.org/pricing` |
| **Rate limits** | 5 RPS on free tier, fixed 1-second windows |
| **Latency** | Sub-second typical; aggregates AMM + RFQ in parallel |
| **Arbitrum support** | ✅ Yes (`chainId=42161`) |
| **Integration** | REST. Single GET with `chainId`, `sellToken`, `buyToken`, `sellAmount`, `taker`. Headers: `0x-api-key`, `0x-version: v2` |
| **Calldata returned** | ✅ Yes - `transaction.data` is ready-to-use calldata |
| **Used by top CoW solvers** | **Yes** - 0x is widely used in the CoW solver ecosystem as a pricing and execution source |
| **Notes** | Also returns `tokenMetadata.buyToken.buyTaxBps` - use this to flag fee-on-transfer tokens. Dashboard at `dashboard.0x.org`. RFQ quotes embedded silently. Use `Gasless API` if you want meta-tx. Supports `includedSources`/`excludedSources` |

---

### 1.3 ParaSwap / Velora

| Field | Detail |
|---|---|
| **URL** | `https://api.paraswap.io/prices` (GET) + `https://api.paraswap.io/transactions/{network}` (POST calldata) |
| **What it gives** | GET `/prices` returns price route; POST `/transactions` returns calldata for the Augustus router. Supports RFQ via `AugustusRFQ`, Hashflow. Tax token flags via `srcTokenTransferFee`/`destTokenTransferFee` |
| **Pricing** | Free tier with partner string; defaults to "anon" which charges 1 bps on all swaps. Custom partner integrations can be zero-fee |
| **Rate limits** | Not publicly stated; generous free tier |
| **Latency** | Sub-second typical |
| **Arbitrum support** | ✅ Yes (`network=42161` in path) |
| **Integration** | REST. GET `/prices` → POST `/transactions/{network}`. SDK available (`@paraswap/sdk`) |
| **Calldata returned** | ✅ Yes - POST transactions returns `data`, `to`, `value` |
| **Used by top CoW solvers** | **Yes** - ParaSwap is a documented source in CoW Protocol solver implementations |
| **Notes** | `excludeRFQ=true` flag to go AMM-only. `otherExchangePrices=true` returns competitor quotes - useful for benchmarking. Rebranded "Velora" but same API. docs at `developers.velora.xyz` |

---

### 1.4 Odos Smart Order Router

| Field | Detail |
|---|---|
| **URL** | `https://api.odos.xyz/sor/quote/v3` (POST) + `https://api.odos.xyz/sor/assemble` (POST) |
| **What it gives** | Two-step: quote returns `pathId` + output estimates; assemble returns full calldata. Unique: supports multi-token input/output (up to 6 in/out) in one atomic tx - useful for portfolio rebalances |
| **Pricing** | Public API: 1 RPS, 1,000 req/day. Enterprise: custom |
| **Rate limits** | 1 RPS public; enterprise via contact |
| **Latency** | pathId valid for 60 seconds post-quote |
| **Arbitrum support** | ✅ Yes |
| **Integration** | REST. POST `/sor/quote/v3` → POST `/sor/assemble`. JSON body |
| **Calldata returned** | ✅ Yes - assemble endpoint returns transaction calldata |
| **Used by top CoW solvers** | Likely yes for multi-token optimization scenarios |
| **Notes** | Also offers a Token Pricing API covering 1,150+ liquidity sources - useful for token price discovery on obscure tokens. Public rate limits are restrictive; enterprise is needed for production solver use |

---

### 1.5 OpenOcean

| Field | Detail |
|---|---|
| **URL** | `https://open-api.openocean.finance/v4/{chain}/quote` + `/v4/{chain}/swap` |
| **What it gives** | GET `/quote` for preview; GET `/swap` returns transaction calldata. 40+ chains, 1,000+ liquidity sources including private RFQ |
| **Pricing** | Free; contact for enterprise |
| **Rate limits** | Not publicly stated |
| **Latency** | Not stated; REST-based |
| **Arbitrum support** | ✅ Yes - Arbitrum supported (see chain docs) |
| **Integration** | REST. GET `/quote` → GET `/swap` (with `account` param) |
| **Calldata returned** | ✅ Yes - swap endpoint returns `data`, `to`, `value`, `gasPrice`, `gasLimit` |
| **Used by top CoW solvers** | Unknown |
| **Notes** | V4 API recommended. Also offers `gasPrice` endpoint. Good fallback aggregator especially for long-tail tokens. 99.9% uptime claim |

---

### 1.6 1inch Aggregation Protocol

| Field | Detail |
|---|---|
| **URL** | `https://api.1inch.dev/swap/v6.0/{chainId}/quote` + `/swap` |
| **What it gives** | Quote + calldata in one call. Sources from Uniswap, Curve, Balancer, and 1inch's own limit order protocol |
| **Pricing** | Requires API key via `portal.1inch.dev` (Cloudflare-protected). Business tier. Free dev keys available |
| **Rate limits** | Varies by plan; free tier limited |
| **Latency** | Sub-second |
| **Arbitrum support** | ✅ Yes (`chainId=42161`) |
| **Integration** | REST via 1inch Developer Portal. API key in header |
| **Calldata returned** | ✅ Yes - `/swap` endpoint returns `tx.data`, `tx.to`, `tx.value` |
| **Used by top CoW solvers** | **Yes** - 1inch is a canonical source used by CoW's own reference solver |
| **Notes** | Portal is Cloudflare-gated; key provisioning may require brief wait. 1inch also has fusion (intent-based) and limit orders which are separate products |

---

### 1.7 Li.Fi (LI.FI)

| Field | Detail |
|---|---|
| **URL** | `https://li.quest/v1/quote` |
| **What it gives** | Cross-chain AND same-chain swap quotes. Returns transaction object with calldata, includedDEXs, gas estimates. Aggregates 40+ bridges and DEXs |
| **Pricing** | Free tier available; enterprise plans at `lifi.xyz/integrators` |
| **Rate limits** | Not publicly stated |
| **Latency** | Sub-second for same-chain; higher for cross-chain routes |
| **Arbitrum support** | ✅ Yes (`fromChain=ARB`, `toChain=ARB`) |
| **Integration** | REST or SDK (`@lifi/sdk`). Single GET `/quote` for same-chain |
| **Calldata returned** | ✅ Yes - returns `transactionRequest.data` |
| **Used by top CoW solvers** | Unlikely for pure Arbitrum CoW solving (cross-chain focus) |
| **Notes** | Most useful if you're routing cross-chain orders. For single-chain Arbitrum CoW solving, other aggregators are more targeted. Docs at `docs.lifi.xyz` |

---

### 1.8 Rango Exchange

| Field | Detail |
|---|---|
| **URL** | `https://api.rango.exchange/basic/swap` (Simple API) |
| **What it gives** | Single-step: quote + calldata in one call for same-chain swaps. Multi-step API for cross-chain. Aggregates DEXs and bridges |
| **Pricing** | API key required; free tier available |
| **Rate limits** | Not publicly stated |
| **Latency** | Sub-second for same-chain |
| **Arbitrum support** | ✅ Yes |
| **Integration** | REST. Simple API or multi-step (check → create-tx). SDK available |
| **Calldata returned** | ✅ Yes |
| **Used by top CoW solvers** | Not widely documented |
| **Notes** | Less commonly used in CoW solver context; better suited for wallets/dApps. Documentation at `docs.rango.exchange` |

---

### 1.9 OKX DEX Aggregator

| Field | Detail |
|---|---|
| **URL** | `https://www.okx.com/api/v5/dex/aggregator/` (WaaS/DEX endpoints) |
| **What it gives** | Quote + calldata. OKX aggregates its own DEX, CEX liquidity, and third-party AMMs |
| **Pricing** | API key required via OKX Developer Portal; pricing unclear without account |
| **Rate limits** | Not publicly stated |
| **Latency** | Sub-second |
| **Arbitrum support** | ✅ Yes |
| **Integration** | REST with OKX API key |
| **Calldata returned** | ✅ Yes |
| **Used by top CoW solvers** | Not widely documented in CoW solver context |
| **Notes** | OKX has unique liquidity from their CEX order books potentially accessible via RFQ. Worth testing but documentation is behind their developer portal sign-up. May not be accessible from automated solver without formal partnership |

---

### 1.10 Firebird Finance

| Field | Detail |
|---|---|
| **URL** | `https://docs.firebird.finance` (docs redirect to parked domain - status unclear as of research) |
| **What it gives** | DEX aggregator focused on optimal routing across Polygon, BSC, and Arbitrum |
| **Pricing** | Free |
| **Arbitrum support** | ✅ Claimed |
| **Used by top CoW solvers** | Not documented |
| **Notes** | ⚠️ **Docs appear to be parked/redirected at time of research.** Verify API is still operational before integrating. May be inactive |

---

### 1.11 DODO Route API

| Field | Detail |
|---|---|
| **URL** | `https://docs.dodoex.io/en/developer/dodo-route` |
| **What it gives** | DODO's own routing engine. Optimized for DODO pool liquidity (PMM algorithm) plus external DEXs |
| **Pricing** | Free |
| **Arbitrum support** | ✅ Yes - DODO has Arbitrum pools |
| **Integration** | REST API |
| **Calldata returned** | ✅ Yes |
| **Used by top CoW solvers** | Not documented; worth testing for DODO-specific pair liquidity |
| **Notes** | KyberSwap already includes DODO as a pool type (`poolType: "dodo"`) so routing through KyberSwap may already capture DODO liquidity. DODO direct API useful for DODO-native pools with PMM pricing |

---

### 1.12 BitGet DEX (Web3)

| Field | Detail |
|---|---|
| **URL** | `https://web3.bitget.com/en/docs` (restricted in browser session) |
| **What it gives** | Swap aggregation from BitGet's Web3 wallet product |
| **Pricing** | Unknown |
| **Arbitrum support** | Claimed |
| **Notes** | ⚠️ Documentation inaccessible in testing. BitGet's DEX aggregator is relatively new and less mature than competitors. Low priority for CoW solver use |

---

## 2. Private / Exclusive Liquidity & RFQ Systems

### 2.1 Hashflow RFQ

| Field | Detail |
|---|---|
| **URL** | `https://hashflow.com` / `https://docs.hashflow.com` |
| **What it gives** | Cryptographically signed quotes from 25+ institutional market makers. Zero slippage, MEV-protected. Quote is guaranteed at submission time. Makers sign off-chain using advanced pricing functions |
| **Pricing** | Integration via partnership/BD. Not a simple public API key signup |
| **Latency** | Quote → fill is fast (signed quotes, no AMM math) |
| **Arbitrum support** | ✅ Yes - Hashflow is multi-chain |
| **Integration** | REST API (taker-side). Request quote, receive signed order, submit to Hashflow settlement contract |
| **Accessible to small solvers?** | ⚠️ **Requires formal integration partnership.** Not a self-serve API key. Contact Hashflow BD |
| **Used by top CoW solvers** | **Yes** - Hashflow is a documented RFQ source in CoW Protocol. ParaSwap also routes through Hashflow |
| **Notes** | Hashflow's $25B+ in RFQ volume makes it significant. 0x Swap API and KyberSwap V1 both already route through Hashflow - so calling these aggregators you get Hashflow implicitly |

---

### 2.2 Bebop RFQ / JAM

| Field | Detail |
|---|---|
| **URL** | `https://docs.bebop.xyz` |
| **What it gives** | JAM (Just-in-time Automated Market) - institutional-grade RFQ with multi-token support (any-to-any). Bebop connects to market makers for custom pricing. Also offers a JAM Settlement contract for complex interactions |
| **Pricing** | Contact Bebop for integration. Not fully self-serve |
| **Latency** | Sub-second for RFQ quotes |
| **Arbitrum support** | ✅ Yes |
| **Integration** | REST API + JAM SDK |
| **Accessible to small solvers?** | ⚠️ Requires integration with Bebop. Reachable via `@bebop_dex` on Twitter |
| **Used by top CoW solvers** | Known to be a liquidity source for solver competition |
| **Notes** | Bebop's any-to-any capability is uniquely valuable for CoW batch settlements where you might have 3+ token pairs to settle simultaneously |

---

### 2.3 AirSwap RFQ

| Field | Detail |
|---|---|
| **URL** | `https://airswap.io` / `https://github.com/airswap/airswap-protocols` |
| **What it gives** | Peer-to-peer RFQ. Market makers register servers; takers request quotes via HTTP (the "Request for Quote" protocol). Fully permissionless - anyone can be a maker or taker |
| **Pricing** | Free - protocol is open-source |
| **Latency** | Depends on maker response time; typically <500ms |
| **Arbitrum support** | ✅ Yes - AirSwap contracts deployed on Arbitrum |
| **Integration** | HTTP RFQ: discover maker URLs, request quote, receive signed order, submit to AirSwap settlement |
| **Accessible to small solvers?** | ✅ **Yes, fully permissionless.** Can query maker servers directly |
| **Used by top CoW solvers** | Less common; smaller liquidity depth than Hashflow/Bebop |
| **Notes** | AirSwap maker discovery is via a registry of HTTP endpoints. Lower liquidity than Hashflow but fully accessible. Good for niche token pairs where a specific maker has inventory |

---

### 2.4 0x RFQ (Native to 0x Swap API)

| Field | Detail |
|---|---|
| **URL** | Embedded in `https://api.0x.org/swap/allowance-holder/quote` |
| **What it gives** | 0x routes through its own RFQ market maker network silently. When you call the 0x Swap API, if an RFQ maker provides a better quote than the AMM route, it's included automatically (visible in `fills[].source = "0x_RFQ"`) |
| **Pricing** | Included in 0x Swap API free tier |
| **Accessible to small solvers?** | ✅ **Yes** - implicit via the Swap API |
| **Notes** | You get 0x RFQ "for free" by using the 0x Swap API. No separate integration needed |

---

### 2.5 Native (native.org)

| Field | Detail |
|---|---|
| **URL** | `https://native.org` |
| **What it gives** | RFQ-based liquidity aggregation focused on DeFi protocols |
| **Pricing** | Contact |
| **Arbitrum support** | Unknown from public docs |
| **Accessible to small solvers?** | Unclear - likely partnership required |
| **Notes** | Less prominent; lower priority than Hashflow/Bebop/0x RFQ |

---

### 2.6 Wintermute OTC / Jump Trading OTC

| Field | Detail |
|---|---|
| **Notes** | **Not accessible via self-serve API.** These are institutional OTC desks. As a small solver you cannot access Wintermute or Jump directly without a formal institutional relationship and large minimum trade sizes. Skip unless you have existing institutional connections |

---

## 3. Real-Time Price & Liquidity Feeds

### 3.1 The Graph - Subgraphs for Arbitrum DEXs

| Field | Detail |
|---|---|
| **URL** | `https://thegraph.com/studio/` / Graph Explorer |
| **What it gives** | GraphQL APIs over indexed DEX data. Key Arbitrum subgraphs: Uniswap V3 Arbitrum, Camelot DEX, Balancer Arbitrum, SushiSwap. Query pool states, tick data, reserves, swap history |
| **Pricing** | Free: 100,000 queries/month. Growth Plan: usage-based with GRT or credit card |
| **Latency** | ⚠️ **1–3 block delay typical** (~3–12 seconds on Arbitrum). Not suitable for real-time pool state during a 25-second solve window |
| **Arbitrum support** | ✅ Yes - many Arbitrum subgraphs available |
| **Integration** | GraphQL HTTP POST. API key from Subgraph Studio |
| **Notes** | **Not recommended for real-time pool state** during solving. Use for off-peak analytics, token metadata, historical liquidity depth analysis. Better suited for building your internal token database than live pricing |

---

### 3.2 Goldsky

| Field | Detail |
|---|---|
| **URL** | `https://goldsky.com` / `https://docs.goldsky.com` |
| **What it gives** | Five products: **Subgraphs** (GraphQL, like The Graph), **Mirror** (stream to your own DB with <1s latency), **Turbo** (streaming pipelines), **Edge RPC** (high-performance EVM RPCs), **Compose** (onchain/offchain workflows) |
| **Pricing** | Contact for pricing; not public self-serve for Mirror |
| **Latency** | **Mirror: <1 second** - this is the key differentiator. Stream pool events directly to Postgres/ClickHouse |
| **Arbitrum support** | ✅ Yes - 130+ chains supported |
| **Integration** | Mirror: YAML pipeline config streams to your DB. Subgraphs: GraphQL. Edge RPC: standard JSON-RPC |
| **Used by top CoW solvers** | Plausible for sophisticated solver teams |
| **Notes** | **Mirror is the standout product for solvers.** Stream Uniswap V3/Camelot swap events into your own ClickHouse DB in <1s. Query locally with zero RPC overhead. Pipeline is reorg-aware. Requires contact for pricing/access |

---

### 3.3 Pyth Network

| Field | Detail |
|---|---|
| **URL** | `https://docs.pyth.network` / Hermes API: `https://hermes.pyth.network` |
| **What it gives** | Pull oracle - publish prices on-chain on demand. Off-chain: Hermes aggregates prices from 80+ data publishers (exchanges, market makers). Latency: ~400ms from Pythnet to EVM. Price + confidence interval. Covers 400+ price feeds |
| **Pricing** | Free for on-chain integration; Hermes API (off-chain) is free |
| **Latency** | **~400ms end-to-end** from source data to available price update. Hermes REST: sub-200ms for current price |
| **Arbitrum support** | ✅ Yes - Pyth deployed on Arbitrum |
| **Integration** | REST/WebSocket via Hermes (`GET /api/latest_price_feeds?ids[]=...`). For on-chain: call `updatePriceFeeds()` with Wormhole VAA |
| **Used by top CoW solvers** | Useful as reference price for fair-value estimation |
| **Notes** | Good for fast reference pricing of major assets (ETH, BTC, stables). Not useful for pool-level liquidity state. Use alongside DEX quotes, not instead of them |

---

### 3.4 Chainlink Price Feeds

| Field | Detail |
|---|---|
| **URL** | `https://docs.chain.link/data-feeds` |
| **What it gives** | On-chain price feeds (push model). Arbitrum has Chainlink aggregators for ETH/USD, BTC/USD, ARB/USD, LINK/USD, and more. Off-chain: read aggregator contracts via RPC |
| **Pricing** | Free to read on-chain |
| **Latency** | ⚠️ **Updated every heartbeat (1 hour) or 0.5% deviation** - not real-time enough for solver use |
| **Arbitrum support** | ✅ Yes |
| **Integration** | Read `latestRoundData()` on aggregator contract |
| **Notes** | **Not suitable as your primary price source during solving** - too infrequent. Use as a sanity check / fair-value reference to detect bad quotes from aggregators. Can help flag suspicious prices |

---

### 3.5 RedStone

| Field | Detail |
|---|---|
| **URL** | `https://docs.redstone.finance` |
| **What it gives** | Modular oracle with two models: **Push** (on-chain, like Chainlink) and **Pull** (inject price data into your transaction calldata - uniquely gas-efficient). Covers 1,000+ assets including LRT, RWA, BTCFi |
| **Pricing** | Free for open feeds; contact for custom feeds |
| **Latency** | Push model: depends on heartbeat. Pull model: real-time (you fetch from RedStone nodes and include in tx) |
| **Arbitrum support** | ✅ Yes - 70+ chains |
| **Integration** | SDK for pull model (`@redstone-finance/evm-connector`). Standard ABI for push |
| **Notes** | Most useful for exotic token pricing where Chainlink/Pyth don't have feeds. If you encounter a token with no major oracle coverage, RedStone likely has a feed |

---

### 3.6 Blocknative Gas Platform

| Field | Detail |
|---|---|
| **URL** | `https://api.blocknative.com/gasprices/blockprices` |
| **What it gives** | Real-time gas price estimates for 40+ chains. Delivers 600+ gas estimates/second. Returns predicted base fee, priority fee at various confidence levels (70%, 80%, 95%, 99%) for next N blocks |
| **Pricing** | API key required via `blocknative.com`. Free tier available; paid plans for higher throughput |
| **Latency** | Real-time |
| **Arbitrum support** | ✅ Yes |
| **Integration** | REST GET with auth header. Response is JSON with block-level predictions |
| **Notes** | Critical for gas cost estimation in your solver objective function. On Arbitrum L2, gas costs are lower but still affect profitability calculation. Knowing the next-block base fee helps you set `gasPrice` correctly in settlement proposals |

---

### 3.7 Allium

| Field | Detail |
|---|---|
| **URL** | `https://allium.so` |
| **What it gives** | Real-time and historical blockchain data warehouse. Covers decoded DEX events, token transfers, pool states. Targets data teams and analytics |
| **Pricing** | Contact for pricing |
| **Latency** | Near-real-time (streaming pipelines); not millisecond-level |
| **Arbitrum support** | ✅ Yes |
| **Notes** | Better for analytics and building internal datasets than live solving. Lower priority than Goldsky Mirror for latency-sensitive applications |

---

### 3.8 Dune Analytics API

| Field | Detail |
|---|---|
| **URL** | `https://docs.dune.com/api-reference` |
| **What it gives** | Execute any saved DuneSQL query via API. Access CoW Protocol's own official dashboard data (`dune.com/cowprotocol/cow-protocol`). Token volumes, solver competition history, surplus data |
| **Pricing** | Free: 100,000 credits/month. Paid plans for higher usage. Execution endpoints are credit-based; metadata endpoints free |
| **Latency** | ⚠️ Query execution takes seconds to minutes. **Not suitable for real-time solving** |
| **Arbitrum support** | ✅ Yes - DuneSQL covers Arbitrum tables |
| **Integration** | REST. Execute query → poll for results → retrieve |
| **Notes** | Best use: **off-line research and analytics** - e.g., studying which solver wins what types of orders, historical surplus patterns, token liquidity profiles. Not for live solving decisions |

---

## 4. Simulation & Gas Estimation

### 4.1 Tenderly Simulation API

| Field | Detail |
|---|---|
| **URL** | `https://api.tenderly.co/api/v1/account/{slug}/project/{slug}/simulate` |
| **What it gives** | Full transaction simulation with decoded traces, logs, asset changes, revert reason, and gas used. Supports state overrides (mock balances, approvals). Can also simulate via RPC: `tenderly_simulateTransaction`. Bundled simulation (`simulateBundle`) for sequential tx chains |
| **Pricing** | Starter: $45/mo (35M TUs/month, 20 TU/s). Pro: $450/mo (350M TUs, 300 TU/s). Pro+: custom |
| **Latency** | **Sub-100ms for quick simulation**; full simulation slightly higher |
| **Arbitrum support** | ✅ Yes - Arbitrum One in supported networks list |
| **Integration** | REST POST with `X-Access-Key` header. Also via RPC URL (`tenderly_simulateTransaction` JSON-RPC method) |
| **Used by top CoW solvers** | **Yes** - CoW Protocol's own orderbook API has a `GET /api/v1/debug/simulation/{uid}` endpoint that returns a Tenderly simulation request for an order |
| **Notes** | **This is the most actionable simulation tool for CoW solvers.** The CoW orderbook debug endpoint literally returns Tenderly simulation payloads. Use simulation to pre-check settlement validity before submitting. Pro plan needed for high-frequency use in a 25-second solve window |

---

### 4.2 Alchemy Simulation APIs

| Field | Detail |
|---|---|
| **URL** | `https://eth-mainnet.g.alchemy.com/v2/{apiKey}` (via JSON-RPC methods) |
| **What it gives** | Three simulation methods: `alchemy_simulateAssetChanges` (returns token/ETH balance changes), `alchemy_simulateExecution` (decoded call traces + logs), `alchemy_simulateAssetChangesBundle` (sequential tx bundle). **Does not require API gateway changes - works on your existing Alchemy node URL** |
| **Pricing** | Included in Alchemy plans. Free tier available. Compute Units consumed per call |
| **Latency** | Sub-100ms typical |
| **Arbitrum support** | ✅ Yes - Alchemy supports Arbitrum One |
| **Integration** | JSON-RPC POST to your Alchemy node URL. Standard JSON-RPC format. No separate API needed |
| **Used by top CoW solvers** | Likely for teams already using Alchemy as their RPC provider |
| **Notes** | **Easiest to integrate if you're already using Alchemy for RPC.** `alchemy_simulateExecution` gives you full decoded traces - useful for debugging settlement reverts. `alchemy_simulateAssetChanges` is faster and cheaper if you only need balance outcome |

---

### 4.3 Blocknative Gas Estimation (Mempool Context)

| Field | Detail |
|---|---|
| **URL** | `https://api.blocknative.com/gasprices/blockprices` |
| **What it gives** | Predicted gas prices for confirmed inclusion. Not a simulation API but critical for gas cost calculation. Also has mempool monitoring (Ethereum mainnet focused) |
| **Arbitrum support** | ✅ Yes for gas prices |
| **Notes** | MEV/mempool monitoring from Blocknative is **Ethereum mainnet focused** and not directly useful for Arbitrum CoW solving - Arbitrum's mempool is private to the sequencer |

---

## 5. MEV & Order Flow Intelligence

> **Critical context for CoW on Arbitrum:** CoW Protocol handles MEV protection at the protocol level - user orders are batched off-chain and settled atomically, so individual order MEV is already mitigated. However, the **settlement transaction itself** can be front-run or sandwiched if sent to the public mempool.

### 5.1 Flashbots (MEV on Arbitrum)

| Field | Detail |
|---|---|
| **URL** | `https://docs.flashbots.net` |
| **What it gives** | Private transaction bundles, MEV-Share, SUAVE. On **Ethereum mainnet**: critical for private settlement. On **Arbitrum**: ⚠️ Flashbots MEV infrastructure **does not apply to Arbitrum** - Arbitrum uses a centralized sequencer (Offchain Labs) with no MEV bundle auction |
| **Arbitrum support** | ❌ Not applicable for settlement MEV protection |
| **Notes** | On Arbitrum, transactions go to the Offchain Labs sequencer. There is no block builder auction. Your settlement tx gets included in FCFS (first-come, first-served) order. Focus instead on **submission speed** rather than private bundles |

---

### 5.2 MEV Blocker / Private RPCs

| Field | Detail |
|---|---|
| **Notes** | MEV Blocker and similar services protect transactions from front-running on **Ethereum mainnet** via `mev-blocker.io`. **Not applicable on Arbitrum** - the sequencer is not MEV-exploitable in the traditional sense |

---

### 5.3 Pending Transaction Monitoring (Mempool Intelligence)

| Field | Detail |
|---|---|
| **Notes** | On Arbitrum, the **mempool is private** - the sequencer receives transactions directly and orders them. There is no public pending transaction mempool to monitor. Tools like Blocknative's mempool streaming, Flashbots' `eth_callBundle`, etc., work on Ethereum mainnet only. **You cannot see pending Arbitrum transactions before they are sequenced.** The only "mempool" signal available is the CoW Protocol orderbook itself |

### 5.4 Practical MEV/Speed Strategy for Arbitrum CoW

The effective strategy: submit your settlement transaction as fast as possible after the solve deadline. Use a high-quality low-latency Arbitrum RPC endpoint (Alchemy, Infura, or a private node). Your settlement tx speed to the sequencer is your only submission-level lever.

---

## 6. CoW Protocol–Specific Tools

### 6.1 CoW Orderbook API

| Field | Detail |
|---|---|
| **URL** | `https://api.cow.fi/arbitrum_one/api/v1/` |
| **What it gives** | Full orderbook operations. Key solver endpoints: `GET /auction` (current batch auction with all open orders, prices, deadline), `GET /solver_competition/{auction_id}` (who won, what solutions were submitted, scores), `GET /solver_competition/latest` (last auction results), `POST /quote` (reference price for an order), `GET /debug/simulation/{uid}` (Tenderly simulation request) |
| **Pricing** | Free |
| **Latency** | Sub-second reads |
| **Arbitrum support** | ✅ Yes - `https://api.cow.fi/arbitrum_one/` is a dedicated endpoint |
| **Integration** | REST. OpenAPI spec at `https://raw.githubusercontent.com/cowprotocol/services/main/crates/orderbook/openapi.yml` |
| **Notes** | `GET /auction` is the heartbeat of your solver - this is what the driver sends to your solver engine. `GET /solver_competition/latest` lets you study winning solutions in real-time to improve your scoring |

---

### 6.2 CoW Solver API (Driver ↔ Solver Interface)

| Field | Detail |
|---|---|
| **URL** | Internal to your solver deployment; defined in `https://raw.githubusercontent.com/cowprotocol/services/main/crates/solvers/openapi.yml` |
| **What it gives** | The two endpoints your solver engine must implement: `POST /solve` (receives auction, must return solution) and `POST /notify` (receives auction result notification). Your solver engine is a server, not a client |
| **Integration** | Your solver is an HTTP server. The CoW driver calls you. See `github.com/cowprotocol/services` for the driver binary |
| **Notes** | Run the driver binary (`ghcr.io/cowprotocol/services`) pointing at your solver URL. Staging environment: `barn.api.cow.fi/arbitrum_one` |

---

### 6.3 CoW SDK

| Field | Detail |
|---|---|
| **URL** | `https://github.com/cowprotocol/cow-sdk` |
| **What it gives** | TypeScript monorepo SDK. Key packages: `@cowprotocol/sdk-order-book` (orderbook API client), `@cowprotocol/sdk-trading` (quote + sign + post), `@cowprotocol/sdk-subgraph` (CoW Protocol subgraph queries), `@cowprotocol/sdk-composable` (TWAP, conditional orders) |
| **Pricing** | Free, open-source |
| **Integration** | npm. Works with viem or ethers.js (v5 and v6 adapters) |
| **Notes** | If you're building in TypeScript, use this. Supports Arbitrum One (`chainId: 42161`) |

---

### 6.4 Solver Competition Analytics

| Field | Detail |
|---|---|
| **URL** | `https://api.cow.fi/arbitrum_one/api/v2/solver_competition/latest` |
| **What it gives** | Full detail of the most recent batch auction: all submitted solutions, scores, winning solver, orders filled, prices used |
| **Notes** | This is your primary intelligence feed on competition. Pull this after every auction to study competitors' pricing, which liquidity sources they used (visible in `interactions`), and what scores they achieved. Build a feedback loop |

---

### 6.5 CoW Protocol Dune Dashboard

| Field | Detail |
|---|---|
| **URL** | `https://dune.com/cowprotocol/cow-protocol` |
| **What it gives** | Official CoW Protocol analytics including solver rankings, surplus by solver, order flow statistics, chain breakdowns |
| **Notes** | Monitor which solvers win on Arbitrum specifically. Track your own surplus score relative to competitors over time |

---

## 7. Token Intelligence

### 7.1 DefiLlama API

| Field | Detail |
|---|---|
| **URL** | `https://api.llama.fi` / `https://api-docs.defillama.com` |
| **What it gives** | **Token prices** (`GET /prices/current/{coins}` - supports `arbitrum:{address}` format), TVL per protocol, DEX volume, yield pools, stablecoin data. Covers 500+ protocols |
| **Pricing** | Free for most endpoints. Premium plan: $300/month for advanced endpoints (inflows, chain assets, token liquidity history) |
| **Latency** | Sub-second for price queries |
| **Arbitrum support** | ✅ Yes - use `arbitrum:0x...` coin format |
| **Integration** | REST. SDK: `@defillama/api` (npm), `defillama-sdk` (pip) |
| **Notes** | `GET /prices/current/arbitrum:0x{tokenAddress}` is extremely useful for fair-value pricing of any token with on-chain liquidity. Free, fast, no API key required for basic endpoints. Also has TVL data to assess liquidity depth of protocols |

---

### 7.2 CoinGecko API

| Field | Detail |
|---|---|
| **URL** | `https://api.coingecko.com/api/v3/` |
| **What it gives** | Token metadata (name, symbol, decimals, contract addresses per chain), market prices, trading volume, market cap, holder data, trust scores |

The response was cut off mid-section 7.2. Here is the complete remainder of the report - pick it up from 7.2 and continue through to the end:

---

### 7.2 CoinGecko API

| Field | Detail |
|---|---|
| **URL** | `https://api.coingecko.com/api/v3/` |
| **What it gives** | Token metadata (name, symbol, decimals, contract addresses per chain), market prices, trading volume, market cap, trust scores, platform-specific contract addresses |
| **Pricing** | Free (Demo API): 30 calls/min, limited endpoints. Pro: $129/month. Analyst: $499/month |
| **Latency** | Sub-second for price/metadata reads |
| **Arbitrum support** | ✅ Yes - use `asset_platform_id: arbitrum-one` in contract lookups |
| **Integration** | REST. No SDK officially but community ones exist. `GET /coins/{id}/contract/{address}` for contract metadata |
| **Notes** | Best use for solvers: **token trust scoring and metadata bootstrapping** - pull token decimals, detect rebasing tokens, check if a token is listed/legit before pricing it. Not your real-time price source during solving. Also useful for building your internal token whitelist/blacklist |

---

### 7.3 Token Lists (Uniswap Standard)

| Field | Detail |
|---|---|
| **URL** | `https://tokens.uniswap.org`, `https://tokenlist.arbitrum.io/ArbTokenLists/arbed_uniswap_labs.json` |
| **What it gives** | Curated JSON lists of trusted tokens per chain. Fields include: `chainId`, `address`, `name`, `symbol`, `decimals`, `logoURI`. Arbitrum-specific lists available |
| **Pricing** | Free |
| **Latency** | Static JSON - load once, cache |
| **Arbitrum support** | ✅ Yes - Arbitrum token lists specifically available |
| **Integration** | HTTP GET to fetch JSON. Load into your solver's token registry at startup |
| **Notes** | **Critical for solver safety.** A token not on any reputable token list is a strong signal it may be honeypot, fee-on-transfer, or rebasing. Use as a first-pass filter before applying AMM math. The Arbitrum Bridge canonical token list (`arbed_uniswap_labs.json`) is the gold standard for Arbitrum tokens |

---

### 7.4 Fee-on-Transfer / Rebasing Token Detection

| Field | Detail |
|---|---|
| **Method 1** | **0x Swap API** - returns `tokenMetadata.buyToken.buyTaxBps` and `sellTaxBps`. Non-zero = fee-on-transfer token. Actionable immediately |
| **Method 2** | **ParaSwap** - accepts `srcTokenTransferFee` and `destTokenTransferFee` params; if you supply them, it filters DEXs that can't handle tax tokens |
| **Method 3** | **On-chain simulation** - simulate a small transfer of the token and compare input vs output amount. Tenderly or Alchemy simulation can detect this |
| **Method 4** | **CoinGecko / CMC metadata** - some rebasing tokens are tagged. Not reliable as a sole signal |
| **Method 5** | **CoW Protocol orderbook** - `GET /api/v1/token/{token}/native_price` returns the CoW-computed native price; if this fails or is wildly off-market, it's a signal the token has unusual behavior |
| **Notes** | For your solver: maintain a local cache of known fee-on-transfer and rebasing tokens (e.g., AMPL, stETH, USDT with fee enabled). Refresh via 0x Swap API metadata on first encounter of any new token. **Rebasing tokens break all AMM math** - never use standard `amountOut = amountIn * reserve_ratio` for these |

---

## 8. Summary Priority Matrix

Ranked by **immediate actionability** for a new CoW Protocol solver on Arbitrum:

### Tier 1: Integrate in Week 1 (Core Solving Stack)

| Service | Why First |
|---|---|
| **CoW Orderbook API** (`api.cow.fi/arbitrum_one`) | You literally cannot solve without this. Fetch the auction, post solutions |
| **0x Swap API** | Single API key, returns calldata + RFQ + fee-on-transfer detection. Best ROI |
| **KyberSwap Aggregator** | Free, Arbitrum-native, RFQ-inclusive, calldata ready, covers 40+ DEXs |
| **Alchemy Simulation** (`alchemy_simulateExecution`) | If already using Alchemy RPC, zero extra integration. Catch reverts before submission |
| **DefiLlama Prices** | Free, no key, `arbitrum:0x{address}` - instant token price for fair-value checking |
| **Blocknative Gas API** | Real-time next-block gas prediction for accurate profitability calculation |

### Tier 2: Integrate in Week 2 (Pricing Edge)

| Service | Why |
|---|---|
| **ParaSwap/Velora** | Different routing graph than 0x/KyberSwap; triangulating quotes catches pricing gaps |
| **Odos SOR** | Multi-token I/O is unique; needed for complex batch settlements |
| **OpenOcean** | Good long-tail token coverage; fallback when others fail |
| **Tenderly Simulation API** | Richer debugging than Alchemy; CoW orderbook `/debug/simulation` returns Tenderly payloads directly |
| **Solver Competition API** | Study every winning solution in real-time to calibrate your scoring |
| **Arbitrum Token Lists** | Bootstrap your token safety registry |

### Tier 3: Integrate if Competitive Gap Emerges (Alpha Sources)

| Service | Why |
|---|---|
| **Goldsky Mirror** | Sub-1s pool state streaming to your DB - eliminates RPC polling for pool state |
| **Hashflow RFQ** | Requires BD partnership but gives exclusive better-than-AMM pricing |
| **Bebop JAM** | Exclusive multi-token RFQ - unique for complex batch settlements |
| **Pyth Network Hermes** | ~400ms reference prices for fair-value estimation |

### Tier 4: Analytics Only (Not Real-Time Solving)

| Service | Role |
|---|---|
| **The Graph subgraphs** | Historical analysis, token metadata queries |
| **Dune API** | Solver competition research, off-peak analytics |
| **CoinGecko** | Token trust scoring, metadata bootstrap |
| **RedStone** | Exotic token pricing where no other oracle exists |

---

## 9. Critical Architecture Notes for Arbitrum CoW Solvers

**On the 25-second solve window:**
- Your solver receives `POST /solve` from the CoW driver with the auction payload
- You have ~20–23 seconds to return a solution (driver uses some buffer)
- All API calls should run **in parallel**, not sequentially
- Fan out to 0x + KyberSwap + ParaSwap simultaneously for each order
- Cache pool state aggressively; refresh every block (~250ms on Arbitrum)

**On Arbitrum's sequencer model:**
- There is no public mempool - you cannot front-run or observe pending txs
- Settlement submission is FCFS to the Offchain Labs sequencer
- Use the fastest possible Arbitrum RPC (Alchemy, Infura, or your own node co-located in the AWS us-east-1/eu-west-1 regions where the sequencer runs)
- Block time is ~250ms; your solve window is ~100 blocks

**On MEV protection:**
- CoW Protocol already protects orders from MEV at the batch level
- Your settlement tx itself is not MEV-extractable in the traditional sense on Arbitrum (no public mempool)
- Flashbots / MEV Blocker / private bundles are Ethereum mainnet concepts - **do not apply on Arbitrum**

**On calldata reuse:**
- KyberSwap: pathId in GET `/routes` → use in POST `/route/build`. Route summary must be passed verbatim; do not modify
- Odos: `pathId` is valid for 60 seconds - run quote → assemble in under 60s
- 0x: the returned `transaction.data` is the complete calldata. Wrap in your settlement's `interactions` array
- All aggregator calldatas are designed for the aggregator's own router contract as `to`. In CoW settlement, you call these as **interactions** inside `GPv2Settlement.settle()`, not as top-level transactions

**On token safety checks (pre-execution):**
1. Check token against Arbitrum token lists (whitelist)
2. Check `tokenMetadata.*.buyTaxBps` in 0x response (fee-on-transfer signal)
3. Simulate a small balance change via Alchemy/Tenderly if token is unknown
4. Use `GET /api/v1/token/{token}/native_price` from CoW orderbook - if CoW can't price it, be cautious

---

## 10. Quick-Reference API Table

| Service | Base URL | Key Header/Param | Arbitrum Chain ID/Slug | Calldata? | Free Tier |
|---|---|---|---|---|---|
| CoW Orderbook | `api.cow.fi/arbitrum_one/api/v1` | - | `arbitrum_one` in path | N/A | ✅ Yes |
| 0x Swap v2 | `api.0x.org/swap/allowance-holder/quote` | `0x-api-key`, `0x-version: v2` | `chainId=42161` | ✅ Yes | ✅ 5 RPS |
| KyberSwap | `aggregator-api.kyberswap.com/arbitrum/api/v1/routes` | `x-client-id` | `arbitrum` in path | ✅ Yes (POST build) | ✅ w/ clientId |
| ParaSwap | `api.paraswap.io/prices` | `partner` | `network=42161` | ✅ Yes (POST tx) | ✅ Yes |
| Odos | `api.odos.xyz/sor/quote/v3` | - | in body | ✅ Yes (assemble) | ✅ 1 RPS |
| OpenOcean | `open-api.openocean.finance/v4/arbitrum/swap` | - | `arbitrum` in path | ✅ Yes | ✅ Yes |
| 1inch | `api.1inch.dev/swap/v6.0/42161/swap` | `Authorization: Bearer {key}` | `42161` in path | ✅ Yes | ✅ Dev key |
| Li.Fi | `li.quest/v1/quote` | - | `fromChain=ARB` | ✅ Yes | ✅ Yes |
| Hashflow | Partnership required | - | Yes | ✅ Yes | ❌ BD required |
| Bebop JAM | Partnership required | - | Yes | ✅ Yes | ❌ BD required |
| Tenderly Sim | `api.tenderly.co/api/v1/.../simulate` | `X-Access-Key` | `network_id: 42161` | N/A | ✅ Free tier |
| Alchemy Sim | `{alchemy-arb-url}/v2/{key}` | in URL | Arbitrum One URL | N/A | ✅ Yes |
| Blocknative Gas | `api.blocknative.com/gasprices/blockprices` | `Authorization` | `?chainid=42161` | N/A | ✅ Free tier |
| DefiLlama | `api.llama.fi/prices/current/arbitrum:{addr}` | - | `arbitrum:` prefix | N/A | ✅ No key needed |
| Pyth Hermes | `hermes.pyth.network/api/latest_price_feeds` | `ids[]` | On any EVM | N/A | ✅ Yes |
| The Graph | `gateway.thegraph.com/api/{key}/subgraphs/id/{id}` | `api-key` | Arbitrum subgraphs | N/A | ✅ 100K/mo |
| Goldsky Mirror | Contact for access | - | 130+ chains | N/A | ❌ Paid |
| Dune API | `api.dune.com/api/v1/query/{id}/execute` | `x-dune-api-key` | DuneSQL | N/A | ✅ Free tier |

---

> **Bottom line:** Start with the CoW Orderbook API + 0x Swap API + KyberSwap + Alchemy Simulation. These four give you a functional, competitive solver you can launch in a week. Then layer in ParaSwap, Odos, and the Solver Competition API for pricing triangulation and feedback-loop tuning. The Goldsky Mirror + Hashflow/Bebop RFQ are your medium-term moat once you've proven the base architecture works.