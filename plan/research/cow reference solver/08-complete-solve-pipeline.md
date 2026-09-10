# CoW Protocol Reference Solver Analysis
## 08 - Complete Solve Pipeline Reference

Source: crates/driver/src/domain/competition/

---

## Stage 1: Auction Construction (Driver)

File: crates/driver/src/domain/competition/auction.rs

1. Driver receives auction parameters from Autopilot (orders, token prices, deadline)
2. Fetches on-chain liquidity for all relevant token pairs
3. Applies volume fee pre-adjustments to orders if fee_handler == Driver:
   - BUY orders: sell_amount reduced by factor (= sell_amount * 1/(1+factor))
   - SELL orders: buy_amount increased (= buy_amount * 1/(1-factor))
4. Serializes via infra/solver/dto/auction.rs::new()

---

## Stage 2: Solver HTTP Call

Driver POSTs:
  POST /solve
  Content-Type: application/json
  Body: { auction JSON }

Solver must respond before deadline.

---

## Stage 3: Baseline Solver Processing (Solver Binary)

File: crates/solvers/src/domain/solver/baseline.rs::Inner::solve()

For each order (in sequence):
  1. Get sell token price:
     a. If referencePrice is in tokens map -> use it
     b. If sell_token == WETH -> use 1e18
     c. Otherwise -> estimate via routing (sell 2^144 of sell_token, buy WETH)
  
  2. Build path candidates:
     base_tokens.path_candidates_with_hops(sell_token, buy_token, max_hops)
  
  3. Try each path:
     - SELL: find max buy_output via estimate_buy_amount()
     - BUY:  find min sell_input via estimate_sell_amount()
  
  4. Build route via traverse_path() which walks the selected pools
  
  5. Cap buy order output: min(route_output, order.buy_amount)
  
  6. Compute fee:
     fee = sell_token_price.ether_value(gas_estimate * gas_price)
  
  7. Build solution via Single::into_solution(fee):
     - Construct clearing prices
     - Compute executed amount
     - Validate limit price
     - Apply buffer internalizations
  
  8. Send solution to driver via channel

---

## Stage 4: Solution Parsing (Driver)

File: crates/driver/src/infra/solver/dto/solution.rs::Solutions::into_domain()

1. Look up each Fulfillment order by UID in the auction
2. Compute haircut_fee = executed_amount * haircut_bps / 10000
3. Create Fulfillment with (order, executed_amount, fee, haircut_fee)
4. Parse Liquidity interactions: look up by ID, create LiquidityInteraction
5. Parse Custom interactions with allowances and input/output assets
6. Convert JIT orders: recover signer from signature

---

## Stage 5: Solution Domain Validation

File: crates/driver/src/domain/competition/solution/mod.rs::Solution::new()

1. Convert surplus-capturing JIT orders to Fulfillments
2. Validate clearing prices present for all trade tokens
3. If fee_handler == Driver: apply protocol fees to each Fulfillment
4. Return validated Solution

Protocol fee application:
```
for each Fulfillment:
  prices = ClearingPrices {
    sell: solution.prices[order.sell_token.as_erc20(weth)],
    buy:  solution.prices[order.buy_token.as_erc20(weth)]
  }
  fulfillment = fulfillment.with_protocol_fees(prices)
```

---

## Stage 6: Settlement Encoding

File: crates/driver/src/domain/competition/solution/encoding.rs::tx()

1. Initialize arrays: tokens[], clearing_prices[], trades[], interactions[]

2. Build uniform price vector (sorted by address):
   for each (token, price) in solution.clearing_prices().sorted_by(token):
     tokens.push(token)
     clearing_prices.push(price)

3. For each trade:
   a. Compute uniform_prices from clearing price map
   b. Compute custom_prices = {buy: sell_amount_with_fee, sell: buy_amount}
   c. Append custom prices to tokens/clearing_prices arrays
   d. trade.sell_token_index = last-2, trade.buy_token_index = last-1
   e. Track native_unwrap amount if order buys ETH

4. For each AMM interaction:
   a. Apply slippage to input/output amounts
   b. Dispatch to pool-specific swap encoder
   c. Append approve interaction if needed

5. If any order buys ETH: append WETH.withdraw(total_unwrap_amount) interaction

6. Encode settlement calldata via GPv2Settlement.settle(tokens, prices, trades, interactions)

7. Append auction_id (8 bytes big-endian) to calldata

---

## Stage 7: Simulation & Validation

File: crates/driver/src/domain/competition/solution/settlement.rs

1. Check trusted tokens for internalized interactions
2. Compute partial access lists for ETH-buying orders with smart contract receivers
3. Simulate with access_list estimation and gas estimation
4. Gas limit check: gas_estimate <= block_gas_limit / 2
5. ETH balance check: solver_eth_balance >= gas_limit * 2 * max_fee_per_gas
6. If internalized interactions exist: also simulate WITHOUT internalizations

---

## Stage 8: Competition

The driver collects all solutions, computes CIP-38 scores, selects winner.

---

## Stage 9: Submission

Gas parameters (settlement.rs::Gas):
  estimate = simulated gas
  limit = min(estimate * 2.0, block_gas_limit / 2)

The 2x gas limit multiplier accounts for gas refunds (some solutions have
significant refunds that only occur at end of execution).

---

## Error Reference

| Error | Source | Meaning |
|-------|--------|---------|
| InvalidClearingPrices | validation | Missing price for traded token |
| InvalidExecutedAmount | validation | Executed + fee != order target |
| ProtocolFeeOnStaticOrder | fee application | fee on market order |
| NonBufferableTokensUsed | internalization | Untrusted token internalized |
| GasLimitExceeded | gas check | Solution too gas-heavy |
| SolverAccountInsufficientBalance | ETH check | Solver can't pay gas |
| FailingInternalization | simulation | Uninternalized version reverts |
