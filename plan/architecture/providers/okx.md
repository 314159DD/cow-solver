# OKX DEX

## What It Is
OKX's decentralized exchange aggregator API. Aggregates across 100+ liquidity sources on Arbitrum including their own private market making. This is the same integration pattern that CoW Protocol's own reference solver uses (`cowprotocol/services/crates/solvers/src/infra/dex/okx/`).

## Why We Use It
If the CoW Protocol team chose to integrate OKX in their reference solver, it validates this as a production-appropriate price source. OKX's market making adds liquidity that other aggregators might not have.

## API
- **Endpoint:** `GET https://www.okx.com/api/v5/dex/aggregator/quote`
- **Auth:** `Ok-Access-Key: {api_key}` header
- **Key params:** `chainId=42161`, `fromTokenAddress`, `toTokenAddress`, `amount`
- **Returns:** `toTokenAmount`, `estimateGasFee`

## Configuration
```
# .env
OKX_API_KEY=your_key_here
```
Register at okx.com (free).

## Cost
Free with registration. Rate limits apply.

## Current Status
**Ready but not active.** Code is implemented, needs `OKX_API_KEY` in `.env` to activate.

## Key Files
- `liquidity/aggregator/okx.rs`
