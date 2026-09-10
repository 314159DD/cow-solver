# Sprint 3: Optimization and Advanced Features (Weeks 5-8)

**Goal:** Make the solver competitive enough to win auctions consistently. Add more DEX sources, optimize solve speed, implement advanced routing, and deploy to production. Target: win at least 5% of auctions on Arbitrum.

**Outcome:** A production-deployed solver on Arbitrum (via CoW DAO bonding pool) winning real auctions and generating revenue.

---

## S3-1: Balancer V2 Integration

**Description:** Add Balancer V2 weighted pools and stable pools as liquidity sources. Balancer has significant liquidity for many pairs, especially stablecoin pools and LST pools.

**Output:** `solver-engine/src/liquidity/balancer_v2.rs`

**Key addresses (Ethereum mainnet):**
- Vault: `0xBA12222222228d8Ba445958a75a0704d566BF2C8`

**Pool types to support:**

1. **Weighted Pools** - generalized constant-product with arbitrary weights
   ```
   amountOut = balanceOut * (1 - (balanceIn / (balanceIn + amountIn * (1 - fee))) ^ (weightIn / weightOut))
   ```

2. **Stable Pools** - StableSwap invariant (like Curve)
   ```
   Uses amplification factor 'A' to concentrate liquidity near 1:1
   Better for stablecoin pairs (USDC/USDT/DAI)
   ```

**Functions to implement:**

```rust
pub struct BalancerPool {
    pub pool_id: [u8; 32],     // Balancer pool ID
    pub pool_type: BalancerPoolType,
    pub tokens: Vec<BalancerToken>,
    pub swap_fee: f64,
    pub amplification: Option<U256>,  // For stable pools
}

pub struct BalancerToken {
    pub address: Address,
    pub balance: U256,
    pub weight: Option<f64>,   // For weighted pools
    pub decimals: u8,
}

pub enum BalancerPoolType { Weighted, Stable, ComposableStable }

impl BalancerPool {
    /// Fetch pool data from Balancer Vault
    pub async fn from_pool_id(client: &EthClient, pool_id: [u8; 32]) -> Result<Self>;

    /// Simulate a swap
    pub fn get_amount_out(&self, token_in: Address, token_out: Address, amount_in: U256) -> Result<U256>;

    /// Encode a Balancer batch swap interaction via the Vault
    pub fn encode_swap(&self, token_in: Address, token_out: Address, amount_in: U256, min_out: U256, recipient: Address) -> Interaction;
}
```

**Pool discovery:**
- Use Balancer Subgraph: `https://api.thegraph.com/subgraphs/name/balancer-labs/balancer-v2`
- Query: pools with TVL > $100K, sorted by liquidity
- Cache pool IDs; refresh balances per-auction from Vault

**Acceptance criteria:**
- [ ] Discovers top 50 Balancer pools by TVL
- [ ] Weighted pool math matches Balancer SDK output within 0.01%
- [ ] Stable pool math matches for USDC/USDT/DAI swaps
- [ ] Vault batch swap encoding produces valid calldata
- [ ] Adds at least 3% more fillable orders (measure against Sprint 2 baseline)

---

## S3-2: Curve Finance Integration

**Description:** Add Curve pools, particularly for stablecoin and ETH LST pairs. Curve dominates stablecoin liquidity and is critical for competitive stablecoin routing.

**Output:** `solver-engine/src/liquidity/curve.rs`

**Key pools to support (Ethereum mainnet):**

| Pool | Address | Tokens |
|------|---------|--------|
| 3pool | `0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7` | USDC, USDT, DAI |
| stETH/ETH | `0xDC24316b9AE028F1497c275EB9192a3Ea0f67022` | stETH, ETH |
| FRAX/USDC | Various | FRAX, USDC |
| tricrypto2 | `0xD51a44d3FaE010294C616388b506AcdA1bfAAE46` | USDT, WBTC, WETH |

**Implementation approach:**
- Curve pools have diverse interfaces (plain, lending, meta, crypto)
- Start with plain pools and StableSwap pools only
- Use Curve's `get_dy()` view function for simulation (cheaper than reimplementing math)

