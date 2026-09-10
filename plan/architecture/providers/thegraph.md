# The Graph

## What It Is
Decentralized indexing protocol. Hosts the official Uniswap V3 Arbitrum subgraph which gives us tick-level liquidity data - the critical piece for accurate V3 swap pricing.

## Why We Use It
Uniswap V3 pools concentrate liquidity in specific price ranges (ticks). To accurately price a swap, we need to know which ticks have liquidity and how much. This data isn't available from a simple RPC call - it requires querying thousands of tick positions. The Graph indexes this data and makes it queryable via GraphQL.

## What We Get
- `sqrtPriceX96`, `tick`, `liquidity` per pool (current state)
- `liquidityNet` per initialized tick (how much liquidity enters/exits at each tick boundary)
- Currently: 100 top pools by TVL, ~30,000 ticks total, refreshed every 30 seconds

## Configuration
```
# .env
thegraph_api_key=your_key_here
```
Subgraph ID: `FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM`

## Cost
Free tier at studio.thegraph.com. Rate-limited (~100K queries/month). Growth: ~$4/1M queries.

## Status
**Active.** 89 pools loaded with tick data, 30K ticks total. Refreshes every 30s (full tick refresh every 5min).

## Key Files
- `subgraph.rs` - GraphQL queries, tick data storage, background updater
