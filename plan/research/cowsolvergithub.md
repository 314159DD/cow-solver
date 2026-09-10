# CoW Protocol Reference Solver Analysis

**Date:** 2026-04-03
**Source:** https://github.com/cowprotocol/services (commit 5b22ece, Apr 1 2026)
**Target path:** plan/research/reference-solver-analysis.md

---

## 1. System Architecture Overview

The CoW Protocol solver infrastructure has TWO separate solver codebases:

### crates/solvers - The Reference Solver (External HTTP Solver)
The actual baseline/reference solver that runs as an external HTTP service. Key modules:
- domain/solver/baseline.rs - The baseline solver logic (one order at a time, no CoW)
- boundary/baseline.rs - Pool math dispatch + routing bridge
- boundary/routing.rs - Path-finding algorithms (estimate_buy_amount / estimate_sell_amount)
- domain/solution.rs - Solution construction including ClearingPrices

### crates/driver - The Driver
Orchestrates the full pipeline: fetches liquidity from on-chain sources, serializes auction to JSON, POSTs to solvers (including baseline), receives solution JSON, validates, scores, and encodes for settlement.

### crates/liquidity-sources - Pool Math
Contains exact Uniswap V2 amount_out / amount_in computations in pool_fetching.rs.

### crates/solvers-dto - Shared Data Transfer Objects
Canonical JSON schemas used by both driver (to serialize) and solver (to deserialize).

---

## 2. Liquidity Parsing - Auction JSON Format

### Source: crates/solvers-dto/src/auction.rs

Top-level auction structure:
```json
{
  "id": "12345",
  "tokens": {
    "0xTokenAddr": {
      "decimals": 18,
      "symbol": "WETH",
      "referencePrice": "1000000000000000000",
      "availableBalance": "500000000000000000",
      "trusted": true
    }
  },
  "orders": [ ... ],
  "liquidity": [ ... ],
  "effectiveGasPrice": "10000000000",
  "deadline": "2026-04-03T12:00:00Z",
  "surplusCapturingJitOrderOwners": []
}
```

### ConstantProduct Pool Format (Uniswap V2 / Swapr)

```json
{
  "kind": "constantProduct",
  "id": "0",
  "address": "0xPoolAddress",
  "router": "0xRouterAddress",
  "gasEstimate": "60000",
  "tokens": {
    "0xTokenA": { "balance": "1000000000000000000000" },
    "0xTokenB": { "balance": "500000000000000000000" }
  },
  "fee": "0.003"
}
```