```rust
pub struct CurvePool {
    pub address: Address,
    pub tokens: Vec<Address>,
    pub pool_type: CurvePoolType,
    pub balances: Vec<U256>,
    pub amplification: U256,
    pub fee: U256,
}

pub enum CurvePoolType { Plain, Meta, Crypto }

impl CurvePool {
    /// Fetch pool parameters
    pub async fn new(client: &EthClient, address: Address, token_count: usize) -> Result<Self>;

    /// Simulate swap using on-chain get_dy() call (accurate, costs 1 RPC call)
    pub async fn get_dy(&self, client: &EthClient, i: usize, j: usize, dx: U256) -> Result<U256>;

    /// Encode exchange interaction
    pub fn encode_exchange(&self, i: usize, j: usize, dx: U256, min_dy: U256) -> Interaction;
}
```

**Acceptance criteria:**
- [ ] 3pool (USDC/USDT/DAI) swap simulation matches actual output within 0.01%
- [ ] stETH/ETH pool works correctly
- [ ] Exchange calldata encoding is correct
- [ ] Stablecoin-to-stablecoin routing prefers Curve over Uniswap (lower slippage)

---

## S3-3: Solve Speed Optimization

**Description:** The solver has ~25 seconds (out of 30s auction) to find the best solution. Optimize the hot path to evaluate more routes in less time.

**Output:** Updates across solver crates. Create `solver-engine/src/solver/parallel.rs`.

**Optimizations to implement:**

1. **Parallel order solving** - Process independent orders concurrently using Rayon
   ```rust
   use rayon::prelude::*;

   let solutions: Vec<_> = orders
       .par_iter()
       .filter_map(|order| solve_order(order).ok().flatten())
       .collect();
   ```

2. **Pool data prefetching** - When auction arrives, immediately batch-fetch all relevant pool states
   ```rust
   // Extract unique tokens from auction
   let tokens: HashSet<Address> = auction.orders.iter()
       .flat_map(|o| [o.sell_token, o.buy_token])
       .collect();

   // Batch fetch all pool states in one multicall
   registry.sync_for_tokens(&tokens, client).await?;
   ```

3. **Route caching** - Cache recently computed routes for common pairs (invalidate on pool state change)

4. **Early termination** - If solution quality hasn't improved in last 2 seconds, stop and submit

5. **Tiered solving** - Fast strategies first (direct, CoW), expensive strategies only if time remains
   ```
   t=0s:   Start CoW matching + direct routing (< 1s)
   t=1s:   Multi-hop routing (< 3s)
   t=4s:   Split optimization (< 5s)
   t=9s:   If time remains, try 3-hop routes
   t=22s:  Deadline - submit best found
   ```

**Acceptance criteria:**
- [ ] Solve time for 20-order auction < 5 seconds (was ~15s before optimization)
- [ ] All orders are evaluated (no orders dropped due to timeout)
- [ ] Parallel solving produces identical results to sequential (deterministic)
- [ ] Time budget is configurable and logged per auction
- [ ] Benchmark: measure orders/second and routes/second

---

## S3-4: EBBO Compliance Checker

**Description:** CoW Protocol enforces EBBO (Ethereum Best Bid and Offer) - solutions must match or exceed the best available price from specified reference liquidity. Failing EBBO leads to solution rejection and potential slashing.

**Output:** `solver-engine/src/validation/ebbo.rs`

**Implementation:**

```rust
pub struct EbboChecker {
    reference_pools: Vec<Pool>,  // Uniswap V3 + Balancer - the EBBO reference set
}

impl EbboChecker {
    /// Check if an execution price meets or beats EBBO for a token pair
    pub fn check_order(
        &self,
        order: &Order,
        executed_price: f64,   // actual execution price we're offering
    ) -> EbboResult;

    /// Check all trades in a solution
    pub fn check_solution(&self, solution: &Solution, orders: &[Order]) -> Vec<EbboResult>;
}

pub enum EbboResult {
    Pass { margin_bps: i32 },       // Better than EBBO by margin_bps
    Fail { deficit_bps: i32 },      // Worse than EBBO by deficit_bps
    NoReference,                     // No EBBO reference available for this pair
}
```

