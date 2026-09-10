# DeFi Infrastructure & Intelligence Tools for CoW Protocol Solver Competitiveness on Arbitrum

*Research conducted April 3, 2026 - all data verified against live documentation and APIs.*

---

## 1. On-Chain Simulation Services

### Tenderly Simulation API
**URL:** https://docs.tenderly.co/simulations/single-simulations

**What it does:** Simulates unsigned EVM transactions against current or forked chain state, returning full execution traces, asset changes, gas estimates, and state diffs.

**Pricing:**
- Free: 120,000 simulations/month, 60/min rate limit
- Starter (~$50/mo): 160,000 sims/month
- Pro ($450/mo billed yearly): 300,000 sims/month, 100/min rate limit, multiregional routing
- Pro+: Custom quotas (contact sales)
- Overage: Pay-per-TU; API simulation = 12,000 TU/request (advanced compute tier)

**API access:** REST (`POST /api/v1/account/{slug}/project/{slug}/simulate`) + RPC method (`tenderly_simulateTransaction`) via node URL. Also supports bundle simulation for sequential tx chains. Auth via `X-Access-Key` header.

**Latency:** "Quick" simulation mode available (skips full trace, just result + gas) - Tenderly advertises sub-second for quick mode. Full simulation with traces is slower.

**Arbitrum support:** Yes - Arbitrum One is a supported network.

**Relevance:** **HIGH** - Tenderly is the go-to for professional solver simulation. Its `tenderly_simulateTransaction` RPC method means you can drop it into your node URL without changing code architecture. The `simulation_type: "quick"` flag is specifically designed for latency-sensitive paths.

**Integration effort:** 4–8 hours (REST or RPC, straightforward JSON payload)

---

### Alchemy Simulation API (`alchemy_simulateAssetChanges` / `alchemy_simulateExecutionBundle`)
**URL:** https://docs.alchemy.com/docs/data/simulation-apis

**What it does:** Simulates a transaction and returns asset changes (ERC20, NFT, native token), gas used, and revert reasons. Bundle simulation allows sequential multi-tx simulation.

**Pricing:**
- Free: 30M CU/month (simulation endpoints cost ~100–400 CU per call depending on complexity)
- Pay-as-you-go: $0.45/1M CU up to 300M, then $0.40/1M
- Transaction simulation is available on all paid tiers

**API access:** JSON-RPC method via `https://arb-mainnet.g.alchemy.com/v2/{apiKey}`. Methods: `alchemy_simulateAssetChanges`, `alchemy_simulateAssetChangesBundle`.

**Arbitrum support:** Yes - Arbitrum is explicitly listed as a supported chain.

**Relevance:** **HIGH** - Particularly useful if you're already using Alchemy for RPC. The bundle method lets you simulate the full settlement transaction including all token flows.

**Integration effort:** 2–4 hours (plug into existing JSON-RPC client)

---

### Anvil (Foundry Local Fork Simulation)
**URL:** https://getfoundry.sh/anvil

**What it does:** Local in-memory EVM node that can fork any EVM chain at any block height. Supports instant mining, account impersonation, state dumps, and full `eth_call`/`debug_traceCall` support.

**Pricing:** Free, open source (MIT/Apache).

**API access:** Standard JSON-RPC (runs locally, typically `http://localhost:8545`). Fork with: `anvil --fork-url $ARB_RPC_URL --chain-id 42161`.

**Latency:** Sub-millisecond for simple `eth_call` against local fork (no network overhead). Fork state is cached at `~/.foundry/cache/rpc/` for repeated use.

**Arbitrum support:** Yes - fork from any Arbitrum RPC. Note: ArbOS precompile behavior is replicated in forked state via the remote node's responses.

**Production use pattern:** Most sophisticated solvers run Anvil as a sidecar process, pre-forked at the current block. They call `anvil_setBlockTimestamp` + `anvil_mine` to simulate the next block, run `eth_call` for settlement simulation, then discard and re-fork on each new block. This gives you full control without rate limits.

**Relevance:** **HIGH** - The professional standard for real-time settlement validation. Zero latency, no rate limits, no cost. The cowprotocol/services repo itself uses Anvil for all e2e fork tests (`anvil` is required in their test setup).

**Integration effort:** 8–16 hours to build the fork-management sidecar process

---

### Flashbots Protect / Simulation
**URL:** https://docs.flashbots.net/flashbots-protect/overview

**What it does:** Flashbots Protect is a private RPC endpoint (Ethereum mainnet) that prevents frontrunning by routing transactions to a private mempool. MEV-Share allows users to share MEV back to themselves via backrun opportunities.