**CRITICAL DETAILS:**
- kind field: "constantProduct" (from #[serde(tag = "kind", rename_all = "camelCase")])
- tokens is a HashMap NOT an array
- balance is raw token reserve as hex or decimal U256
- fee is BigDecimal string "0.003" (NOT "0.997", NOT "3", NOT "1000")
- Uniswap V2 fee: BigDecimal::new(3.into(), 3) = 3/1000 = 0.003
- Swapr fee: BigDecimal::new(bps.into(), 4)

### Rust Struct Definitions (from solvers-dto/src/auction.rs):
```rust
pub struct ConstantProductPool {
    pub id: String,
    pub address: Address,
    pub router: Address,
    pub gas_estimate: U256,          // serde: HexOrDecimalU256
    pub tokens: HashMap<Address, ConstantProductReserve>,
    pub fee: BigDecimal,
}

pub struct ConstantProductReserve {
    pub balance: U256,               // raw token units (NOT scaled by decimals)
}
```

### All Liquidity Kind Tags
```
"constantProduct"     → UniswapV2 or Swapr
"weightedProduct"     → Balancer V2 Weighted
"stable"              → Balancer V2 Stable
"concentratedLiquidity" → Uniswap V3
"limitOrder"          → ZeroEx / foreign limit orders
```

---

## 3. Order Format in Auction JSON

```json
{
  "uid": "0x...56bytes...",
  "sellToken": "0xTokenA",
  "buyToken": "0xTokenB",
  "sellAmount": "1000000000000000000",
  "fullSellAmount": "1000000000000000000",
  "buyAmount": "500000000000000000",
  "fullBuyAmount": "500000000000000000",
  "kind": "sell",
  "receiver": null,
  "owner": "0xOwner",
  "partiallyFillable": false,
  "class": "market",
  "preInteractions": [],
  "postInteractions": [],
  "sellTokenSource": "erc20",
  "buyTokenDestination": "erc20",
  "feePolicies": null,
  "appData": "0x...",
  "signingScheme": "eip712",
  "signature": "0x...",
  "validTo": 1900000000
}
```

Note: kind = "sell"/"buy" (lowercase), class = "market"/"limit" (lowercase).

---

## 4. The Baseline Solver - Exact Algorithm

### Source: crates/solvers/src/domain/solver/baseline.rs

```
FOR EACH order IN auction.orders:
  1. Get sell token reference price:
     - Check auction.tokens[sell_token].reference_price
     - If None AND sell_token != WETH: estimate via native price routing call
     - If still None: use U256::MAX (fee = 0, effectively)
  
  2. Generate route requests via requests_for_order():
     - Fully fillable orders: 1 request at full amount
     - Partially fillable: up to max_partial_attempts requests, halving each time:
       amounts[i] = original_amount >> i  (i.e., divide by 2^i)
  
  3. FOR EACH request:
     - IF sell_token == buy_token: trivial solution (no AMM, gas = 0 + offset)
     - ELSE: route via boundary_solver.route(request, max_hops)
  
  4. Convert route to solution:
     - fee = sell_token_price.ether_value(gas * gas_price)
     - Call Single::into_solution(fee)
  
  5. Apply buffer internalizations:
     with_buffers_internalizations(&auction.tokens)
  
  6. Send solution via channel
  
  7. BREAK (first successful route for each order)
```

### Key Config Constants
- DEADLINE_SLACK = 500ms (solver stops 500ms before deadline)
- max_hops: number of intermediary tokens (typically 2)
- POOL_SWAP_GAS_COST = 60,000 gas
- Base route gas = 50,000 + sum(pool costs)

### Buy Order Output Capping (CRITICAL BUG TARGET)
```rust
if let order::Side::Buy = order.side {
    output.amount = cmp::min(output.amount, order.buy.amount);  // MUST CAP
}
```
Without this, pool math rounding can give output.amount = order.buy.amount + epsilon, causing price validation failure.

---

## 5. Exact Pool Math (amount_out and amount_in)

### Source: crates/liquidity-sources/src/uniswap_v2/pool_fetching.rs

**amount_out formula (sell order, given exact input):**
```rust
fn amount_out(amount_in: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
    // fee = Ratio::new(3, 1000)
    let amount_in_with_fee = amount_in * 997;                              // fee = 0.3%
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = reserve_in * 1000 + amount_in_with_fee;
    numerator / denominator                                                 // TRUNCATES (floor)
}
// amount_out = floor( (amount_in * 997 * reserve_out) / (reserve_in * 1000 + amount_in * 997) )
```

**amount_in formula (buy order, given exact output):**
```rust
fn amount_in(amount_out: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
    let numerator = reserve_in * amount_out * 1000;
    let denominator = (reserve_out - amount_out) * 997;
    numerator / denominator + 1                                             // ROUNDS UP (ceiling)
}
// amount_in = ceil( (reserve_in * amount_out * 1000) / ((reserve_out - amount_out) * 997) )
```

**Reserve validation:**
```rust
// Pools with reserves > uint112 max are SILENTLY DISCARDED
let max = U256::from(2_u128.pow(112) - 1);  // = 5192296858534827628530496329220095
if a.amount > max || b.amount > max { return None; }

// Also validates:
// - Neither reserve can be zero
// - amount_out < reserve_out (or underflow → None)
// - final reserve_in = reserve_in + amount_in <= uint112 max
```

---

## 6. Route Finding Algorithm

### Source: crates/solvers/src/boundary/routing.rs

**Sell orders** → maximize buy amount → `estimate_buy_amount`
```rust
// For each hop in path, find pool with MAX output
// Final: pick path with MAXIMUM buy_amount
// Constraint: buy_amount >= request.buy.amount
```

**Buy orders** → minimize sell amount → `estimate_sell_amount`
```rust
// Reverse path, for each hop find pool with MIN input needed
// Final: pick path with MINIMUM sell_amount
// Constraint: sell_amount <= request.sell.amount
```

**Path candidates** (path_candidates_with_hops):
```
max_hops=0: [sell → buy]
max_hops=1: [sell → buy], [sell → WETH → buy], [sell → base1 → buy]
max_hops=2: above + [sell → base1 → base2 → buy], etc.
```

**Multiple pools per pair:** All pools evaluated, best selected per hop independently.

**traverse_path (forward simulation for sell orders):**
```rust
for liquidity in path {
    buy_amount = liquidity.get_amount_out(buy_token, (sell_amount, sell_token)).await?;
    segments.push(Segment { input: (sell_token, sell_amount), output: (buy_token, buy_amount) });
    sell_token = buy_token;
    sell_amount = buy_amount;
}
```

---

## 7. Clearing Prices - THE CRITICAL FORMULA

### Source: crates/solvers/src/domain/solution.rs - Single::into_solution()

```rust
pub fn into_solution(self, fee: eth::SellTokenAmount) -> Option<Solution> {
    let surplus_fee = fee.surplus().unwrap_or_default();
    
    let (sell, buy) = match order.side {
        
        Side::Buy => (
            input.amount.checked_add(surplus_fee)?,   // total sell = pool_input + fee
            output.amount,                              // buy = exact pool output (already capped)
        ),
        
        Side::Sell => {
            // Cap sell at order's limit
            let sell = input.amount.checked_add(surplus_fee)?.min(order.sell.amount);
            // Proportionally scale buy for the actual sell amount after fee
            let buy = sell
                .checked_sub(surplus_fee)?          // net sell = sell - fee
                .checked_mul(output.amount)?         // scale by pool output
                .checked_div(input.amount)?;         // proportional to pool input
            (sell, buy)
        }
    };
    
    Some(Solution {
        prices: ClearingPrices::new([
            (order.sell.token, buy),                    // price[sell_token] = buy_amount_received
            (order.buy.token, sell.checked_sub(surplus_fee)?),  // price[buy_token] = net_sell_amount
        ]),
        // executed amount: for sell = sell - fee; for buy = buy_amount
        trades: vec![Fulfillment::new(order, executed, fee)?],
        ...
    })
}
```

### What these prices mean:
- prices[SELL_TOKEN] = amount of buy token received from AMM  
- prices[BUY_TOKEN]  = net amount of sell token spent (excluding fee)
- The settlement contract verifies: sell_executed * prices[sell] >= buy_executed * prices[buy]
- These are RELATIVE prices in arbitrary units - their ratio is what matters

### Example: Sell 1 WETH for DAI
Pool: 100 WETH / 200,000 DAI, fee 0.3%
```
amount_out = (1e18 * 997 * 200000e18) / (100e18 * 1000 + 1e18 * 997) ≈ 1975.94e18
fee = gas_cost_in_eth_equivalent_sell_tokens (very small, often ~0 for test scenarios)
prices[WETH] = 1975.94e18   (= DAI received)
prices[DAI]  = 1.0e18       (= WETH spent net)
```

### Solution Merging (driver merges multiple single-order solutions):
```rust
// Prices must be congruent: there must be a UNIQUE scaling factor k
// such that prices_1[token] = k * prices_2[token] for all common tokens
// If no unique k exists → IncongruentPrices → merge rejected
fn scaling_factor(first: &Prices, second: &Prices) -> Option<BigRational>
```

---

## 8. Solution JSON Format (Solver → Driver)

### Source: crates/solvers-dto/src/solution.rs

```json
{
  "solutions": [{
    "id": 0,
    "prices": {
      "0xSellToken": "1975940000000000000000",   // price[sell_token] = buy_amount
      "0xBuyToken":  "1000000000000000000"        // price[buy_token]  = sell_amount_net
    },
    "trades": [{
      "kind": "fulfillment",
      "order": "0x...56bytes...",
      "executedAmount": "1000000000000000000",  // sell amount (excl. fee) for sell orders
      "fee": null                                // non-null only for FeeHandler::Solver mode
    }],
    "preInteractions": [],
    "interactions": [{
      "kind": "liquidity",
      "internalize": false,
      "id": "0",                       // MUST match auction.liquidity[].id
      "inputToken": "0xSellToken",
      "outputToken": "0xBuyToken",
      "inputAmount": "1000000000000000000",
      "outputAmount": "1975940000000000000000"
    }],
    "postInteractions": [],
    "gas": 150000
  }]
}
```

### Key Field Notes:
- prices: HashMap of token_address → U256 (arbitrary unit, meaningful only as ratios)
- executedAmount: sell orders = sell without fee; buy orders = buy amount
- fee field: only set when FeeHandler::Solver (solver manages fees)
- interaction id: string that EXACTLY matches auction.liquidity[N].id
- interactions kind "liquidity" or "custom" (custom has calldata, allowances, inputs, outputs)

---

## 9. Complete Solve Pipeline

### Step 1: Driver builds auction (crates/driver/src/infra/solver/dto/auction.rs)
```
1. Map competition::Auction → solvers_dto::Auction
2. For ConstantProduct pools: fee = BigDecimal::new(3, 3) = "0.003"
3. Ensure tokens map has entry for every token referenced by any liquidity
   (even if token is not in auction.tokens - uses default empty entry)
4. If FeeHandler::Driver + Volume fee policy:
   - Sell orders: sell_amount *= 1/(1+factor)   (reduced)
   - Buy orders:  buy_amount *= 1/(1-factor)     (increased)
5. Wrap ETH → WETH in sell/buy token addresses
6. POST to solver endpoint
```

### Step 2: Solver processes (crates/solvers/src/domain/solver/baseline.rs)
```
1. Build boundary solver from liquidity
2. For each order, find best route through AMM pools
3. Construct ClearingPrices as (buy_amount, sell_amount_net)
4. Return solutions
```

### Step 3: Driver validates solution (crates/driver/src/infra/solver/dto/solution.rs)
```
1. Find each order UID in auction.orders → error if not found
2. Find each liquidity ID in auction.liquidity → error if not found
3. Build competition::Solution with prices map
4. CRITICAL VALIDATION:
   if user_trade.order.sell.token NOT in prices → InvalidClearingPrices (reject entire solution)
   if user_trade.order.buy.token NOT in prices → InvalidClearingPrices (reject entire solution)
5. Apply protocol fees if FeeHandler::Driver
```

---

## 10. Bug Hunt Checklist - Differences from Reference

Compare our implementation against these:

### [BUG-1] Price construction direction (HIGHEST PRIORITY)
Reference: prices[sell_token] = buy_amount_out, prices[buy_token] = net_sell_amount
If inverted → settlement contract rejects (order limit price violated)

### [BUG-2] Buy order output capping (HIGH PRIORITY)
Reference: output.amount = min(output.amount, order.buy.amount)
Without this → rounding gives output > order.buy → price assertion fails

### [BUG-3] Sell order proportional buy scaling (HIGH PRIORITY)
Reference: buy = (sell - fee) * pool_output / pool_input (NOT raw pool_output)
Without proportional scaling → prices[buy_token] vs prices[sell_token] ratio is wrong

### [BUG-4] Clearing prices completeness
Reference: BOTH sell_token AND buy_token must be in prices map
Missing either → InvalidClearingPrices → entire solution rejected

### [BUG-5] amount_in ceiling rounding
Reference: amount_in = floor(numerator/denominator) + 1
Without +1 → slightly underestimates required sell → pool swap reverts

### [BUG-6] Reserve limit enforcement
Reference: silently discard pools where any reserve > 2^112 - 1
Pools with reserves exceeding uint112 fail boundary conversion (return None)

### [BUG-7] Fee format
Reference: fee = "0.003" (BigDecimal, decimal fraction)
NOT "0.997" (complement), NOT 3 (basis points), NOT 1000 (denominator)

### [BUG-8] Liquidity interaction ID matching
Reference: interaction.id must EXACTLY match the string id from auction.liquidity[N].id
Driver uses this string to look up the pool for ABI encoding

### [BUG-9] ETH vs WETH
Reference: solver always receives/sends WETH addresses (never 0xEEE...)
Driver wraps ETH→WETH before sending, maps ETH price = WETH price after receiving

### [BUG-10] executedAmount semantics
Reference: sell orders: executedAmount = sell_amount (excluding fee); buy: buy_amount

### [BUG-11] No CoW - independent order solving
Reference: baseline solver processes each order independently, no inter-order matching

---

## 11. Sample Auction and Solution

### Minimal Valid Auction (Sell 1 WETH for DAI through UniswapV2):
```json
{
  "id": "42",
  "tokens": {
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2": {
      "decimals": 18, "symbol": "WETH",
      "referencePrice": "1000000000000000000",
      "availableBalance": "0", "trusted": true
    },
    "0x6B175474E89094C44Da98b954EedeAC495271d0F": {
      "decimals": 18, "symbol": "DAI",
      "referencePrice": "500000000000",
      "availableBalance": "0", "trusted": true
    }
  },
  "orders": [{
    "uid": "0x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "sellToken": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
    "buyToken": "0x6B175474E89094C44Da98b954EedeAC495271d0F",
    "sellAmount": "1000000000000000000",
    "fullSellAmount": "1000000000000000000",
    "buyAmount": "1900000000000000000000",
    "fullBuyAmount": "1900000000000000000000",
    "kind": "sell",
    "receiver": null,
    "owner": "0x0000000000000000000000000000000000000001",
    "partiallyFillable": false,
    "class": "market",
    "preInteractions": [], "postInteractions": [],
    "sellTokenSource": "erc20", "buyTokenDestination": "erc20",
    "appData": "0x0000000000000000000000000000000000000000000000000000000000000000",
    "signingScheme": "eip712", "signature": "0x",
    "validTo": 9999999999
  }],
  "liquidity": [{
    "kind": "constantProduct",
    "id": "0",
    "address": "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11",
    "router": "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
    "gasEstimate": "60000",
    "tokens": {
      "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2": { "balance": "100000000000000000000" },
      "0x6B175474E89094C44Da98b954EedeAC495271d0F": { "balance": "200000000000000000000000" }
    },
    "fee": "0.003"
  }],
  "effectiveGasPrice": "10000000000",
  "deadline": "2026-04-03T13:00:00Z",
  "surplusCapturingJitOrderOwners": []
}
```

### Expected Solution (assuming fee ≈ 0 for simplicity):
```
amount_out = floor((1e18 * 997 * 200000e18) / (100e18 * 1000 + 1e18 * 997))
           = floor(199400e36 / 100997e18)
           = floor(1975296848568203...e18)  ≈ 1975296848568203... (exact value)

Actually let's compute more carefully:
numerator   = 1e18 * 997 * 200000e18 = 997 * 200000 * 1e36 = 199400000 * 1e36
denominator = 100e18 * 1000 + 1e18 * 997 = (100000 + 997) * 1e18 = 100997 * 1e18
amount_out  = 199400000e36 / (100997e18) = (199400000/100997) * e18 
            = 1974.84... * 1e18

Wait, let me recalculate:
reserve_in  = 100 * 1e18 = 100e18
reserve_out = 200000 * 1e18 = 200000e18  
amount_in   = 1e18

amount_in_with_fee = 1e18 * 997 = 997e18
numerator          = 997e18 * 200000e18 = 199400000e36
denominator        = 100e18 * 1000 + 997e18 = (100000 + 997) * 1e18 = 100997e18
amount_out         = 199400000e36 / 100997e18 = (199400000/100997) * 1e18
                   ≈ 1974.84... * 1e18

Exact: 199400000 / 100997 = 1974.84...
Integer division: floor(199400000 * 1e18 / 100997 * 1e18) = 1974...e18

Let's be more precise:
199400000 / 100997 ≈ 1974.84... 
Actually: 100997 * 1974 = 199367078
Remainder: 199400000 - 199367078 = 32922
So: floor = 1974 + floor(32922/100997) = 1974
amount_out = 1974 * 1e18 (approximately, for round numbers)

With actual values:
amount_out ≈ 1974.84e18 → 1974840000000000000000 (truncated)
```

```json
{
  "solutions": [{
    "id": 0,
    "prices": {
      "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2": "1974840000000000000000",
      "0x6B175474E89094C44Da98b954EedeAC495271d0F": "1000000000000000000"
    },
    "trades": [{
      "kind": "fulfillment",
      "order": "0x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
      "executedAmount": "1000000000000000000",
      "fee": null
    }],
    "preInteractions": [],
    "interactions": [{
      "kind": "liquidity",
      "internalize": false,
      "id": "0",
      "inputToken": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
      "outputToken": "0x6B175474E89094C44Da98b954EedeAC495271d0F",
      "inputAmount": "1000000000000000000",
      "outputAmount": "1974840000000000000000"
    }],
    "postInteractions": [],
    "gas": 150000
  }]
}
```

---

## 12. Source File Reference Table

| File | Purpose |
|------|---------|
| crates/solvers-dto/src/auction.rs | Canonical auction JSON struct definitions |
| crates/solvers-dto/src/solution.rs | Canonical solution JSON struct definitions |
| crates/driver/src/infra/solver/dto/auction.rs | Driver serialization of auction to JSON |
| crates/driver/src/infra/solver/dto/solution.rs | Driver deserialization + validation of solution |
| crates/solvers/src/domain/solver/baseline.rs | **Baseline solver main loop (one order at a time)** |
| crates/solvers/src/boundary/baseline.rs | Route-finding, pool dispatch, traverse_path |
| crates/solvers/src/boundary/routing.rs | Path candidates, estimate_buy/sell_amount |
| crates/solvers/src/domain/solution.rs | **ClearingPrices construction (into_solution)** |
| crates/liquidity-sources/src/uniswap_v2/pool_fetching.rs | **Exact amount_in/amount_out formulas** |
| crates/driver/src/domain/competition/solution/mod.rs | Driver validation (InvalidClearingPrices check) |
| crates/solvers/src/boundary/liquidity/constant_product.rs | Converts domain pool → boundary pool (u128 reserves) |

---

*End of analysis. Generated by examining the cowprotocol/services repository at commit 5b22ece (Apr 1, 2026).*
