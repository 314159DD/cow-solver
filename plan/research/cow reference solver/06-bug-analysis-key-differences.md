# CoW Protocol Reference Solver Analysis
## 06 - Bug Analysis: How the Baseline Differs From Naive Implementations

This is the most important file for debugging your solver implementation.

---

## Bug #1: executedAmount includes fee (MOST COMMON BUG)

WRONG implementation:
  executedAmount = sell_amount + fee   // for sell orders

CORRECT (reference):
  executedAmount = sell_amount - fee   // EXCLUDES fee

Why: The driver's encoding adds fee back:
  executed_amount_for_contract = trade.executed().0 + trade.fee().0

If you include fee in executedAmount, the contract encodes:
  (fee + sell_amount_with_fee) = sell_amount + 2*fee --> over-charges user

---

## Bug #2: Wrong clearing price formula

WRONG: Using total_out / total_in as a decimal/float price
WRONG: Using USD-denominated prices
WRONG: Normalizing to 1e18

CORRECT (reference):
  prices[sell_token] = raw_buy_output    (= COW received from pool)
  prices[buy_token]  = raw_sell_input    (= WETH sent to pool, ex-fee)

The formula amounts to: unit of sell_token is worth (buy_output) of buy_token.
This is the DIRECT AMM exchange amounts, nothing more.

---

## Bug #3: Swapped token prices

WRONG:
  prices[sell_token] = sell_amount
  prices[buy_token]  = buy_amount

CORRECT:
  prices[sell_token] = buy_amount  (sell token "priced in" buy token)
  prices[buy_token]  = sell_amount (buy token "priced in" sell token)

The price of sell_token = how many buy_tokens it's worth.
The price of buy_token  = how many sell_tokens it costs.

---

## Bug #4: Missing buy order output cap

WRONG: Returning the raw AMM output for buy orders without capping.

CORRECT (reference - added in May 2025):
  output.amount = min(route_output, order.buy_amount)

Why: The AMM amount_in uses ceiling division (+1). This can route to buy slightly
more than needed. The cap prevents claiming more output than the order requires.

---

## Bug #5: Floor division instead of ceiling for buy amounts

The settlement contract uses ceiling division when computing buy amounts from
the clearing price vector.

```rust
// From trade.rs
Side::Sell => {
    executed * prices.sell
        .checked_ceil_div(&prices.buy)  // CEILING!
}
```

If your implementation uses floor division, the driver's computed buy_amount
will be 1 wei less than what the contract actually produces, causing score
miscalculation (typically minor but can compound).

---

## Bug #6: Fee field on market orders

WRONG: Including "fee": "12345" in fulfillment for a market order
CORRECT: fee field should be ABSENT for market orders (class == "market")

The driver code:
```rust
match fulfillment.fee {
    Some(fee) => Fee::Dynamic(fee),  // limit order
    None => Fee::Static,             // market order (fee = 0)
}
```

For Static fee: order.solver_determines_fee() must be false (class != Limit).
Including fee for a market order causes validation: "orders with non solver determined gas cost fees are not supported"

---

## Bug #7: Sending ETH address instead of WETH

The driver rewrites ETH token addresses to WETH before sending to solver:
```rust
if solver_native_token.wrap_address {
    available.buy.token = available.buy.token.as_erc20(weth)
}
```

Your solver will NEVER receive ETH_ADDRESS (0xEeee...EEEe) in an order.
If your code handles ETH specially in orders, it will never trigger.
If a user submits an ETH buy order, the driver sends it to you as WETH.

---

## Bug #8: Non-numeric liquidity IDs

The driver parses liquidity IDs as usize:
```rust
let liquidity_id = usize::from_str(&interaction.id)?;
```

Your interaction must use the SAME numeric string ID as the liquidity from the auction.
Don't use UUIDs, hashes, or any non-numeric strings for liquidity interaction IDs.

---

## Bug #9: Missing prices for transitive tokens

If you have a 2-hop route: WETH -> USDC -> COW, you need prices for ALL 3 tokens:
  prices[WETH] = ...
  prices[USDC] = ...
  prices[COW]  = ...

The reference solver only produces SINGLE-hop solutions (one pool per order), so
it only needs 2 prices. If your solver does multi-hop, you need all intermediate
tokens to have prices in the prices map.

Wait - actually, looking at the code more carefully:
The baseline solver DOES support multi-hop routes, but the clearing prices only
include the final sell and buy token of the ORDER, not intermediate tokens.
Intermediate tokens used in interactions are handled implicitly through the
interaction amounts. Only the order's sell and buy token need prices.

---

## Bug #10: Slippage double-application

When the driver encodes a liquidity interaction, it applies slippage:
```rust
let (input, output) = slippage.apply_to(&slippage::Interaction {
    input: liquidity.input,
    output: liquidity.output,
});
```

If your solver already applied slippage to inputAmount/outputAmount, the driver
will apply it AGAIN. Only provide the EXACT amounts you want to swap. Let the
driver handle slippage.

---

## Summary Table

| Check | Reference | Common Bug |
|-------|-----------|------------|
| executedAmount sell | swap_in (no fee) | swap_in + fee |
| executedAmount buy | exact buy_out | swap_out (uncapped) |
| prices[sell_token] | buy_output | sell_amount or USD price |
| prices[buy_token] | sell_input_ex_fee | buy_amount or inverted |
| buy cap | min(output, order.buy) | uncapped |
| division | ceil | floor |
| market order fee | omit fee field | include fee field |
| ETH vs WETH | always WETH | special-case ETH |
| liquidity ID | numeric string | UUID or hash |
| slippage | exact amounts | pre-slipped amounts |
