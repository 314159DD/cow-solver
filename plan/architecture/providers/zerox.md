# 0x

## What It Is
DEX aggregator API. Finds the best swap route across dozens of liquidity sources including private market makers. Our closest competitor (zeroex-solve) likely runs on this infrastructure.

## Why We Use It
0x aggregates liquidity we don't have direct access to - private market makers, exotic DEXes, cross-protocol routes. When their quote beats our internal routing, we use their execution path.

## API
- **Endpoint:** `GET https://api.0x.org/swap/allowance-holder/quote`
- **Headers:** `0x-api-key: {key}`, `0x-version: v2`
- **Key params:** `chainId=42161`, `sellToken`, `buyToken`, `sellAmount`, `taker`
- **Returns:** `buyAmount`, `transaction.data` (calldata), `transaction.to`, `transaction.gas`

## Configuration
```
# .env
ZEROX_API_KEY=your_key_here
```
Register at https://dashboard.0x.org

## Cost
Free Standard plan: 5 requests/second, 0.15% swap fee (only charged on execution, not quoting).

## Current Status
**Active but limited.** Most quotes return "no liquidity available" for the obscure token pairs in our top orders. Works well for mainstream pairs (WETH/USDC, ARB/USDC).

## Key Files
- `liquidity/aggregator/zerox.rs`
