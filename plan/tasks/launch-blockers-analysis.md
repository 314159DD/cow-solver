# Launch Blockers Analysis - 2026-03-30

## Status: Shadow mode working, scores in right ballpark (~1.2M gwei vs winners at ~1M gwei)

## Why Scores Are Constant (~1,230,900 gwei every auction)

The same 5 limit orders sit in the CoW orderbook across batches. Our cached pools
have reserves that change slowly (30s hot-refresh cycle). Same orders + same pools
= same routes = same surplus = same score. Real solvers see variance because they
use fresh per-block reserves from the driver - but our driver sends `liquidity: []`.

## Critical Path to Live

### 1. BLOCKER: Settlement encoding sends empty trades (settlement.rs:157)
- `encode_settlement()` hardcodes `trades_encoded = encode_empty_dynamic_array()`
- Driver eth_call always reverts → simulation fails → can't validate solutions
- Fix: implement proper GPv2Trade ABI encoding with order data, signatures, token indices

### 2. BLOCKER: Driver sends 0 parseable pools
- We only parse 4 liquidity types: constantProduct, weightedProduct, stable, concentratedLiquidity
- Driver likely sends different formats (limitOrder, foreignLimitOrder, etc.)
- We fall back to cached pools (stale reserves)
- Fix: add support for driver's pool formats, OR ensure pool indexer refresh works correctly

### 3. BLOCKER: 5 strategies DEAD (cow, combined, multihop, graph, split)
- These strategies produce solutions that exceed the global score cap (1e16)
- Root cause: they use stale cached pools → inflated surplus → capped scores
- With proper pool data, their scores would be realistic
- Fix: fix pool data first, then remove/raise caps

### 4. MAJOR: No per-auction earnings tracking
- We show total surplus generated but not per-auction "what we would've earned"
- Competition comparison shows gap% but not dollar amounts
- Fix: compute earnings from competition data (our_score vs winner, reward formula)

### 5. MAJOR: EBBO validation not enforced
- validation/ebbo.rs exists but is never called
- Solutions may violate EBBO (execute worse than reference price)
- Driver rejects these silently
- Fix: call ebbo checker before submission

### 6. MEDIUM: Token approvals not prepended
- interactions/approvals.rs has encoding but it's never used in the pipeline
- Settlement may fail on-chain if DEX router needs approval
- Fix: extract approval targets from interactions, prepend if needed
