# CoW Protocol Reference Solver Analysis
## 03 - The Baseline Solver: Routing, AMM Math, and Price Construction

Source files:
- crates/solvers/src/domain/solver/baseline.rs
- crates/liquidity-sources/src/uniswap_v2/pool_fetching.rs
- crates/solvers/src/boundary/routing.rs
- crates/solvers/src/boundary/baseline.rs

---

## Baseline Solver: One Order At A Time

For each order in the auction:
1. Find all path candidates from sell_token to buy_token
2. Query each path for best swap amount
3. Pick best route
4. Compute fee = gas_cost * gas_price (in sell token)
5. Return single-order solution

Does NOT: batch orders, find CoWs, split orders across paths.

---

## Uniswap V2 AMM Formula - amount_out (sell orders)

```rust
fn amount_out(&self, amount_in: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
    // fee = Ratio(3, 1000) for UniV2 = 0.3%
    let amount_in_with_fee = amount_in * (fee.denom - fee.numer);  // amount_in * 997
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = reserve_in * fee.denom + amount_in_with_fee;  // reserve_in * 1000 + amount_in * 997
    numerator / denominator
    // = (amount_in * 997 * reserve_out) / (reserve_in * 1000 + amount_in * 997)
}
```

---

## Uniswap V2 AMM Formula - amount_in (buy orders)

```rust
fn amount_in(&self, amount_out: U256, reserve_in: U256, reserve_out: U256) -> Option<U256> {
    let numerator = reserve_in * amount_out * fee.denom;            // reserve_in * amount_out * 1000
    let denominator = (reserve_out - amount_out) * (fee.denom - fee.numer);  // (reserve_out - amount_out) * 997
    numerator / denominator + 1   // +1 for CEILING DIVISION
    // = ceil(reserve_in * amount_out * 1000 / ((reserve_out - amount_out) * 997))
}
```

CRITICAL: amount_in adds +1 (ceiling). This is required to match the contract.

Reserve limit: Both amount_in and amount_out are rejected if the final reserve exceeds 2^112 - 1.

---

## Route Selection Algorithm

For SELL orders:
1. Try all paths from sell_token to buy_token
2. For each path: simulate selling sell_amount through the path
3. Select the path that produces MAX buy_output

For BUY orders:
1. Try all paths from sell_token to buy_token
2. For each path: simulate buying buy_amount via the path
3. Select the path that requires MIN sell_input

Multi-hop paths are composed sequentially: output of each pool is input of the next.
For multi-pool at same hop: pick best (max out or min in) pool.

---

## Buy Order Output Capping - BUG FIX (May 2025)

```rust
// From baseline.rs
if let order::Side::Buy = order.side {
    output.amount = cmp::min(output.amount, order.buy.amount);
}
```

The AMM amount_in calculation (with ceiling) sometimes routes to buy SLIGHTLY MORE than requested.
This cap prevents the solver from claiming more than the order needs.
Without this cap: simulation fails because the AMM won't actually produce more than needed.

---

## Clearing Price Construction

From Single::into_solution (crates/solvers/src/domain/solution.rs):

```rust
prices: ClearingPrices::new([
    (order.sell_token, buy),               // sell token price = buy_output
    (order.buy_token, sell - surplus_fee), // buy token price = sell_input (excl. fee)
])
```

Where:
- buy = output.amount (COW received from AMM)
- sell = input.amount + surplus_fee (total WETH spent, including fee)
- sell - surplus_fee = input.amount (WETH given to AMM, no fee)

Example for sell 0.1337 WETH -> 6043.9 COW:
```
prices[WETH] = 6043910341261930467761   (= COW received)
prices[COW]  = 133700000000000000       (= WETH spent excl. fee = input.amount)
```

Interpretation: 6043.9 COW is worth 0.1337 WETH at these clearing prices.

---

## Executed Amount Convention

```rust
let executed = match order.side {
    Side::Buy  => buy,                          // = exact buy amount (capped)
    Side::Sell => sell.checked_sub(surplus_fee)?, // = sell amount EXCLUDING fee
};
```

For SELL orders: executedAmount = amount actually traded (not including fee)
For BUY orders: executedAmount = amount bought

---

## Fee Computation

```rust
let fee = sell_token_price.ether_value(
    eth::Ether(gas_estimate * gas_price)
)?.into();
```

fee = gas_used * gas_price * (1 ETH / sell_token_price_in_eth)

For MARKET orders: fee = Fee::Protocol (= zero, no solver-determined fee)
For LIMIT orders: fee = Fee::Surplus(computed_fee) - only when class == "limit"

---

## Sell Order: Full Price/Amount Flow

```
order: sell 0.1337 WETH, min buy 6000 COW
pool: reserve_WETH=3.828, reserve_COW=179617 (in same units)
fee = 0.003

Step 1: amount_out = 6043.9 COW for 0.1337 WETH input

Step 2 (fee for limit order): 
  gas_estimate = 166391 gas units
  gas_price = 15 Gwei
  fee_eth = 166391 * 15e9 = 2495865000000000 wei = 0.002496 ETH
  WETH_price = 1e18 (= 1 ETH worth 1e18 wei)
  surplus_fee = 2495865000000000 / 1e18 * WETH_per_ETH = 2495865000000000

Step 3 (for market order, Fee::Protocol, fee=0):
  sell = input.amount + 0 = 0.1337 WETH
  buy  = 6043.9 COW

Step 4: clearing prices
  prices[WETH] = 6043910341261930467761
  prices[COW]  = 133700000000000000

Step 5: executedAmount = sell - fee = 133700000000000000 - 0 = 133700000000000000
```