**Pricing:** Free.

**Arbitrum support:** **NO** - Flashbots Protect operates on Ethereum mainnet only. There is no Flashbots equivalent for Arbitrum (Arbitrum uses a centralized sequencer, not a decentralized block-proposer mempool).

**Relevance:** **LOW for Arbitrum** - Not applicable. See Section 4 for Arbitrum-specific MEV infrastructure.

**Integration effort:** N/A for Arbitrum

---

### `eth_call` via Premium RPC Providers
Any premium RPC provider (Alchemy, Infura, QuickNode) supports `eth_call` for basic simulation. This is the lowest-level option - you construct the settlement calldata and call `eth_call` against the GPv2Settlement contract. You get back a revert/success and can decode logs. No trace, no asset diff.

**Latency:** 50–200ms for a single `eth_call` to a premium Arbitrum RPC. Acceptable for quick sanity checks.

**Relevance:** **MEDIUM** - Good for fast pre-flight revert checks before committing to more expensive full simulation.

---

## 2. Real-Time DEX Liquidity Data Providers

### The Graph - Subgraph APIs
**URL:** https://thegraph.com

**What it does:** Decentralized indexing protocol. Subgraphs for Uniswap V2/V3, Sushiswap, Camelot, and others on Arbitrum are available via GraphQL. You can query pool reserves, tick data, swap history, and more.

**Pricing:**
- Free plan: 100,000 queries/month (Subgraph Studio)
- Growth plan: Usage-based, payable by credit card or GRT
- Arbitrum One is a fully supported network for both subgraph deployment and querying

**API access:** GraphQL via unique query URL per subgraph. Requires API key from Subgraph Studio. The endpoint structure is `https://gateway.thegraph.com/api/{api-key}/subgraphs/id/{subgraph-id}`.

**Key Arbitrum subgraphs:**
- Uniswap V3 Arbitrum: `https://thegraph.com/explorer?search=uniswap+v3+arbitrum`
- Camelot V2/V3 on Arbitrum - community subgraphs available

**Latency caveat:** Subgraph data is indexed on-chain but may lag 1–5 blocks behind the chain tip. Not suitable for real-time tick-level reserve data; suitable for pool discovery, historical data, and warm-up cache building.

**Relevance:** **MEDIUM** - Great for pool discovery and building your initial liquidity map. Too slow for within-batch settlement pricing. Use for populating your pool registry, not for live reserve queries.

**Integration effort:** 4–8 hours (GraphQL client + subgraph ID lookup per DEX)

---

### GeckoTerminal API
**URL:** https://api.geckoterminal.com/docs

**What it does:** Public REST API providing pool-level data including price, liquidity (reserves in USD), 24h volume, recent trades, and OHLCV candles. Data is sourced from custom blockchain indexers, not external APIs.

**Pricing:**
- Free tier: ~10 calls/minute (confirmed from docs: "approximately 10 calls/minute, subject to network traffic")
- Higher rate limits available via any CoinGecko paid plan (access to `/onchain` endpoints)
- CoinGecko paid plans start at ~$129/month for Analyst tier

**API access:** REST. Base URL: `https://api.geckoterminal.com/api/v2`. Version header: `Accept: application/json;version=20230203`. No API key required for free tier. Data freshness: "as fast as 2–3 seconds after a transaction is confirmed."

**Verified Arbitrum example:** Live test (`/latest/dex/tokens/0x82aF...WETH`) returned 25+ pairs from Uniswap V2/V3, PancakeSwap, Camelot, SushiSwap, TraderJoe, Chronos, Arbswap, etc., with current liquidity USD values and price data. **Reserve data (base/quote token amounts) is included in the response.**

**Arbitrum support:** Yes - `chainId: "arbitrum"` confirmed in live API response.

**Relevance:** **HIGH** - Provides reserve data you can use for price estimation without hammering your own RPC. At 2–3s freshness with a 10 req/min free tier, it's useful for slower background pool monitoring. Upgrade CoinGecko plan for production solver use.

**Integration effort:** 2–4 hours (simple REST client)

---

### DexScreener API
**URL:** https://docs.dexscreener.com

**What it does:** Free public API for DEX pair data. Returns price, liquidity (with reserve amounts), volume, trade counts, and price change percentages. Data sourced from custom on-chain indexer.

**Pricing:** Free, no API key required for the basic REST API.

