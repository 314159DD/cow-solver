# CoW Solver - Master Expansion Plan

> Goal: Make this solver best-in-class competitive. No shortcuts. Quality first.

---

## Current State (Post-Audit)

**Working:** Uniswap V2, V3, SushiSwap, Balancer V2 (weighted + stable), Curve (StableSwap)
**Stubs:** JIT liquidity, split routing, parallel solver
**Missing:** Tick bitmap traversal, private liquidity, MEV protection, advanced routing, monitoring

---

## PHASE 1 - Total Liquidity Domination

> Every token pair, every pool, every drop of liquidity on Arbitrum. If there's a better price somewhere, we find it.

### 1.1 Fix What We Have

| Task | Why | Priority |
|------|-----|----------|
| **V3 tick bitmap traversal** | Current V3 sim ignores tick crossings. Large swaps ($50K+) get wrong prices. This is the #1 accuracy bug. | CRITICAL |
| **Curve on Arbitrum** | Curve pools exist on Arbitrum but our token addresses are hardcoded to mainnet. Dead code on our target chain. | CRITICAL |
| **Dynamic gas model** | Arbitrum L1 surcharges are hardcoded estimates. Real costs vary 20-30%. We're bidding blind. Fetch actual L1 base fee and compute from calldata size. | HIGH |
| **Chain-agnostic config** | Token addresses, factory addresses, pool addresses all hardcoded. Move to per-chain TOML config files so adding a chain is config, not code. | HIGH |

### 1.2 New Liquidity Sources (Arbitrum Priority)

| DEX | Type | Why | Complexity |
|-----|------|-----|------------|
| **Camelot V2** | Concentrated liquidity (Algebra) | Arbitrum's native DEX. Huge TVL. Not having this is leaving money on the table. | Medium |
| **Camelot V3** | Concentrated liquidity | Same as above, V3 fork | Medium |
| **GMX V2** | Perps/spot | Massive Arbitrum liquidity. GLP pools offer deep spot liquidity for majors (ETH, BTC, USDC). | High |
| **Trader Joe V2.1** | Liquidity Book (bin-based) | Major Arbitrum DEX. Different math from Uniswap - discretized bins instead of continuous curve. | High |
| **DODO** | Proactive Market Maker | PMM algorithm gives tighter spreads than constant-product for stablecoins. Significant Arbitrum presence. | Medium |
| **Wombat Exchange** | Stableswap variant | Arbitrum stablecoin specialist. Coverage-ratio-based pricing. | Medium |
| **KyberSwap Elastic** | Concentrated liquidity | Large aggregator with its own pools on Arbitrum. | Medium |
| **Zyberswap / Ramses** | Ve(3,3) DEXes | Solidly-fork DEXes on Arbitrum. Volatile + stable pool types. | Low-Medium |
| **Pendle** | Yield tokenization | PT/YT pools create unique arbitrage opportunities. Niche but profitable. | High |

### 1.3 External Liquidity APIs

| Source | What It Gives Us | Why |
|--------|-----------------|-----|
| **0x API** | RFQ quotes from professional market makers | Market makers often beat on-chain DEXes by 5-20bps. This is how top solvers win. |
| **1inch Fusion** | Aggregated quotes across 100+ sources | Instant access to every DEX we haven't integrated natively. Fallback for exotic pairs. |
| **Paraswap API** | Alternative aggregator quotes | Second opinion on routing. Sometimes finds paths 1inch misses. |
| **Hashflow** | RFQ from institutional market makers | Zero-slippage quotes for large orders. Huge edge on big trades. |
| **Bebop** | Multi-token batch RFQ | Can quote entire settlement batches, not just individual swaps. |

### 1.4 Routing Engine Upgrades

