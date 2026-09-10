# Intelligence Layer Research - Evaluation

**Date:** 2026-04-07
**Source:** `plan/research/theonepercentgap_tools.md`
**Verdict:** Good research. Clear priority matrix. Actionable.

---

## Strategy: Stack Free Tiers

We burn through any single API's free quota fast (~1K/day for Odos).
Instead of paying $299/mo for one aggregator, stack 5 free tiers:

| Aggregator | Free Tier | Rate Limit | Calldata? | RFQ Included? |
|-----------|-----------|------------|-----------|---------------|
| **Odos** | 1,000 req/day | 1 RPS | Yes (2-step) | No |
| **0x Swap v2** | Unlimited? | 5 RPS | Yes (1-step) | Yes (0x_RFQ, Hashflow) |
| **KyberSwap** | Unlimited w/ client-id | ~5 RPS est. | Yes (2-step) | Yes (Hashflow, 1inch LO) |
| **ParaSwap** | Unlimited w/ partner | generous | Yes (2-step) | Yes (Hashflow, AugustusRFQ) |
| **OpenOcean** | Unlimited? | unknown | Yes (1-step) | Some |

**Combined capacity:** ~15+ RPS across 5 sources, effectively unlimited daily quota.
When one hits its limit, rotate to the next. Race all 5 in parallel per order,
pick the best quote. This is what top solvers do.

**Exit strategy:** Once we know revenue, drop to 1-2 paid aggregators (likely 0x + Odos).

---

## Aggregator Integration Priority

### 1. 0x Swap API v2 - HIGHEST PRIORITY
- **Why first:** Single GET call returns calldata. 5 RPS free. Includes RFQ liquidity.
  Also returns `tokenMetadata.buyTaxBps`, free fee-on-transfer detection.
- **URL:** `https://api.0x.org/swap/allowance-holder/quote`
- **Headers:** `0x-api-key: {key}`, `0x-version: v2`
- **Params:** `chainId=42161`, `sellToken`, `buyToken`, `sellAmount`, `taker`
- **Response:** `transaction.data`, `transaction.to`, `transaction.gas`
- **Key:** Free API key from `dashboard.0x.org`

### 2. KyberSwap - HIGH PRIORITY
- **Why:** Different routing graph (40+ DEXs), includes Hashflow RFQ implicitly.
- **URL:** GET `https://aggregator-api.kyberswap.com/arbitrum/api/v1/routes`
  then POST `https://aggregator-api.kyberswap.com/arbitrum/api/v1/route/build`
- **Headers:** `x-client-id: cow-solver`
- **Response:** `encodedSwapData` + `routerAddress`

### 3. ParaSwap/Velora - MEDIUM PRIORITY
- **Why:** Third independent routing engine. Has `otherExchangePrices=true` for benchmarking.
- **URL:** GET `https://api.paraswap.io/prices` then POST `https://api.paraswap.io/transactions/42161`
- **Params:** `partner=cow-solver` (custom string, avoids 1bps fee)
- **Response:** `data`, `to`, `value`

### 4. OpenOcean - FALLBACK
- **Why:** Good long-tail token coverage. Free, no key needed. Backup when others fail.
- **URL:** GET `https://open-api.openocean.finance/v4/arbitrum/swap`
- **Params:** `inTokenAddress`, `outTokenAddress`, `amount`, `account`, `slippage`
- **Response:** `data`, `to`, `value`, `gasPrice`, `gasLimit`

### 5. Odos - ALREADY INTEGRATED
- **Status:** Already in codebase with budget limiter. 1 RPS, 1K/day.
- **Role:** Unique multi-token I/O capability. Keep as one of the parallel sources.

---

## Non-Aggregator Tools Worth Adding

### DefiLlama Prices (free, no key)
- `GET https://api.llama.fi/prices/current/arbitrum:0x{address}`
- Use for fair-value reference pricing in triage. Replaces reliance on driver `referencePrice`.

### Alchemy Simulation (already have RPC key)
- `alchemy_simulateExecution` via our existing Alchemy JSON-RPC URL
- Pre-check settlement validity before submission. Catches reverts.

### Token Lists (free, static)
- `https://tokenlist.arbitrum.io/ArbTokenLists/arbed_uniswap_labs.json`
- Load at startup. Token not on list = likely honeypot/fee-on-transfer. Skip.

---

## Architecture: Multi-Aggregator Racing

```
Order arrives
    │
    ├──► Odos quote (async)
    ├──► 0x quote (async)
    ├──► KyberSwap quote (async)
    ├──► ParaSwap quote (async)
    └──► OpenOcean quote (async)
    │
    ▼ (wait for all, timeout 3s)
    │
    Pick best outputAmount
    │
    ▼
    Use winner's calldata as interaction
```

Each aggregator returns calldata targeting its own router contract.
In CoW settlement, these become `interactions` entries with:
- `target` = aggregator's router address
- `calldata` = aggregator's returned calldata
- `value` = 0 (ERC20 swaps)

The settlement contract calls these interactions during `GPv2Settlement.settle()`.

---

## What We Skip (and why)

| Service | Reason |
|---------|--------|
| 1inch | Cloudflare-gated API key provisioning, less accessible |
| Li.Fi / Rango | Cross-chain focus, not useful for single-chain Arbitrum |
| Firebird | Docs appear parked/dead |
| BitGet DEX | Docs inaccessible, immature |
| DODO direct | Already captured via KyberSwap routing |
| Hashflow/Bebop direct | Require BD partnerships, get implicitly via 0x/KyberSwap |
| Flashbots/MEV tools | Not applicable on Arbitrum (centralized sequencer) |
| Goldsky Mirror | Overkill for our scale, paid |
| Pyth/Chainlink | Too slow for live solving |

---

## Cost Projection

| Phase | Monthly Cost | What We Get |
|-------|-------------|-------------|
| **Now** | $0 | 5 free aggregator APIs stacked |
| **If winning** | $0 still | Free tiers sufficient at ~100 auctions/day |
| **If >$500/mo revenue** | ~$299 | Upgrade Odos or 0x to paid tier, drop others |
| **If >$2K/mo revenue** | ~$500 | Odos paid + 0x paid, Tenderly for simulation |