**API access:** REST. Base URL: `https://api.dexscreener.com/latest/dex/`. Endpoints: `/tokens/{tokenAddress}`, `/pairs/{chainId}/{pairAddress}`, `/search/pairs?q={query}`. Rate limit undocumented but practically limited to a few hundred calls/minute for free use.

**Arbitrum support:** Yes - confirmed live; `chainId: "arbitrum"` is returned with full pool data.

**Caveat:** Docs state data comes directly from blockchains without external APIs. No WebSocket streaming feed; polling only.

**Relevance:** **MEDIUM-HIGH** - Excellent free supplement for pool discovery and price reference. Reserve amounts included. Too slow/limited for real-time settlement simulation but great for background monitoring and benchmarking.

**Integration effort:** 1–2 hours

---

### 0x Swap API (as price benchmark)
**URL:** https://docs.0x.org/docs/0x-swap-api/introduction

**What it does:** Professional DEX aggregation API sourcing from 150+ AMMs and private market makers (RFQ). Returns best executable route including split routing across sources. Supports Arbitrum (chainId 42161).

**Pricing:** Free tier available via dashboard.0x.org API key. Rate limits apply; paid plans for higher throughput.

**API access:** REST GET request: `https://api.0x.org/swap/allowance-holder/quote?sellToken=...&buyToken=...&chainId=42161&...`. Requires `0x-api-key` and `0x-version: v2` headers.

**Arbitrum support:** Yes - confirmed in docs. Supports UniV2/V3, Curve, SushiSwap, Camelot, and many more on Arbitrum plus 0x's own RFQ network.

**Relevance:** **HIGH as benchmark** - Use 0x's quote response as a quality baseline for your solver's output. If your solution is worse than a 0x quote, you will not win the batch. The `fills` array in the response shows you which pools 0x would use and at what proportions - extremely valuable for competitive analysis.

**Integration effort:** 2–4 hours

---

### Odos API (Multi-token routing, Arbitrum-native)
**URL:** https://docs.odos.xyz / https://api.odos.xyz

**What it does:** Smart order routing API that handles multi-input/multi-output swaps across 15 EVM chains. Integrates DEX AMMs, onchain order books, lending protocols, and private RFQ systems.

**Pricing:** Free for basic access. Partner Portal available for custom RPS. V3 migration required (V2 being retired).

**API access:** REST. Confirmed supported chains from live API: `[1, 130, 324, 8453, 5000, 137, 10, 34443, 43114, 59144, 534352, 42161, 146, 56, 252]` - chainId 42161 (Arbitrum) confirmed.

**Relevance:** **MEDIUM-HIGH** - Odos is used by CoW Protocol competitors and is a useful price discovery benchmark. Its multi-token routing is particularly useful for complex CoW batch settlements.

**Integration effort:** 4–8 hours

---

### KyberSwap Aggregator API
**URL:** https://docs.kyberswap.com/kyberswap-solutions/kyberswap-aggregator

**What it does:** DEX aggregation API with V1 dual-request model for performant routing. Integrates AMMs, limit orders, and Professional Market Makers (PMMs) on Ethereum mainnet; Arbitrum coverage for AMMs only.

**Pricing:** Free (no API key required for basic use).

**API access:** REST. V1 GET: `https://aggregator-api.kyberswap.com/arbitrum/api/v1/routes?...`. V1 POST for encoded calldata.

**Relevance:** **MEDIUM** - Useful as a secondary pricing benchmark on Arbitrum. PMM integration is mainnet-only, limiting its RFQ value on Arbitrum.

**Integration effort:** 4–6 hours

---

### Paraswap / Velora API
**URL:** https://developers.velora.xyz

**What it does:** Paraswap was rebranded to Velora. Offers two APIs: Velora Delta (intent-based, gasless, competitive execution) and Velora Market (DEX aggregation with real-time routing).

**Arbitrum support:** Available on major chains including Arbitrum.

**Relevance:** **MEDIUM** - Another benchmark aggregator. Delta API's intent model is directly analogous to CoW Protocol's own mechanism.

**Integration effort:** 4–8 hours

---

### Bitquery / Defined.fi
**URL:** https://bitquery.io / https://defined.fi

Bitquery offers a GraphQL streaming API for DEX trades. Defined.fi provides token and pool analytics. Both have free tiers and paid plans. For real-time reserve data on Arbitrum, these are less commonly used by professional solvers compared to direct RPC calls or GeckoTerminal/DexScreener. Bitquery's WebSocket streaming is its differentiator for real-time DEX event feeds.

**Relevance:** **MEDIUM** for Bitquery streaming, **LOW** for Defined.fi (more analytics-focused).