**Acceptance criteria:**
- [ ] EBBO check runs on every solution before submission
- [ ] Solutions failing EBBO are logged and discarded (not submitted)
- [ ] EBBO margin is tracked as a metric (how much better than minimum)
- [ ] Handles pairs with no reference price gracefully

---

## S3-5: Solution Scoring Optimization

**Description:** Optimize how we compute and maximize solution scores. The CoW competition selects winners by score, so maximizing score directly improves win rate.

**Output:** `solver-engine/src/solver/scoring.rs`

**Score maximization strategies:**

1. **Surplus optimization** - When multiple pools can fill an order, pick the one with highest surplus (not just best price)
   ```
   surplus = (actual_output - limit_price_output) * reference_price
   score = surplus + protocol_fees
   ```

2. **Order selection** - When solving multiple orders, prioritize those with highest potential surplus (large orders, loose limit prices)

3. **Price improvement** - Set clearing prices to maximize score while maintaining UDCP
   ```
   For a sell order: user gets buy_amount + surplus
   Score increases with surplus, so push execution as far above limit as possible
   ```

4. **Solution merging** - When we have multiple partial solutions, find the combination that maximizes total score

**Acceptance criteria:**
- [ ] Score computation exactly matches CoW Protocol's formula (validate against known examples)
- [ ] Solver consistently picks higher-surplus routes over lower-surplus ones
- [ ] Multi-order solutions are scored correctly (sum of individual surpluses)
- [ ] A/B test: compare our score vs winning score in shadow competition

---

## S3-6: Private Liquidity / JIT Liquidity

**Description:** Implement Just-In-Time (JIT) liquidity: instead of routing through existing pools, the solver can provide liquidity itself for a small spread. This is useful for orders where no pool has good liquidity.

**Output:** `solver-engine/src/solver/jit.rs`

**NOTE:** JIT liquidity requires holding an inventory of tokens. Start with a small inventory (0.5 ETH + 2000 USDC) and only use JIT for small orders where pool routing is expensive.

**Implementation:**

```rust
pub struct JitProvider {
    inventory: HashMap<Address, U256>,  // Token balances available for JIT
    min_spread_bps: u32,                // Minimum spread to earn (e.g., 5 bps)
}

impl JitProvider {
    /// Check if we can profitably fill an order from inventory
    pub fn can_fill(
        &self,
        order: &Order,
        market_price: f64,
    ) -> Option<JitQuote>;

    /// Create a JIT trade (no interaction needed - solver provides tokens directly)
    pub fn create_jit_trade(
        &self,
        order: &Order,
        price: f64,
    ) -> Trade;
}

pub struct JitQuote {
    pub amount_out: U256,
    pub spread_earned: U256,
    pub spread_bps: u32,
}
```

**Acceptance criteria:**
- [ ] JIT provider correctly identifies orders it can fill from inventory
- [ ] Spread is at least `min_spread_bps` (never fills at a loss)
- [ ] JIT trades don't require on-chain interactions (lower gas cost)
- [ ] Inventory tracking is accurate (decrements on fill)
- [ ] JIT is only used when it beats pool routing (surplus comparison)

---

## S3-7: Deployment Pipeline

**Description:** Set up deployment infrastructure for running the solver 24/7 on a VPS. The solver needs to be always-on to receive auctions.

**Output:** `Dockerfile`, `docker-compose.yml`, `scripts/deploy.sh`, `.github/workflows/deploy.yml`

**Infrastructure:**

```yaml
# docker-compose.yml
services:
  solver:
    build: .
    env_file: .env
    ports:
      - "8000:8000"
    restart: always
    logging:
      driver: json-file
      options:
        max-size: "100m"
        max-file: "5"
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8000/health"]
      interval: 30s
      timeout: 10s
      retries: 3
```

