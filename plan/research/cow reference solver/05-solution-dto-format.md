# CoW Protocol Reference Solver Analysis
## 05 - Solution DTO Format: What the Solver Must Return

Source files:
- crates/solvers-dto/src/solution.rs
- crates/driver/src/infra/solver/dto/solution.rs (parsing logic)

---

## Solution JSON Structure

```json
{
  "solutions": [
    {
      "id": 0,
      "prices": {
        "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2": "6043910341261930467761",
        "0xdef1ca1fb7fbcdc777520aa7f396b4e015f497ab": "133700000000000000"
      },
      "trades": [
        {
          "kind": "fulfillment",
          "order": "0x2a2a...2a2a",
          "executedAmount": "133700000000000000"
        }
      ],
      "preInteractions": [],
      "interactions": [
        {
          "kind": "liquidity",
          "internalize": false,
          "id": "0",
          "inputToken": "0xc02aaa...",
          "outputToken": "0xdef1ca...",
          "inputAmount": "133700000000000000",
          "outputAmount": "6043910341261930467761"
        }
      ],
      "postInteractions": [],
      "gas": 166391
    }
  ]
}
```

---

## Trade Types

Fulfillment (settles an auction order):
```json
{
  "kind": "fulfillment",
  "order": "<56-byte-uid-hex>",
  "executedAmount": "<U256>",
  "fee": "<U256>"   // OPTIONAL: only present for limit orders
}
```

- executedAmount for SELL: executed sell amount, EXCLUDING the fee
- executedAmount for BUY: executed buy amount (exact fill)
- fee: ONLY include for class=="limit" orders. OMIT for market orders.

JIT (just-in-time liquidity):
```json
{
  "kind": "jit",
  "order": { full JIT order object },
  "executedAmount": "<U256>",
  "fee": "<U256>"
}
```

---

## Interaction Types

Liquidity (references a pool from the auction):
```json
{
  "kind": "liquidity",
  "internalize": false,
  "id": "0",
  "inputToken": "0x...",
  "outputToken": "0x...",
  "inputAmount": "133700000000000000",
  "outputAmount": "6043910341261930467761"
}
```

- id MUST match a liquidity item's id from the auction
- id MUST be parseable as usize (numeric string only)
- Driver generates the swap calldata; solver only specifies amounts

Custom (arbitrary on-chain call):
```json
{
  "kind": "custom",
  "internalize": false,
  "target": "0x...",
  "value": "0",
  "calldata": "0x...",
  "allowances": [],
  "inputs": [{"token": "0x...", "amount": "..."}],
  "outputs": [{"token": "0x...", "amount": "..."}]
}
```

---

## Clearing Price Encoding Rules

Keys: token addresses as lowercase 0x-prefixed hex strings
Values: U256 integers (arbitrary unit, only ratios matter)

The reference solver sets:
  prices[sell_token] = buy_output  (= COW received from AMM)
  prices[buy_token]  = sell_input  (= WETH given to AMM, excluding fee)

---

## Executed Amount - THE MOST CRITICAL DETAIL

| Order Kind | executedAmount = |
|------------|-----------------|
| Sell, market | sell_amount (= full sellAmount since Fee::Protocol) |
| Sell, limit | sell_amount MINUS the solver fee |
| Buy, market | buy_amount (= order.buyAmount for fill-or-kill) |
| Buy, limit | buy_amount (capped at order.buyAmount) |

The driver ADDS the fee back when encoding:
```rust
executed_amount: match trade.order().side {
    Side::Sell => trade.executed().0 + trade.fee().0,  // fee re-added!
    Side::Buy => trade.executed().into(),
}
```

So if you include fee in executedAmount, the contract will charge fee twice.

---

## Solution ID Convention

- id is a u64 assigned by the solver
- Driver wraps it in a globally unique ID
- For multiple solutions, each gets a distinct id
- The driver maps back to the solver's solution IDs in notifications