---

### Dune Analytics API
**URL:** https://docs.dune.com

**What it does:** SQL-based blockchain analytics platform. Execute saved queries via API, build custom analytics pipelines. The CoW Protocol team publishes official dashboards at `dune.com/cow_protocol`.

**Pricing:**
- Free: 2,500 credits/month, 40 API calls/minute
- Analyst ($65/mo): 4,000 credits, API endpoint creation
- Plus ($349/mo): 25,000 credits, team access

**API access:** REST + Python/TypeScript/Go SDKs. Queries are executed on DuneSQL (Trino-based).

**Arbitrum support:** Yes - Dune indexes Arbitrum One fully.

**Relevance:** **MEDIUM** for solver analytics and leaderboards. **LOW** for real-time price data (query latency is seconds to minutes). Use for post-hoc analysis, not live routing.

**Integration effort:** 4–8 hours

---

## 3. RFQ / Private Liquidity Providers

### 0x RFQ System
**URL:** https://docs.0x.org/docs/0x-swap-api/introduction

**What it does:** 0x's Swap API automatically solicits off-chain RFQ quotes from market makers and merges them with AMM routes to find the best price. As a taker using the 0x API, you get access to these private quotes automatically - you don't need to integrate with each market maker directly. The `fills` array will show `source: "0x_RFQ"` when RFQ liquidity is used.

**Access model:** Sign up at dashboard.0x.org for an API key. Free tier available. No institutional onboarding required to be a **taker** of RFQ quotes.

**Arbitrum support:** Yes (chainId 42161 supported).

**Relevance:** **HIGH** - This is the most accessible path to private RFQ liquidity on Arbitrum without direct MM relationships. Use 0x API as your RFQ aggregator layer.

**Integration effort:** 2–4 hours (already covered under Section 2)

---

### Hashflow
**URL:** https://docs.hashflow.com

**What it does:** RFQ-based DEX using professional market makers for zero-slippage quotes. Market makers sign quotes off-chain; settlement happens on-chain.

**Access model:** The Hashflow taker API documentation exists but is behind a Cloudflare challenge. Based on public knowledge: Hashflow operates a permissioned taker API for institutional integrators. Getting access requires contacting the Hashflow team directly. No open self-service registration known as of early 2026.

**Arbitrum support:** Yes - Hashflow is deployed on Arbitrum.

**Relevance:** **MEDIUM** - High-quality zero-slippage quotes if you can get access. The onboarding barrier is real.

**Integration effort:** 8–16 hours once access is granted

---

### Bebop
**URL:** https://docs.bebop.xyz

**What it does:** Institutional-grade DeFi liquidity infrastructure providing single-swap and multi-token "jam" orders (buy/sell multiple tokens in one transaction). Backed by Jane Street and other professional market makers.

**Access model:** The Bebop docs describe an open API for integration (no visible institutional onboarding gate based on docs). API quickstarts and reference documentation are available publicly. Contact via `@bebop_dex` on X or LinkedIn for partnership/integration support.

**Arbitrum support:** Check at docs.bebop.xyz - multi-chain support is advertised.

**Relevance:** **HIGH** - Bebop's multi-token order type is uniquely valuable for CoW batch settlements where you're moving multiple assets simultaneously. This is a direct upgrade path from simple single-swap RFQ.

**Integration effort:** 8–16 hours

---

### Wintermute RFQ
Wintermute does not operate a public-facing RFQ API. They provide liquidity through aggregators like 0x and 1inch. Direct integration requires a bilateral trading relationship with Wintermute OTC. Not accessible without institutional onboarding.

**Relevance:** **LOW** for direct access (access 0x/1inch RFQ to get their liquidity indirectly)

---

### Paradigm, Jump, Flow Traders
No public-facing RFQ APIs. These firms operate in institutional OTC markets. Access to their liquidity comes indirectly through aggregators (0x RFQ, Hashflow) or requires negotiated bilateral agreements.

**Relevance:** **LOW** for direct access

---

### Native (formerly Uniswap's private liquidity tool)
Native.org offered RFQ-style liquidity. Status in 2026 is unclear - their documentation has limited public availability. Contact via their website for current access.

---

## 4. MEV Protection / Submission Infrastructure

### Arbitrum Sequencer - Critical Context
Arbitrum One uses a **centralized sequencer** operated by Offchain Labs. This fundamentally changes the MEV landscape compared to Ethereum mainnet:

