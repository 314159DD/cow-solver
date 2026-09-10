# CoW Protocol Reference Solver Analysis
## 01 - Architecture Overview & Executive Summary

**Repository:** https://github.com/cowprotocol/services  
**Analysis Date:** 2026-04-03  
**Commit analyzed:** 5b22eceaa10ab98c61bb4619cd2e8b3f87df282d (main)

---

## Architecture: Two-Layer System

The CoW Protocol backend has TWO distinct solver layers that are easily confused:

### Layer 1: The driver crate (crates/driver/)
- Orchestrates the solve competition
- Fetches on-chain liquidity (UniV2, UniV3, Balancer, etc.)
- Serializes auctions and sends them to external solver HTTP APIs
- Receives solutions back from solvers via HTTP
- Validates, simulates, and submits settlements on-chain

### Layer 2: The solvers crate (crates/solvers/)
- A separate binary: the Baseline solver (and DEX aggregator solvers)
- Exposes an HTTP API that the driver calls
- Contains the actual path-finding and AMM math logic
- The "Baseline" solver is the production reference implementation

KEY INSIGHT: The driver sends JSON to the solver over HTTP. The solver is a separate process.
The protocol between them is defined in crates/solvers-dto/.

---

## Crate Map

| Crate | Role |
|-------|------|
| crates/driver | Orchestrator - calls external solver APIs, submits settlements |
| crates/solvers | Solver binary - Baseline + DEX solvers |
| crates/solvers-dto | Shared JSON DTO types (auction + solution) |
| crates/liquidity-sources | On-chain AMM fetching + pool math |
| crates/driver/src/infra/solver/dto/ | Driver-side serialization to solver API format |
| crates/driver/src/domain/competition/solution/ | Solution domain model |

---

## The Baseline Solver

Location: crates/solvers/src/domain/solver/baseline.rs

The Baseline solver is the production default solver. It:
- Finds the best path through on-chain liquidity (up to max_hops intermediate tokens)
- Uses a pure constant-product (Uniswap V2) AMM math model
- Handles Sell orders and Buy orders differently (see File 03)
- Processes ONE order at a time - no batch optimization, no CoW matching

IMPORTANT: The naive/baseline solver was removed (commit "Remove naive solver #3161", Dec 2024).
The current Baseline solver IS the path-finding solver.

---

## Complete Solve Pipeline

```
Autopilot
  => Driver
        => Fetch on-chain liquidity (UniV2/V3, Balancer, ZeroEx)
        => Build auction JSON (via infra/solver/dto/auction.rs)
        => POST /solve => Solver HTTP API (Baseline or external solver)
              Solver receives: auction JSON with orders + liquidity
              Solver returns: solution JSON with prices + trades + interactions
        => Parse solution (infra/solver/dto/solution.rs)
        => Validate clearing prices exist for all trades
        => Apply protocol fees (if fee_handler == Driver)
        => Encode settlement transaction (domain/competition/solution/encoding.rs)
        => Simulate settlement (check no revert, compute gas)
        => Check solver ETH balance
        => Submit to mempool
```

---

## Solver Entry Point

crates/solvers/src/run.rs:

```rust
cli::Command::Baseline { config: path } => {
    let config = config::baseline::load(&path).await;
    solver::Solver::Baseline(solver::Baseline::new(config).await)
}
```

The baseline solver config includes:
- weth: WETH address
- base_tokens: intermediary tokens for path-finding
- max_hops: maximum hops (default typically 2)
- max_partial_attempts: number of partial fill attempts
- solution_gas_offset: base gas overhead per settlement
