# CoW Protocol Reference Solver Analysis
## 04 - Clearing Prices: Construction, Validation, and Encoding

Source files:
- crates/driver/src/domain/competition/solution/mod.rs
- crates/driver/src/domain/competition/solution/trade.rs
- crates/driver/src/domain/competition/solution/encoding.rs

---

## What Clearing Prices Are

Clearing prices are an ARBITRARY-UNIT price vector where:
  amount_x * price_x = amount_y * price_y

They are NOT denominated in any specific currency. Only ratios matter.

---

## Driver Validation

```rust
// From solution/mod.rs
if solution.user_trades().any(|trade| {
    solution.clearing_price(trade.order().sell.token).is_none()
    || solution.clearing_price(trade.order().buy.token).is_none()
}) {
    return Err(error::Solution::InvalidClearingPrices);
}
```

Every token in EVERY user trade must have a clearing price. Missing = rejected.

---

## ETH vs WETH

```rust
pub fn clearing_prices(&self) -> Prices {
    if self.user_trades().any(|trade| trade.order().buys_eth()) {
        // ETH price = WETH price (driver handles wrap/unwrap)
        prices.insert(eth::ETH_TOKEN, self.prices[&self.weth]);
    }
    prices
}
```

The solver always deals with WETH. Driver adds ETH address mapping automatically.

---

## Custom Clearing Prices (Per-Trade)

Each trade also gets custom prices that account for fees:

```rust
// From trade.rs
pub fn custom_prices(&self, prices: &ClearingPrices) -> CustomClearingPrices {
    CustomClearingPrices {
        sell: self.buy_amount(prices)?,   // effective buy amount (after haircut)
        buy: self.sell_amount(prices)?,   // effective sell amount (including fees)
    }
}
```

These per-trade prices are encoded into the settlement calldata.

---

## Effective Amount Formulas

### sell_amount (what user actually pays):

For SELL orders:
  sell_amount = executed + fee

For BUY orders:
  sell_amount = executed * prices.buy / prices.sell + fee + haircut_in_sell

### buy_amount (what user actually receives):

For BUY orders:
  buy_amount = executed

For SELL orders (uses CEILING DIVISION):
  buy_amount = ceil(executed * prices.sell / prices.buy) - haircut_in_buy

CRITICAL: Ceiling division is used to match the settlement contract behavior!
Using floor division here will produce a buy_amount 1 wei too low.

---

## Settlement Encoding (encoding.rs)

Step 1: Sort tokens by address, build uniform price arrays:
```rust
for (token, amount) in clearing_prices.sorted_by_key(|(token, _)| *token) {
    tokens.push(token);
    clearing_prices.push(amount);
}
```

Step 2: For each trade, append custom prices:
```rust
tokens.push(price.sell_token);
tokens.push(price.buy_token);
clearing_prices.push(price.sell_price);  // = effective buy_amount
clearing_prices.push(price.buy_price);   // = effective sell_amount
trade.sell_token_index = tokens.len() - 2;
trade.buy_token_index = tokens.len() - 1;
```

The sell_token_index and buy_token_index for each trade point to its custom prices.

Step 3: The settlement contract checks:
  executedSellAmount * prices[buy_token_index] <= executedBuyAmount * prices[sell_token_index]

---

## Solution Merging

```rust
fn scaling_factor(first: &Prices, second: &Prices) -> Option<BigRational>
```

Two solutions can be merged only if common tokens have the SAME price ratio.
All shared tokens must scale by a single factor. Otherwise: IncongruentPrices error.

---

## CIP-38 Scoring

Score = sum of all trade surpluses

For SELL orders:
  surplus = executed_buy - limit_buy_for(executed_sell + fee)
  where limit_buy_for(x) = order.buy_amount * x / order.sell_amount (proportional)

For BUY orders:
  surplus = limit_sell_for(executed_buy) - (executed_sell + fee)
  where limit_sell_for(x) = order.sell_amount * x / order.buy_amount

The driver uses CEILING division in limit_buy_for to be consistent with contract.