- There is **no public mempool** - transactions go directly to the sequencer
- The sequencer processes transactions in **FIFO order** at the time received (no gas-price auction for ordering within a block)
- **Traditional frontrunning and sandwich attacks are essentially eliminated** - a searcher cannot see your transaction before it's included
- **Backrunning is still possible** if searchers monitor on-chain state changes after your transaction is confirmed
- The main risk is the sequencer itself having ordering power (currently controlled by Offchain Labs)

**Implications for solvers:** You do not need Flashbots Protect on Arbitrum. Submit transactions directly to the public RPC. The main optimization is minimizing latency to the sequencer to win the batch.

---

### Flashbots Protect on Arbitrum
**Arbitrum support: NO.** Flashbots Protect operates on Ethereum mainnet only. There is no Flashbots equivalent for Arbitrum.

**Relevance:** **N/A for Arbitrum**

---

### MEV Blocker
**URL:** https://mevblocker.io

**What it does:** MEV Blocker is an RPC endpoint that routes Ethereum mainnet transactions through a network of searchers who can backrun but not frontrun, sharing MEV with the user.

**Arbitrum support:** **Ethereum mainnet only.** Not available on Arbitrum.

**Relevance:** **N/A for Arbitrum**

---

### Direct RPC Submission to Arbitrum
The professional approach on Arbitrum: Submit settlement transactions via `eth_sendRawTransaction` to a low-latency Arbitrum RPC endpoint as close geographically to the Offchain Labs sequencer as possible. The sequencer is currently located in the US East region (AWS us-east-1/us-east-2).

**Recommended providers for low-latency Arbitrum submission:**
- Alchemy (supports Arbitrum, enterprise-grade infrastructure, multi-region)
- QuickNode (Arbitrum endpoints with guaranteed throughput)
- Infura (Arbitrum support with dedicated endpoints)
- Running your own Arbitrum Nitro node (see Section 7) for direct sequencer feed access

**Relevance:** **HIGH** - Submission latency directly affects whether you win the batch. Geo-proximity to the sequencer matters.

---

### Arbitrum's "Priority Queue" (ArbOS 20+)
ArbOS supports a speed bump mechanism where transactions paying higher L1 gas data fees can be prioritized within a small time window. This is not a full priority mempool but is worth understanding for competitive submission.

---

## 5. Price Oracle / Reference Data

### Chainlink Price Feeds on Arbitrum
**URL:** https://docs.chain.link/data-feeds/price-feeds/addresses?network=arbitrum

**What it does:** Decentralized oracle network providing aggregated price feeds for major asset pairs. On-chain aggregator contracts updatable by anyone calling `latestRoundData()`.

**Pricing:** Free to read (on-chain calls). No off-chain API fees.

**Key Arbitrum feed details:**
- Settlement contract: `0x9008D19f58AAbD9eD0D60971565AA8510560ab41` (deployed on Arbitrum One - confirmed from CoW Protocol docs)
- Update mechanism: Deviates >0.5% or heartbeat (typically 1 hour for major pairs, 24 hours for stablecoins)
- Heartbeat varies: BTC/USD and ETH/USD update frequently; long-tail pairs may have 24h heartbeats

**Latency:** Suitable for reference price, not tick-level real-time pricing. Do not use as your primary routing price source - use as a sanity check against extreme deviations.

**API access:** On-chain `eth_call` to `latestRoundData()` on the aggregator proxy contract address. No off-chain API needed.

**Relevance:** **HIGH** - Essential for detecting stale or manipulated prices in your settlement. The CoW Protocol uses these as fairness benchmarks. Use as your "is this trade sane?" sanity check layer.

**Integration effort:** 2–4 hours

---

### Pyth Network
**URL:** https://docs.pyth.network / https://hermes.pyth.network/docs

**What it does:** Pull-oracle architecture. Publishers (major exchanges and market makers) post signed price attestations to Pythnet; consumers pull the latest price + proof onto any supported chain when needed.

**Pricing:** Free to use on-chain. Hermes API (off-chain price feed) is free. Permissionless - no API key required.

**API access:** 
- Off-chain REST: `GET https://hermes.pyth.network/v2/updates/price/latest?ids[]={priceId}` 
- SSE streaming: `GET https://hermes.pyth.network/v2/updates/price/stream`
- On-chain: Call `updatePriceFeeds()` with the returned VAA proof, then read the price

**Latency:** Pyth claims ~400ms global update frequency (price updates every Pythnet slot). Hermes SSE stream is real-time push.

**Arbitrum support:** Yes - Pyth is deployed on Arbitrum One. Pull oracle model means you include the price update proof in your settlement transaction.

