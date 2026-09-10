# CoW Protocol

## What It Is
CoW Protocol is the batch auction system we compete in. They run the **Autopilot** (auction coordinator) and the **Driver** (connects to our solver). We don't run any CoW infrastructure - they host it and call our `/solve` endpoint.

## How It Works
1. Users submit trade orders to CoW Protocol
2. Autopilot batches orders every ~8 seconds
3. Driver sends batch to all registered solvers (including us) via `POST /solve`
4. Each solver returns their best solution
5. Driver picks the highest-scoring valid solution
6. Winning solution gets settled on-chain via the GPv2Settlement contract

## What the Driver Sends Us
- `orders`: 900-1000+ trade intents per batch
- `tokens`: reference prices for ~114 tokens
- `liquidity`: pool data (currently `[]` - we've asked them to enable V3 + Balancer V2)
- `effectiveGasPrice`: current gas price
- `block`: current chain head

## What We Send Back
```json
{ "solutions": [{ "id": 0, "prices": {...}, "trades": [...], "interactions": [...], "score": {...} }] }
```

## Key People
- **Tamir** - CoW team contact for driver configuration
- **Bram** - CoW team, answered timing questions

## Pending Requests
1. Enable `[[liquidity.uniswap-v3]]` in driver config (sent config block with subgraph ID)
2. Enable `[[liquidity.balancer-v2]]` in driver config
3. Onboarding call for KYC + live environment access

## Settlement Contract
`0x9008D19f58AAbD9eD0D60971565AA8510560ab41` (same on all chains)

## Key URLs
- Competition API: `https://api.cow.fi/arbitrum_one/api/v1/solver_competition/{auction_id}`
- Docs: https://docs.cow.fi
- Solver onboarding: https://docs.cow.fi/cow-protocol/tutorials/solvers/onboard
- Reference code: https://github.com/cowprotocol/services

## Key Files
- `routes/solve.rs` - POST /solve handler
- `models/auction.rs` - Auction payload parsing
- `models/solution.rs` - Solution response format
- `competition.rs` - Shadow comparison against real winners
- `settlement.rs` - GPv2Settlement ABI encoding
