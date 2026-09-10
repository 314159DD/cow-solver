# CoW Protocol Reference Solver Analysis
## 02 - Liquidity DTO Format: What the Solver Receives

Source files:
- crates/solvers-dto/src/auction.rs (canonical DTO types)
- crates/driver/src/infra/solver/dto/auction.rs (serialization logic)

---

## ConstantProduct Pool JSON Format

This is the CRITICAL format for UniV2-style pools.

```json
{
  "kind": "constantProduct",
  "id": "0",
  "address": "0x97b744df0b59d93A866304f97431D8EfAd29a08d",
  "router": "0x7a250d5630b4cf539739df2c5dacb4c659f2488d",
  "gasEstimate": "110000",
  "tokens": {
    "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2": {
      "balance": "3828187314911751990"
    },
    "0xDEf1CA1fb7FBcDC777520aa7f396b4E015F497aB": {
      "balance": "179617892578796375604692"
    }
  },
  "fee": "0.003"
}
```

CRITICAL DETAILS:
1. "kind" is a string tag using camelCase values
2. "tokens" is a HASHMAP keyed by address - NOT an array
3. "balance" is U256 as hex-or-decimal string
4. "fee" is a BigDecimal string (e.g. "0.003" = 0.3%)
5. UniV2 fee is hardcoded: BigDecimal::new(3.into(), 3) = 0.003
6. Swapr fee: BigDecimal::new(fee.bps(), 4) (basis points / 10000)

Rust structs:

```rust
pub struct ConstantProductPool {
    pub id: String,
    pub address: Address,
    pub router: Address,
    pub gas_estimate: U256,
    pub tokens: HashMap<Address, ConstantProductReserve>,
    pub fee: BigDecimal,
}

pub struct ConstantProductReserve {
    pub balance: U256,   // encoded as HexOrDecimalU256
}
```

---

## Full Auction JSON Structure

```json
{
  "id": "1",
  "tokens": {
    "<address>": {
      "decimals": 18,
      "symbol": "WETH",
      "referencePrice": "1000000000000000000",
      "availableBalance": "1412206645170290748",
      "trusted": true
    }
  },
  "orders": [ ... ],
  "liquidity": [ ... ],
  "effectiveGasPrice": "15000000000",
  "deadline": "2106-01-01T00:00:00.000Z",
  "surplusCapturingJitOrderOwners": []
}
```

Token fields:
- referencePrice: native token price in wei (1 ETH = 10^18)
- availableBalance: settlement contract buffer balance (for internalization)
- trusted: whether contract holds this token safely

---

## Order JSON Format

```json
{
  "uid": "0x2a2a...2a2a",
  "sellToken": "0xC02aaa...",
  "buyToken": "0xDEf1...",
  "sellAmount": "133700000000000000",
  "fullSellAmount": "133700000000000000",
  "buyAmount": "6000000000000000000000",
  "fullBuyAmount": "6000000000000000000000",
  "feePolicies": [],
  "validTo": 0,
  "kind": "sell",
  "owner": "0x5b1e...",
  "partiallyFillable": false,
  "preInteractions": [],
  "postInteractions": [],
  "sellTokenSource": "erc20",
  "buyTokenDestination": "erc20",
  "class": "market",
  "appData": "0x...",
  "signingScheme": "presign",
  "signature": "0x"
}
```

Key fields:
- kind: "sell" or "buy"
- class: "market" or "limit" - ONLY "limit" orders let solver determine fee
- sellAmount/buyAmount: effective amounts after driver-side fee adjustments
- fullSellAmount/fullBuyAmount: original un-adjusted amounts
- feePolicies: ONLY present if fee_handler == Solver

---

## All Liquidity Kinds

Available kinds: "constantProduct", "weightedProduct", "stable", "concentratedLiquidity", "limitOrder"

UniV3 uses an ARRAY for tokens (not HashMap):
```json
{
  "kind": "concentratedLiquidity",
  "tokens": ["0xtoken0", "0xtoken1"],
  "sqrtPrice": "...",
  "liquidity": "1000000",
  "tick": -887272,
  "liquidityNet": {"-887272": "1000000"},
  "fee": "0.003"
}
```

Balancer Weighted uses HashMap with extra fields:
```json
{
  "kind": "weightedProduct",
  "tokens": {
    "0xtoken": {
      "balance": "...",
      "scalingFactor": "1.0",
      "weight": "0.5"
    }
  },
  "fee": "0.001",
  "balancerPoolId": "0x...",
  "version": "V0"
}
```