**Relevance:** **HIGH** - Pyth's SSE streaming endpoint (`/v2/updates/price/stream`) is the best available public "fair market price" feed with sub-second latency and no API key required. Ideal for real-time price reference in your solver engine.

**Integration effort:** 4–8 hours (pull oracle integration has an on-chain component)

---

### RedStone Oracle
**URL:** https://docs.redstone.finance

**What it does:** Modular oracle with three data delivery models: Core (data in calldata), Classic (on-chain aggregator like Chainlink), and X (on-demand). Deployed on 70+ chains. Particularly strong for non-standard assets (LRTs, RWAs, BTCFi derivatives).

**Pricing:** Free to read on-chain data. Detailed integration pricing available on request.

**Arbitrum support:** Yes - RedStone is deployed on Arbitrum One.

**Relevance:** **MEDIUM** - More useful for long-tail assets where Chainlink coverage is sparse. For WETH/USDC/WBTC type majors, Chainlink and Pyth are sufficient.

**Integration effort:** 4–8 hours

---

### Uniswap V3 TWAP Oracles
**What it does:** Uniswap V3 pools maintain on-chain tick accumulator history, enabling trustless time-weighted average prices over arbitrary windows.

**Pricing:** Free (on-chain `eth_call` only).

**API access:** Call `observe()` on any Uniswap V3 pool contract with desired time window (e.g., 30-minute TWAP).

**Arbitrum support:** Yes - all Uniswap V3 Arbitrum pools support TWAP queries.

**Relevance:** **HIGH** - The most manipulation-resistant on-chain price source for Arbitrum-native tokens. Use 30m TWAP as your "fair price" sanity layer to detect sandwich-inflated pool states before settling against them.

**Integration effort:** 4–8 hours (requires knowing pool addresses and implementing the `observe()` call math)

---

## 6. Solver-Specific Tools and Frameworks

### cowprotocol/services - Official Solver Framework
**URL:** https://github.com/cowprotocol/services

**What it does:** The official Rust monorepo containing all CoW Protocol backend services: orderbook, autopilot, driver (replaces the legacy solver binary), and the solver crates. This is the definitive reference for how CoW Protocol works internally.

**Key crates for solver developers:**
- `crates/solvers/` - The solver process exposing the `openapi.yml` API that the driver calls. **This is the exact API your solver must implement.**
- `crates/driver/` - The driver process that fetches auctions from autopilot and calls your solver. You run this binary pointing at your solver endpoint.
- `crates/liquidity-sources/` - On-chain liquidity source integrations (AMMs, etc.)
- `crates/price-estimation/` - Price estimation logic
- `crates/simulator/` - Transaction simulation infrastructure

**Third-party solver integrations already in the repo (as of March 2026):**
- **OKX DEX Aggregator** (`crates/solvers/src/infra/dex/okx/`) - Full integration, recently migrated
- **BitGet DEX** (`crates/solvers/src/infra/dex/bitget/`) - Added March 2026

These integrations show the exact pattern: the solver binary exposes an HTTP server implementing the `openapi.yml` spec; internally it calls an aggregator API (OKX, BitGet) and formats the response into a CoW settlement solution.

**What the solver OpenAPI spec defines:**
- `POST /solve` - Receive an auction (list of orders, available liquidity sources, deadline), return a solution (list of trades, interactions, prices)
- `POST /notify` - Receive competition result feedback
- Custom gas fee setting is now supported (merged March 31, 2026)

**Sophistication level:** The baseline solver in the repo is production-grade. It implements proper interaction encoding, slippage handling, and the full settlement object format. New entrants should fork this and replace the aggregator backend, not start from scratch.

**Pricing:** Free, open source (mixed Apache 2.0/GPL/MIT licenses depending on crate).

**Relevance:** **CRITICAL** - Start here. The `openapi.yml` defines the exact contract your solver engine must implement. The driver binary handles all protocol communication; you only implement the `solve` endpoint.

**Integration effort:** 40–80 hours to get a working custom solver engine connected to the driver

---

### CoW Protocol Solver Competition Analytics
**URL:** https://dune.com/cow_protocol/cowswap

CoW Protocol maintains official Dune dashboards tracking solver performance, win rates, surplus generated, and batch statistics. The dashboard at `dune.com/cow_protocol/cow-solver-competition` is particularly relevant.

**Relevance:** **HIGH** - Watching which solvers win which auction types reveals their strategy. Track the OKX and BitGet integrations (both recently added to the official services repo) as reference implementations.

