# Bebop

## What It Is
DeFi liquidity aggregator backed by institutional market makers. Their quotes come from PMMv3 (private market makers) and JAMv2 sources - off-chain liquidity that on-chain DEXes don't have access to.

## Why We Use It
Private market makers often offer better prices than on-chain pools, especially for large trades ($10K+). The spread improvement is typically 0.1-0.5%, which is exactly the margin that wins CoW auctions.

## API
- **Endpoint:** `GET https://api.bebop.xyz/router/arbitrum/v1/quote`
- **Auth:** None required (free public API)
- **Key params:** `buy_tokens`, `sell_tokens`, `sell_amounts`, `taker_address`, `approval_type=Standard`
- **Returns:** `buyTokens.{address}.amount`, execution calldata

## Configuration
No configuration needed - always active on Arbitrum (chain_id=42161).

## Cost
Free. No API key, no rate limits published. We self-limit to 2 req/s.

## Current Status
**Active.** Enabled on every auction. Returns quotes for mainstream pairs.

## Key Files
- `liquidity/aggregator/bebop.rs`
