# Solving Pipeline

**What this is:** The core engine that takes an auction (list of orders + available pools) and produces the best solution (which orders to fill, through which pools, at what prices).

## How It Works

Every ~8 seconds, the CoW driver sends us an auction via `POST /solve`. We have up to 25 seconds to respond with our best solution. The solver runs through 5 phases of increasing depth, each building on the previous:

```
Auction arrives (970 orders, 114 tokens)
   │
   ▼
Phase 1: Quick Routes (0-50ms)
   CoW matching + direct single-pool routing
   → Guarantees a baseline solution fast
   │
   ▼
Phase 2: Multi-hop (50-500ms)
   Route through intermediaries (WETH, USDC)
   → Sometimes 2-hop beats direct
   │
   ▼
Phase 3: Split Routing (500ms-5s)          ← Skipped if >200 pools
   Split large orders across multiple pools
   → Reduces price impact on big trades
   │
   ▼
Phase 4: Graph Search (5-15s)              ← Skipped if >200 pools
   Yen's K-shortest paths across all pools
   → Finds non-obvious multi-hop routes
   │
   ▼
Phase 5: Aggregator Quotes (remaining time)
   Query 0x, Bebop, Paraswap for top 3 orders
   → Private market maker prices may beat on-chain
   │
   ▼
Best solution selected, submitted to driver
```

## Key Files

| File | What it does |
|------|-------------|
| `solver/mod.rs` | Orchestrates all 5 phases, manages time budgets |
| `solver/assembler.rs` | Assembles trades into valid solutions with UDCP prices |
| `solver/direct.rs` | Single-pool routing (V2 constant product + V3 concentrated) |
| `solver/router.rs` | Multi-hop routing through intermediaries |
| `solver/split.rs` | Split large orders across multiple pools |
| `solver/graph.rs` | Graph-based pathfinding (Yen's K-shortest) |
| `solver/cow_matching.rs` | Coincidence of Wants - match opposite orders directly |
| `solver/scoring.rs` | Score computation (surplus × reference price) |
| `solver/pricing.rs` | Uniform Directional Clearing Price (UDCP) computation |

## Why Phases Matter

Each phase is an "anytime algorithm" - if the driver times us out, we return whatever we have. Phase 1 runs in ~50ms and guarantees at least one solution. Later phases may find better routes but take longer.

With our current pool set (1,395 pools), Phases 3-4 are skipped because graph search takes too long. Phase 5 (aggregators) always runs since it only queries 3 orders.

## Order Selection

Not all 970 orders can be profitably filled. The assembler:
1. Routes ALL orders through available pools
2. Filters out unprofitable routes (surplus < gas cost)
3. Sorts by: new orders first → market orders → large orders (CIP-74) → highest surplus
4. Selects top 10 routes for the solution

## Scoring

The score determines which solution wins the auction:
```
score = Σ (surplus × reference_price) - gas_cost
```
Where surplus = how much better we fill the order vs its limit price.
Higher score = more value captured = we win.