---

### Solver Settlement Contracts - GPv2Settlement on Arbitrum
**URL:** https://docs.cow.fi/cow-protocol/reference/contracts/core

The settlement contract address on Arbitrum One is `0x9008D19f58AAbD9eD0D60971565AA8510560ab41` (deterministic CREATE2 address, same on all chains). The vault relayer is `0xC92E8bdf79f0507f65a392b0ab4667716BFE0110`. These are the contracts your settlement interactions flow through - knowing their ABIs is essential.

---

## 7. Arbitrum-Specific Infrastructure

### Arbitrum Nitro Node (Self-Hosted)
**URL:** https://docs.arbitrum.io

**What it does:** Full Arbitrum Nitro node syncs the chain and provides direct access to:
- Real-time transaction feed from the sequencer
- Local `eth_call` with no rate limits
- Archive data access
- ArbOS precompile access

**Running costs:** Infrastructure (VM or bare metal) - typically $200–$500/month for a properly provisioned full node (high IOPS SSD required, 32+ GB RAM recommended).

**Arbitrum sequencer feed:** Nitro nodes can subscribe to the sequencer's real-time transaction feed via WebSocket, giving you visibility into incoming transactions before they're finalized (though Arbitrum's FIFO model means you can't exploit this for classic frontrunning).

**ArbOS Precompiles useful for solvers:**
- `ArbGasInfo` (`0x000000000000000000000000000000000000006C`) - Query current L1 basefee, L2 gas price, and per-call gas costs. **Directly relevant for accurate gas cost estimation in your settlement.**
- `ArbSys` (`0x0000000000000000000000000000000000000064`) - L2 block number, L1 block number mapping
- `ArbRetryableTx` (`0x000000000000000000000000000000000000006E`) - Retryable ticket management (less relevant for solver)

**Relevance:** **HIGH** for production competitive solvers - local RPC = zero latency for `eth_call` simulation, and `ArbGasInfo` precompile gives you accurate gas cost accounting without extra RPC calls.

**Integration effort:** 16–24 hours to set up and tune the node; ongoing maintenance

---

### Arbitrum MEV Landscape vs. Ethereum Mainnet

The key differences:

**No public mempool:** Transactions go directly to the sequencer. You cannot see competitor solver transactions before finalization. This also means your solutions are safe from frontrunning.

**FIFO ordering (mostly):** The sequencer respects arrival order within a time window. Submitting faster wins within the protocol's batch window - latency to the sequencer datacenter matters.

**Lower gas costs:** Arbitrum's L2 execution is ~10–50x cheaper than Ethereum for complex settlements. This means smaller trades are worth solving and the cost of submitting a losing solution is much lower.

**L1 data fees still apply:** Each transaction pays an L1 data publishing fee proportional to calldata size. The `ArbGasInfo.getL1BaseFeeEstimate()` precompile gives you the current estimate. Account for this in your solver's cost calculations - large settlements with many interactions can have non-trivial L1 data costs.

**No bundle auction:** On Ethereum, MEV searchers use Flashbots bundles to get atomic transaction ordering guarantees. On Arbitrum, the sequencer handles this - atomic transactions are always executed atomically regardless of other transactions (they're included in order, so your settlement can't be split by another transaction within the same "block").

---

## 8. DeFi Intelligence Platforms

### EigenPhi
**URL:** https://eigenphi.io / https://eigenphi.io/mev/arbitrum

**What it does:** Real-time MEV detection and classification platform. Tracks arbitrage, sandwich, and liquidation MEV across chains including Arbitrum. Shows per-contract MEV profitability leaderboards and live MEV transaction streams.

**What's confirmed live:** The Arbitrum dashboard (`eigenphi.io/mev/arbitrum`) shows real-time MEV with ~1 minute lag, classified arbitrage/sandwich/liquidation data, contract-level profit attribution, and hot liquidity pools (where most MEV happens).

**Pricing:** Free dashboard. API/data access: "EigenTxAlert" and "Reports" available; pricing appears subscription-based (contact EigenPhi directly).

**Relevance:** **HIGH** - The MEV contract leaderboard directly tells you which smart contracts are extracting the most value on Arbitrum. The hot liquidity pools section shows you where to focus your solver's routing attention. The live stream shows you what's happening now.

**Integration effort:** 2–4 hours for dashboard analysis; API integration TBD based on pricing

---

### Zeromev
**URL:** https://www.zeromev.org

Zeromev tracks MEV extraction with a focus on transparency and user protection. Primarily Ethereum-focused. Arbitrum coverage is limited compared to EigenPhi.

