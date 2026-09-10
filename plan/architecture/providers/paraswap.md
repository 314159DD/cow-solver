# Paraswap

## What It Is
DEX aggregator with 21 liquidity sources on Arbitrum - the widest coverage of any free API we use. Includes UniswapV2/V3, SushiSwap, BalancerV2, Curve, Hashflow (private RFQ), DODO, Camelot, Ramses, WooFi, and their own AugustusRFQ network.

## Why We Use It
Used primarily as a **price oracle** - we compare Paraswap's best price against our internal routing to catch stale-data pricing errors. If Paraswap finds a much better price, it means our pool data is wrong. Also gives us access to Hashflow and Curve pricing without separate integrations.

## API
- **Endpoint:** `GET https://api.paraswap.io/prices`
- **Auth:** None required
- **Key params:** `srcToken`, `destToken`, `amount`, `side=SELL`, `network=42161`
- **Returns:** `priceRoute.destAmount`, best route details, source breakdown

## Configuration
No configuration needed - always active on Arbitrum.

## Cost
Free. No API key required for price queries.

## Current Status
**Active.** Enabled on every auction as a price validation source.

## Note
Paraswap's `/prices` endpoint returns price information only (no execution calldata). For execution, you'd need their `/transactions` endpoint which requires additional integration. We use it purely for price comparison.

## Key Files
- `liquidity/aggregator/paraswap.rs`