| Upgrade | What It Does | Impact |
|---------|-------------|--------|
| **Split routing** | Large orders split across multiple pools (e.g., 60% Uniswap V3, 40% Camelot) | 5-15% better execution on orders >$10K |
| **Dynamic intermediaries** | Instead of hardcoded [WETH, USDC, USDT, DAI, WBTC], scan all tokens with deep liquidity as potential hops | Catches routes through wstETH, rETH, FRAX, ARB, GMX, etc. |
| **3-hop routing** | A→B→C→D paths for exotic pairs | Only DEX route for long-tail tokens |
| **Graph-based pathfinding** | Model all pools as weighted edges in a directed graph. Run modified Dijkstra/Bellman-Ford for optimal path. | Replaces brute-force loop with O(E log V) optimal routing |
| **Partial fill optimization** | For partially_fillable orders, compute optimal fill amount that maximizes surplus per gas | Currently we fill 100% or skip. Partial fills can be more profitable. |

---

## PHASE 2 - MEV Strategy

> Decide: do we play defense, offense, or both?

### 2.1 Defense (Must Have)

| Protection | What | Why |
|-----------|------|-----|
| **Slippage limits in calldata** | Encode minAmountOut in all swap interactions | Without this, a sandwich attacker can drain all surplus from our solutions |
| **Private transaction submission** | Submit through Flashbots Protect / MEV Blocker instead of public mempool | Prevents frontrunning of our settlement transactions |
| **Solution encryption** | Encrypt solution before submission, reveal at settlement | Prevents other solvers from copying our routing |

### 2.2 Offense (Competitive Edge)

| Strategy | What | Risk/Reward |
|----------|------|-------------|
| **Backrunning** | After a large trade moves a pool's price, immediately arb it back to equilibrium and pocket the difference | Low risk, consistent small profits. Every top solver does this. |
| **MEV-Share integration** | Receive private pending transactions from Flashbots. Build solutions that backrun these for shared profit. | Medium risk. Requires Flashbots partnership. |
| **JIT liquidity** | When we see an order, provide concentrated liquidity in the exact tick range, collect fees, withdraw. | High complexity. Requires on-chain capital. But very profitable. |
| **Cross-domain MEV** | Spot price discrepancies between Arbitrum and mainnet. Bundle bridge + swap. | High complexity. Long-term play. |

### 2.3 Recommendation

Start with **full defense** (slippage + private submission). Add **backrunning** as first offensive move - it's low risk and the code structure already supports it via the assembler. JIT and cross-domain are Phase 3 territory.

---

## PHASE 3 - Support Systems (The War Machine)

> The solver is the brain. These systems are the eyes, ears, reflexes, and memory.

### 3.1 Live Pool State Engine

**What:** A persistent service that maintains a real-time graph of ALL pool states across every DEX.

**How:**
- WebSocket connections to Arbitrum node (not HTTP RPC)
- Subscribe to `eth_subscribe("logs")` for Swap/Sync/Mint/Burn events on all tracked pools
- On each event, update the in-memory pool state instantly
- Solver queries this cache instead of making RPC calls per auction

**Why:** Current flow is: receive auction → fetch pool data → compute routes → respond. The fetch step costs 200-500ms. With a live state engine, pool data is already in memory. Response time drops to pure compute time.

**Architecture:**
```
[Arbitrum Node] --WebSocket--> [Pool State Engine] --gRPC/shared memory--> [Solver]
                                    |
                                    v
                              [Pool State DB]
                              (hot cache in Redis or in-process)
```

### 3.2 Fork-Based Validation (Simulation Engine)

**What:** Before submitting a solution, simulate it against a local fork of the actual blockchain state.

