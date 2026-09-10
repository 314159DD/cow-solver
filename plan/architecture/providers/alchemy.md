# Alchemy

## What It Is
Alchemy is our Ethereum RPC provider. It gives us access to the Arbitrum blockchain - reading pool states, fetching events, and simulating transactions. Think of it as our "internet connection" to the blockchain.

## Why We Use It
Every DEX pool lives on-chain. To know the current price of a swap, we need to read the pool's reserves from the blockchain. Alchemy provides this via JSON-RPC calls (`eth_call`, `eth_getLogs`, `eth_blockNumber`).

## How We Use It

### HTTP RPC (pool data)
- **getReserves()** on V2 pools → current reserves
- **slot0()** on V3 pools → current price/tick/liquidity
- **eth_getLogs** → catch Sync/Swap events to track pool changes
- **eth_blockNumber** → track chain head

### WebSocket (real-time events)
- **eth_subscribe("logs")** → receive V2 Sync and V3 Swap events in real-time
- Sub-second latency vs 10-second HTTP polling
- Free on Alchemy (WS subscriptions don't consume CU)

## Configuration
```
# .env
RPC_URL=https://arb-mainnet.g.alchemy.com/v2/{your_key}
```
The WebSocket URL is auto-derived by replacing `https://` with `wss://`.

## Cost
- **Free tier:** 30M compute units (CU) per month
- **Pay-as-you-go:** $0.45 per 1M CU over 30M
- **Our usage:** ~25M CU/month (fits free tier)
- **Breakdown:** Event polling ~19M + subgraph ~0.5M + JIT refresh ~5M

| Operation | CU per call |
|-----------|------------|
| eth_call (getReserves, slot0) | 26 |
| eth_getLogs | 75 |
| eth_blockNumber | 10 |
| WebSocket subscription | 0 (events counted on delivery) |

## Current Status
- **Plan:** Pay-as-you-go (should switch to free tier)
- **Spend to date:** ~$15
- **Dashboard URL:** https://dashboard.alchemy.com

## Key Files
- `pool_indexer.rs` - HTTP RPC calls for pool data
- `ws_monitor.rs` - WebSocket event subscriptions
- `shared/src/rpc.rs` - Low-level RPC client