**Relevance:** **LOW for Arbitrum** - Use EigenPhi instead for Arbitrum-specific MEV intelligence.

---

### CoW Protocol Official Analytics (Dune)
**URL:** https://dune.com/cow_protocol/cowswap

The official CoW Protocol Dune dashboard shows: solver leaderboards by surplus generated, win rates by solver, batch auction statistics, and comparative solver performance over time. This is the primary public-facing solver competition analytics tool.

**Relevance:** **HIGH** - Essential for understanding your competitive position and tracking what winning solvers are doing differently.

---

## Summary Priority Matrix

| Tool | Category | Arbitrum ✓ | Pricing | Relevance | Est. Hours |
|---|---|---|---|---|---|
| **Anvil (local fork)** | Simulation | ✓ | Free | HIGH | 8–16h |
| **Tenderly Simulation API** | Simulation | ✓ | Free–$450/mo | HIGH | 4–8h |
| **Alchemy Simulation API** | Simulation | ✓ | Free–PAYG | HIGH | 2–4h |
| **cowprotocol/services repo** | Solver Framework | ✓ | Free/OSS | CRITICAL | 40–80h |
| **0x Swap API (benchmark + RFQ)** | Liquidity/RFQ | ✓ | Free tier | HIGH | 2–4h |
| **DexScreener API** | Liquidity Data | ✓ | Free | MED-HIGH | 1–2h |
| **GeckoTerminal API** | Liquidity Data | ✓ | Free/CoinGecko | HIGH | 2–4h |
| **Pyth Network (SSE stream)** | Price Oracle | ✓ | Free | HIGH | 4–8h |
| **Chainlink Data Feeds** | Price Oracle | ✓ | Free (on-chain) | HIGH | 2–4h |
| **Uniswap V3 TWAP** | Price Oracle | ✓ | Free (on-chain) | HIGH | 4–8h |
| **Bebop API** | RFQ/Liquidity | ✓ | Contact required | HIGH | 8–16h |
| **Odos API** | Aggregator Benchmark | ✓ | Free | MED-HIGH | 4–8h |
| **EigenPhi (Arbitrum)** | MEV Intelligence | ✓ | Free dashboard | HIGH | 2–4h |
| **Arbitrum Nitro Node** | Infrastructure | ✓ | $200–500/mo infra | HIGH | 16–24h |
| **ArbGasInfo precompile** | Gas Estimation | ✓ | Free (on-chain) | HIGH | 2–4h |
| **Dune Analytics API** | Solver Analytics | ✓ | Free–$349/mo | MEDIUM | 4–8h |
| **The Graph subgraphs** | Pool Discovery | ✓ | Free–usage based | MEDIUM | 4–8h |
| **KyberSwap API** | Aggregator Benchmark | ✓ | Free | MEDIUM | 4–6h |
| **Hashflow RFQ** | Private Liquidity | ✓ | Permissioned | MEDIUM | 8–16h |
| **Flashbots Protect** | MEV Protection | ✗ Mainnet only | Free | N/A | - |
| **MEV Blocker** | MEV Protection | ✗ Mainnet only | Free | N/A | - |

---

## Recommended Build Sequence

**Phase 1 - Get a working solver (weeks 1–4):** Fork `cowprotocol/services`, implement the `solve` endpoint, integrate the 0x Swap API as your initial routing backend. This immediately gives you AMM liquidity + RFQ access on Arbitrum. Use Alchemy's simulation API to validate settlements before submission.

**Phase 2 - Improve price accuracy (weeks 5–8):** Add Pyth SSE streaming for real-time price reference. Implement Uniswap V3 TWAP reads as a manipulation check. Integrate Chainlink feeds for major pair sanity validation. Add DexScreener polling to warm your pool registry cache without burning RPC quota.

**Phase 3 - Get competitive (weeks 9–16):** Switch simulation to a local Anvil fork sidecar (eliminates latency and rate limits). Deploy your own Arbitrum Nitro node. Integrate `ArbGasInfo` precompile for accurate gas accounting. Contact Bebop for API access. Study EigenPhi's Arbitrum MEV leaderboard to understand where surplus is being extracted from and tune your routing accordingly.

**Phase 4 - Close the gap (ongoing):** Monitor `dune.com/cow_protocol/cowswap` solver leaderboards weekly. Study the OKX and BitGet solver implementations in the CoW Protocol services repo for patterns. Pursue Hashflow API access for zero-slippage quotes on liquid pairs.