**How:**
- Run Anvil (Foundry's local fork) pointed at Arbitrum
- For each candidate solution, replay the settlement transaction on the fork
- Verify: correct token transfers, no reverts, actual surplus matches predicted
- Only submit solutions that pass simulation

**Why:** Right now we trust our math. But smart contract edge cases (rebasing tokens, fee-on-transfer, approval race conditions) can cause reverts. A revert = wasted gas + penalty. Simulation catches these before they cost money.

### 3.3 Historical Auction Analyzer

**What:** A system that downloads every past CoW Protocol auction, replays them through our solver, and compares our solutions to the winning solutions.

**How:**
- CoW Protocol publishes all auction data via their API
- Download last 30 days of auctions on Arbitrum
- Run each through our solver
- Compare: did we win? By how much? If we lost, why? Which pool did the winner use that we didn't?

**Why:** This is how we identify exactly which liquidity sources and routing paths we're missing. Data-driven improvement instead of guessing.

**Output:** Weekly report showing:
- Win rate (% of auctions where our solution was best)
- Revenue gap (how much more we'd have earned with perfect routing)
- Top 10 missed opportunities (specific pools/routes we should add)

### 3.4 ML Route Predictor

**What:** A machine learning model that predicts the best routing strategy before we compute it.

**How:**
- Training data: historical auctions + winning solutions
- Features: token pair, order size, current pool reserves, gas price, time of day
- Output: probability distribution over strategies (direct V3, 2-hop via WETH, split across pools, etc.)
- Use prediction to prioritize which routes to compute first (within the 30s deadline)

**Why:** With 15+ DEXes and 3-hop paths, the search space is enormous. We can't evaluate every possible route in 30 seconds. ML tells us which 20% of routes cover 95% of wins, so we compute those first.

### 3.5 Revenue & Performance Dashboard

**What:** Real-time monitoring of solver performance.

**Metrics:**
- Auctions received / solutions submitted / wins per hour
- Revenue earned (surplus captured) in ETH and USD
- Win rate by token pair, order size, strategy type
- Latency breakdown (receive → compute → submit → settle)
- Gas cost vs surplus per solution
- EBBO compliance rate
- Comparison to other solvers (from public CoW data)

**Stack:** Prometheus metrics → Grafana dashboards. Already have metrics stubs in code.

### 3.6 Smart RPC Infrastructure

**What:** Optimized blockchain data access layer.

| Component | Purpose |
|-----------|---------|
| **Dedicated Arbitrum node** | Eliminate RPC rate limits. Sub-millisecond reads. Full archive access. |
| **Multicall V3 contract** | Batch 100+ pool reads into single call. Currently using JSON-RPC batching which is slower. |
| **State diff subscriptions** | Instead of polling, get push notifications when any tracked pool changes state. |
| **Geographic colocation** | Run solver in same datacenter as CoW Protocol infrastructure. Shave 10-50ms network latency. |

### 3.7 Order Flow Analysis

**What:** Predictive system that anticipates which orders are coming.

**How:**
- Monitor CoW Protocol's pending order book
- Track which tokens are trending (high volume = more orders coming)
- Pre-warm routing caches for likely token pairs
- Detect large incoming orders and pre-compute optimal splits

**Why:** If we know WETH/USDC is about to be hot, we can pre-compute the optimal route and respond in <1 second while competitors take 5-10 seconds.

---

## Priority Order

If I had to rank everything by impact-per-effort:

### Immediate (Before Shadow Competition)
1. V3 tick bitmap fix (accuracy)
2. Curve on Arbitrum (liquidity)
3. Dynamic gas model (profitability)

### First Month of Shadow
4. Camelot V2/V3 (Arbitrum's biggest native DEX)
5. Split routing (big order execution)
6. 0x RFQ integration (market maker liquidity)
7. Graph-based pathfinding (routing quality)
8. Slippage protection (MEV defense)

### Second Month
9. GMX V2 pools
10. Trader Joe Liquidity Book
11. Historical auction analyzer (data-driven iteration)
12. Fork-based validation (safety)
13. Live pool state engine (speed)
14. Backrunning (MEV offense)

### Third Month+
15. ML route predictor
16. Revenue dashboard
17. Dedicated node + colocation
18. Order flow analysis
19. JIT liquidity
20. Cross-domain MEV