**Dockerfile:**
```dockerfile
FROM rust:1.77 as builder
WORKDIR /app
COPY . .
RUN cargo build --release -p solver-engine

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/solver-engine /usr/local/bin/
CMD ["solver-engine"]
```

**Deployment target:** Railway or a $20/month VPS (Hetzner CX22 or similar)

**Acceptance criteria:**
- [ ] `docker build` produces a working image < 100MB
- [ ] Docker container starts and passes health check
- [ ] `docker-compose up` runs solver with persistent logs
- [ ] Deploy script handles zero-downtime restart
- [ ] Solver recovers automatically from crashes (restart: always)

---

## S3-8: Monitoring and Alerting

**Description:** Build a monitoring layer that tracks solver performance, win rate, revenue, and alerts on issues.

**Output:** `solver-engine/src/monitoring/mod.rs`, `solver-engine/src/monitoring/revenue.rs`

**Metrics to expose (Prometheus format at `/metrics`):**

```
# Auction metrics
cow_solver_auctions_total{status="received|solved|empty|error"}
cow_solver_solve_duration_seconds{quantile="0.5|0.9|0.99"}
cow_solver_orders_per_auction{quantile="0.5|0.9|0.99"}

# Competition metrics
cow_solver_solutions_submitted_total
cow_solver_solutions_won_total
cow_solver_win_rate
cow_solver_score_ratio{quantile="0.5"}  # our_score / winning_score

# Revenue metrics
cow_solver_surplus_earned_eth_total
cow_solver_gas_spent_eth_total
cow_solver_net_revenue_eth_total

# Health metrics
cow_solver_rpc_latency_seconds{quantile="0.5|0.9|0.99"}
cow_solver_rpc_errors_total
cow_solver_pool_count
cow_solver_uptime_seconds
```

**Revenue tracking:**
```rust
pub struct RevenueTracker {
    pub total_surplus_eth: f64,
    pub total_gas_eth: f64,
    pub net_revenue_eth: f64,
    pub wins_today: u64,
    pub daily_log: Vec<DailyRevenue>,
}

pub struct DailyRevenue {
    pub date: String,
    pub auctions_won: u64,
    pub surplus_earned: f64,
    pub gas_spent: f64,
    pub net: f64,
}
```

**Alerting (log-based initially, webhook later):**
- CRITICAL: Solver not receiving auctions for 5+ minutes
- WARNING: Win rate drops below 1% over last hour
- WARNING: RPC error rate > 10%
- INFO: Daily revenue summary

**Acceptance criteria:**
- [ ] `/metrics` returns Prometheus-format metrics
- [ ] Revenue tracker correctly computes net profit (surplus - gas)
- [ ] Alerts fire on test conditions
- [ ] Daily summary is logged at midnight UTC
- [ ] Metrics survive solver restart (persisted to disk or reconstructed from logs)

---

## S3-9: Arbitrum Deployment (Production)

**Description:** Deploy to Arbitrum (required first chain for CoW DAO bonding pool). Arbitrum has lower gas costs and is the required starting point for new solvers.

**Output:** Configuration updates, Arbitrum-specific contract addresses, deployment scripts.

**Arbitrum-specific configuration:**

| Config | Mainnet | Arbitrum |
|--------|---------|----------|
| Chain ID | 1 | 42161 |
| Settlement Contract | `0x9008D19f58AAbD9eD0D60971565AA8510560ab41` | `0x9008D19f58AAbD9eD0D60971565AA8510560ab41` |
| Uniswap V3 Factory | `0x1F98431c8aD98523631AE4a59f267346ea31F984` | `0x1F98431c8aD98523631AE4a59f267346ea31F984` |
| Sushiswap Factory | `0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac` | `0xc35DADB65012eC5796536bD9864eD8773aBc74C4` |
| Gas price | ~30 gwei | ~0.1 gwei |
| Block time | 12s | 0.25s |
| RPC endpoint | Alchemy Arbitrum | Alchemy Arbitrum |

