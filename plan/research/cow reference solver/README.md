# CoW Protocol Reference Solver Analysis

Research conducted: 2026-04-03
Repository: https://github.com/cowprotocol/services
Commit: 5b22eceaa10ab98c61bb4619cd2e8b3f87df282d

## Files in This Package

| File | Contents |
|------|---------|
| 01-architecture-overview.md | System architecture, crate map, pipeline overview |
| 02-liquidity-dto-format.md | Exact JSON formats for all pool types (ConstantProduct, V3, Balancer) |
| 03-baseline-solver-amm-math.md | AMM formulas, routing algorithm, clearing price construction |
| 04-clearing-prices-construction.md | Driver-side price validation, encoding, custom prices |
| 05-solution-dto-format.md | Exact solution JSON format the solver must return |
| 06-bug-analysis-key-differences.md | 10 key ways the reference differs from naive implementations |
| 07-sample-auction-json.md | Complete production-format auction+solution JSON with math verification |
| 08-complete-solve-pipeline.md | Full pipeline: stage by stage, with error reference |

## Quick Reference: The #1 Most Common Bug

For SELL orders: executedAmount = swap_input (EXCLUDING the solver fee)
The driver ADDS the fee when encoding the settlement. If you include the fee,
users get charged twice. See 06-bug-analysis-key-differences.md for all bugs.

## Key Formula: Clearing Prices

  prices[sell_token] = buy_output   (e.g., COW received)
  prices[buy_token]  = sell_input   (e.g., WETH sent to pool, excl. fee)

This is the direct AMM exchange amounts. NOT normalized. NOT in USD.