**Steps:**
1. Update contract addresses for Arbitrum
2. Adjust gas estimates (Arbitrum L1 data cost + L2 execution)
3. Discover Arbitrum-specific pools (different DEX landscape)
4. Deploy solver with Arbitrum RPC
5. Join CoW shadow competition on Arbitrum
6. Contact CoW team to join staging/barn competition
7. Get whitelisted for production

**Initial gas funding needed:** 0.05 ETH on Arbitrum for staging, more for production

**Acceptance criteria:**
- [ ] Solver runs against Arbitrum RPC and discovers pools
- [ ] Gas estimation accounts for Arbitrum's L1+L2 cost model
- [ ] Solutions are valid for Arbitrum settlement contract
- [ ] Connected to Arbitrum shadow competition
- [ ] Win rate metrics are being tracked

---

## S3-10: Onboarding Completion

**Description:** Complete the CoW Protocol onboarding process to get the solver into production competition.

**Output:** Documentation of completed steps, solver credentials, monitoring access.

**Checklist:**

1. **Shadow competition** (must complete first):
   - [ ] Solver connected and receiving auctions
   - [ ] > 50% solution validity rate
   - [ ] Competitive scores (within 2x of winners)

2. **Onboarding call:**
   - [ ] Schedule call with CoW team via Telegram group
   - [ ] Demonstrate solver capabilities
   - [ ] Discuss bonding pool terms

3. **KYC documentation:**
   - [ ] Company/entity documentation
   - [ ] Developer passport(s)
   - [ ] Rewards address (must be controlled on both Arbitrum and Mainnet)

4. **Staging/barn competition:**
   - [ ] Receive submission addresses from CoW team
   - [ ] Configure solver for staging endpoints
   - [ ] Run for 1+ week with good validity rate

5. **Production:**
   - [ ] Get promoted to production (typically Tuesday release)
   - [ ] Monitor first 24 hours closely
   - [ ] Verify revenue is being earned

**Acceptance criteria:**
- [ ] Solver is live in production competition on Arbitrum
- [ ] First auction win recorded
- [ ] Revenue tracking shows positive net (surplus > gas)
- [ ] All onboarding documentation archived in `docs/onboarding/`

---

## S3-11: Performance Benchmarking Suite

**Description:** Build a benchmark suite that measures solver performance against historical auctions. Use this to validate optimizations without waiting for live auctions.

**Output:** `benchmarks/`, `scripts/benchmark.sh`

**Data collection:**
- Download 1000+ historical auction instances from CoW Protocol API
- Store in `benchmarks/data/auctions/`
- Include winning solution scores for comparison

**Benchmarks to implement:**

```rust
// benchmarks/solve_speed.rs
/// Measure: how many orders can we solve per second?
fn bench_solve_throughput(c: &mut Criterion);

// benchmarks/route_quality.rs
/// Measure: how does our output compare to the winning solution?
fn bench_route_quality(c: &mut Criterion);

// benchmarks/pool_sync.rs
/// Measure: how fast can we sync pool data?
fn bench_pool_sync(c: &mut Criterion);
```

**Acceptance criteria:**
- [ ] Benchmark suite runs via `cargo bench`
- [ ] Reports orders/second throughput
- [ ] Reports score ratio vs historical winners (our_score / winning_score)
- [ ] Reports pool sync latency
- [ ] Results are saved to `benchmarks/results/` for trend tracking

---

## Sprint 3 Definition of Done

1. Balancer V2 and Curve pools are integrated as liquidity sources
2. Solve speed handles 50+ order auctions within 25 seconds
3. EBBO compliance checker prevents invalid submissions
4. Solution scoring matches CoW Protocol formula exactly
5. Docker deployment runs 24/7 with health checks and auto-restart
6. Prometheus metrics expose all key performance indicators
7. Solver is deployed to Arbitrum and connected to shadow competition
8. Onboarding process initiated with CoW team
9. Benchmark suite validates performance against historical auctions
10. Win rate target: >5% of auctions on Arbitrum (even 1-2% is a valid start)
