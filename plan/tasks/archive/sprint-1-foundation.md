# Sprint 1: Foundation (Weeks 1-2)

**Goal:** Set up the Rust project, connect to Ethereum, monitor DEX pools, and implement a basic solver HTTP server that can receive auction instances from the CoW Protocol driver.

**Outcome:** A running solver engine at `localhost:8000/solve` that receives auction JSON, returns trivial solutions, and can be tested locally with the CoW driver.

---

## S1-1: Project Scaffolding

**Description:** Initialize the Rust workspace with all crate dependencies, CI config, and project structure.

**Input:** Nothing (greenfield).

**Output:** A compilable Rust workspace with the directory structure below.

**Files to create:**

```
cow-solver/
  Cargo.toml                    # Workspace manifest
  solver-engine/
    Cargo.toml                  # Binary crate for the solver HTTP server
    src/
      main.rs                   # Entry point - starts Actix/Axum HTTP server
      config.rs                 # Env-based config (RPC_URL, PORT, LOG_LEVEL, CHAIN_ID)
      routes/
        mod.rs
        solve.rs                # POST /solve endpoint handler
      models/
        mod.rs
        auction.rs              # Auction instance deserialization structs
        solution.rs             # Solution serialization structs
        order.rs                # Order types (sell, buy, limit)
        token.rs                # Token metadata
        liquidity.rs            # AMM/liquidity source types
  shared/
    Cargo.toml                  # Shared library crate
    src/
      lib.rs
      types.rs                  # Ethereum types (Address, U256, H256)
      errors.rs                 # Error types
  tests/
    integration/
      test_solve_endpoint.rs    # Integration test: POST /solve with sample auction
  data/
    sample_auction.json         # Sample auction instance from CoW docs/GitHub
  .env.example                  # RPC_URL, SOLVER_PORT, LOG_LEVEL, CHAIN_ID
  .gitignore
  README.md
```

**Dependencies (Cargo.toml):**

```toml
[workspace]
members = ["solver-engine", "shared"]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
axum = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
ethers = "2"
alloy = { version = "0.9" }
reqwest = { version = "0.12", features = ["json"] }
tracing = "0.1"
tracing-subscriber = "0.3"
dotenvy = "0.15"
anyhow = "1"
thiserror = "1"
```

**Acceptance criteria:**
- [ ] `cargo build` succeeds with zero warnings
- [ ] `cargo test` runs (tests can be empty stubs)
- [ ] `.env.example` documents all required env vars
- [ ] `cargo run` starts HTTP server on configured port

---

## S1-2: Configuration and Environment

**Description:** Implement config loading from environment variables with sensible defaults.

**Input:** `.env.example` template.

**Output:** `solver-engine/src/config.rs` with a `Config` struct.

**Config struct fields:**

| Field | Env Var | Default | Description |
|-------|---------|---------|-------------|
| `rpc_url` | `RPC_URL` | (required) | Ethereum JSON-RPC endpoint (Alchemy/Infura) |
| `solver_port` | `SOLVER_PORT` | `8000` | HTTP server listen port |
| `chain_id` | `CHAIN_ID` | `1` | Target chain (1=mainnet, 42161=Arbitrum, 100=Gnosis) |
| `log_level` | `LOG_LEVEL` | `info` | Tracing log level |
| `driver_url` | `DRIVER_URL` | `""` | CoW driver URL (for future use) |
| `max_solve_time_ms` | `MAX_SOLVE_TIME_MS` | `25000` | Max time per solve request (30s auction, 5s buffer) |

**Acceptance criteria:**
- [ ] Server refuses to start if `RPC_URL` is missing
- [ ] All optional fields have working defaults
- [ ] Config is logged at startup (with RPC_URL partially redacted)

---

## S1-3: Auction Instance Data Model

**Description:** Define Rust structs that deserialize the auction JSON the CoW driver sends to `POST /solve`. The format is defined by the CoW solver API.

**Input:** Sample auction JSON from `data/sample_auction.json` (download from CoW Protocol GitHub: `https://github.com/cowprotocol/services/tree/main/crates/solvers/test_data`).

**Output:** `solver-engine/src/models/auction.rs` and related model files.

**Key structs to implement:**

```rust
/// Top-level auction instance sent by the driver
pub struct AuctionInstance {
    pub id: Option<String>,
    pub tokens: HashMap<Address, TokenInfo>,
    pub orders: Vec<Order>,
    pub liquidity: Vec<Liquidity>,
    pub effective_gas_price: U256,
    pub deadline: String,  // ISO 8601
}

pub struct TokenInfo {
    pub decimals: Option<u8>,
    pub symbol: Option<String>,
    pub reference_price: Option<f64>,  // price in ETH
    pub available_balance: U256,
    pub trusted: bool,
}

pub struct Order {
    pub uid: String,
    pub sell_token: Address,
    pub buy_token: Address,
    pub sell_amount: U256,
    pub buy_amount: U256,
    pub fee_amount: U256,
    pub kind: OrderKind,        // "sell" or "buy"
    pub partially_fillable: bool,
    pub class: OrderClass,      // "market", "limit", "liquidity"
}

pub enum OrderKind { Sell, Buy }
pub enum OrderClass { Market, Limit, Liquidity }

pub struct Liquidity {
    pub kind: LiquidityKind,
    pub id: String,
    // fields vary by kind - use enum variants
}

pub enum LiquidityKind {
    ConstantProduct(ConstantProductPool),
    WeightedProduct(WeightedProductPool),
    Stable(StablePool),
    ConcentratedLiquidity(ConcentratedPool),
}

pub struct ConstantProductPool {
    pub address: Address,
    pub token_a: Address,
    pub token_b: Address,
    pub reserve_a: U256,
    pub reserve_b: U256,
    pub fee: f64,  // e.g., 0.003 for 0.3%
}
```

**Acceptance criteria:**
- [ ] `serde_json::from_str::<AuctionInstance>(sample_json)` succeeds on the sample data
- [ ] All CoW order types (market, limit, liquidity) deserialize correctly
- [ ] All liquidity types (constant product, weighted, stable, concentrated) deserialize correctly
- [ ] Unit tests cover happy path and malformed input

---

## S1-4: Solution Data Model

**Description:** Define Rust structs for the solution JSON that gets returned from `POST /solve`.

**Output:** `solver-engine/src/models/solution.rs`

**Key structs:**

```rust
pub struct SolveResponse {
    pub solutions: Vec<Solution>,
}

pub struct Solution {
    pub id: String,
    pub prices: HashMap<Address, U256>,    // Uniform clearing prices per token
    pub trades: Vec<Trade>,
    pub interactions: Vec<Interaction>,      // On-chain calls to execute
    pub score: Score,
}

pub struct Trade {
    pub kind: TradeKind,
    pub order: String,             // Order UID
    pub executed_amount: U256,     // Amount actually executed
    pub fee: Option<U256>,         // Solver-determined fee
}

pub enum TradeKind { Fulfillment, JIT }  // Fulfillment = filling a user order

pub struct Interaction {
    pub target: Address,
    pub value: U256,
    pub call_data: Vec<u8>,
}

pub enum Score {
    Solver { score: U256 },        // Self-reported score
    RiskAdjusted { success_probability: f64 },
}
```

**Acceptance criteria:**
- [ ] Solution serializes to valid JSON matching CoW driver expectations
- [ ] Round-trip test: serialize -> deserialize -> compare
- [ ] Empty solution (no trades) serializes correctly

---

## S1-5: HTTP Server and /solve Endpoint

**Description:** Implement the Axum HTTP server with a `POST /solve` endpoint that receives an `AuctionInstance` and returns a `SolveResponse`. Initially returns empty solutions (no trades). This is the "hollow solver" that proves the plumbing works.

**Input:** Auction JSON from the driver.

**Output:** `solver-engine/src/routes/solve.rs`, `solver-engine/src/main.rs`

**Implementation:**

```
POST /solve
  Request body: AuctionInstance (JSON)
  Response body: SolveResponse (JSON)
  Timeout: MAX_SOLVE_TIME_MS

  Logic (Sprint 1 - trivial):
    1. Deserialize auction
    2. Log: number of orders, number of liquidity sources, tokens involved
    3. Return { solutions: [] }  // empty - no trades yet
```

**Also implement:**
- `GET /health` - returns 200 with `{"status": "ok"}`
- Request logging middleware (method, path, status, latency)
- Graceful shutdown on SIGTERM

**Acceptance criteria:**
- [ ] `curl -X POST http://localhost:8000/solve -H "Content-Type: application/json" -d @data/sample_auction.json` returns valid JSON
- [ ] Response contains `{ "solutions": [] }`
- [ ] Server logs auction metadata (order count, token count)
- [ ] Health check returns 200
- [ ] Server starts in < 2 seconds

---

## S1-6: Ethereum RPC Client

**Description:** Build a reusable Ethereum RPC client wrapper using `alloy` for querying on-chain state. This is the foundation for pool monitoring and price fetching.

**Output:** `shared/src/rpc.rs`

**Functions to implement:**

```rust
pub struct EthClient {
    provider: RootProvider<Http<Client>>,
    chain_id: u64,
}

impl EthClient {
    pub async fn new(rpc_url: &str, chain_id: u64) -> Result<Self>;

    /// Get current block number
    pub async fn block_number(&self) -> Result<u64>;

    /// Get ETH balance of address
    pub async fn eth_balance(&self, address: Address) -> Result<U256>;

    /// Call a contract read function (generic)
    pub async fn call(&self, to: Address, data: Vec<u8>) -> Result<Vec<u8>>;

    /// Get ERC20 token balance
    pub async fn erc20_balance(&self, token: Address, owner: Address) -> Result<U256>;

    /// Get ERC20 decimals
    pub async fn erc20_decimals(&self, token: Address) -> Result<u8>;

    /// Batch multiple calls in one RPC request (eth_call batching)
    pub async fn multicall(&self, calls: Vec<(Address, Vec<u8>)>) -> Result<Vec<Vec<u8>>>;
}
```

**Acceptance criteria:**
- [ ] Can connect to Alchemy/Infura free tier and fetch block number
- [ ] `erc20_balance` works for WETH and USDC on mainnet
- [ ] `multicall` batches 10+ calls into a single RPC request
- [ ] Handles RPC errors gracefully (timeout, rate limit, invalid response)
- [ ] Integration test against mainnet (skipped in CI, run manually)

---

## S1-7: Uniswap V2 Pool Monitor

**Description:** Implement a pool data fetcher for Uniswap V2 (and Sushiswap, which uses the same interface). Given a token pair, fetch reserves, fee tier, and compute spot price.

**Output:** `solver-engine/src/liquidity/uniswap_v2.rs`

**Key addresses (Ethereum mainnet):**
- Uniswap V2 Factory: `0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f`
- Sushiswap Factory: `0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac`
- Standard fee: 0.3% (30 bps)

**Functions to implement:**

```rust
pub struct UniV2Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: u32,   // 30 for Uniswap V2
    pub dex: DexSource, // Uniswap, Sushiswap
}

impl UniV2Pool {
    /// Fetch pool address from factory for a given token pair
    pub async fn from_pair(
        client: &EthClient,
        factory: Address,
        token_a: Address,
        token_b: Address,
        fee_bps: u32,
        dex: DexSource,
    ) -> Result<Option<Self>>;

    /// Fetch current reserves from the pool contract
    pub async fn sync_reserves(&mut self, client: &EthClient) -> Result<()>;

    /// Calculate output amount for a given input (constant product formula)
    /// output = (input * fee_factor * reserve_out) / (reserve_in + input * fee_factor)
    pub fn get_amount_out(&self, amount_in: U256, token_in: Address) -> Result<U256>;

    /// Calculate required input for a desired output
    pub fn get_amount_in(&self, amount_out: U256, token_out: Address) -> Result<U256>;

    /// Spot price of token_in denominated in token_out
    pub fn spot_price(&self, token_in: Address) -> f64;
}
```

**Acceptance criteria:**
- [ ] Can fetch WETH/USDC pool from Uniswap V2 factory
- [ ] Reserves match what Etherscan shows (within 1 block)
- [ ] `get_amount_out` for 1 ETH matches Uniswap UI quote (within 0.1%)
- [ ] `get_amount_in` is the inverse of `get_amount_out` (round-trip test)
- [ ] Works for both Uniswap V2 and Sushiswap factories

---

## S1-8: Uniswap V3 Pool Monitor

**Description:** Implement pool data fetching for Uniswap V3. V3 is more complex due to concentrated liquidity (ticks), but it has the most volume and is essential for competitive routing.

**Output:** `solver-engine/src/liquidity/uniswap_v3.rs`

**Key addresses (Ethereum mainnet):**
- Uniswap V3 Factory: `0x1F98431c8aD98523631AE4a59f267346ea31F984`
- Fee tiers: 100 (0.01%), 500 (0.05%), 3000 (0.3%), 10000 (1%)

**Functions to implement:**

```rust
pub struct UniV3Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
}

impl UniV3Pool {
    /// Fetch pool from factory
    pub async fn from_pair(
        client: &EthClient,
        factory: Address,
        token_a: Address,
        token_b: Address,
        fee: u32,
    ) -> Result<Option<Self>>;

    /// Sync current price and liquidity from on-chain slot0
    pub async fn sync_state(&mut self, client: &EthClient) -> Result<()>;

    /// Simulate a swap through the pool using tick math
    /// This is the core V3 math: iterate through ticks, compute amounts
    pub fn simulate_swap(
        &self,
        amount_in: U256,
        token_in: Address,
        tick_data: &[TickData],  // pre-fetched tick bitmap
    ) -> Result<SwapResult>;

    /// Get spot price from sqrtPriceX96
    pub fn spot_price(&self) -> f64;
}

pub struct TickData {
    pub tick: i32,
    pub liquidity_net: i128,
}

pub struct SwapResult {
    pub amount_out: U256,
    pub price_impact: f64,
    pub ticks_crossed: u32,
}
```

**Acceptance criteria:**
- [ ] Can fetch WETH/USDC 0.3% pool from V3 factory
- [ ] `sqrt_price_x96` decodes to correct human-readable price
- [ ] `simulate_swap` for 1 ETH is within 0.5% of actual V3 quoter result
- [ ] Handles edge cases: zero liquidity at current tick, crossing tick boundaries
- [ ] Unit tests with hardcoded pool state (no RPC needed)

---

## S1-9: Pool Registry and Discovery

**Description:** Build a registry that discovers and caches pool data across multiple DEXs. This is the "liquidity map" the solver uses to find routing paths.

**Output:** `solver-engine/src/liquidity/registry.rs`

**Implementation:**

```rust
pub struct PoolRegistry {
    pools: HashMap<(Address, Address), Vec<Pool>>,  // token pair -> pools
    last_sync: Instant,
}

pub enum Pool {
    UniV2(UniV2Pool),
    UniV3(UniV3Pool),
    // Future: Balancer, Curve
}

impl PoolRegistry {
    /// Discover all pools for a given set of tokens
    /// Checks Uniswap V2, Sushiswap, Uniswap V3 (all fee tiers)
    pub async fn discover_pools(
        &mut self,
        client: &EthClient,
        tokens: &[Address],
    ) -> Result<usize>;

    /// Refresh reserves/prices for all known pools
    pub async fn sync_all(&mut self, client: &EthClient) -> Result<()>;

    /// Get all pools that can trade between two tokens
    pub fn pools_for_pair(&self, token_a: Address, token_b: Address) -> &[Pool];

    /// Get all pools involving a specific token
    pub fn pools_for_token(&self, token: Address) -> Vec<&Pool>;

    /// Get the best spot price for a pair across all pools
    pub fn best_price(&self, token_in: Address, token_out: Address) -> Option<f64>;
}
```

**Acceptance criteria:**
- [ ] Discovers 6+ pools for WETH/USDC (V2 Uni, V2 Sushi, V3 at 4 fee tiers)
- [ ] `sync_all` refreshes reserves in < 3 seconds (batched RPC)
- [ ] `best_price` returns the V3 0.05% pool price (usually best for major pairs)
- [ ] Pool data is cached; subsequent lookups don't hit RPC
- [ ] Handles token pairs with no pools gracefully

---

## S1-10: Local Testing with CoW Driver

**Description:** Set up the local testing environment by running the CoW Protocol driver against our solver engine. This validates end-to-end plumbing.

**Input:** Our solver running at `localhost:8000`, CoW services repo cloned.

**Output:** `scripts/local_test.sh`, `scripts/driver.config.toml`

**Steps to automate in script:**

```bash
#!/bin/bash
# scripts/local_test.sh

# 1. Start our solver engine
cargo run --release -p solver-engine &
SOLVER_PID=$!
sleep 2

# 2. Verify solver is healthy
curl -f http://localhost:8000/health || exit 1

# 3. Send sample auction directly (unit test)
curl -X POST http://localhost:8000/solve \
  -H "Content-Type: application/json" \
  -d @data/sample_auction.json | jq .

# 4. (Advanced) Run with CoW driver - requires services repo
# cargo run -p driver -- --config scripts/driver.config.toml --ethrpc $RPC_URL

kill $SOLVER_PID
```

**driver.config.toml:**
```toml
[[solver]]
name = "cow-solver"
endpoint = "http://localhost:8000"
```

**Acceptance criteria:**
- [ ] `scripts/local_test.sh` runs end-to-end without errors
- [ ] Solver receives auction, logs metadata, returns empty solution
- [ ] Script exits cleanly (no zombie processes)
- [ ] README documents how to run the local test

---

## S1-11: Logging, Metrics, and Error Handling

**Description:** Set up structured logging with `tracing` and basic metrics counters. Every auction should be logged with enough context to debug issues.

**Output:** Update `solver-engine/src/main.rs`, create `solver-engine/src/metrics.rs`

**Log events to capture:**

| Event | Level | Fields |
|-------|-------|--------|
| Server started | INFO | port, chain_id, rpc_url (redacted) |
| Auction received | INFO | auction_id, order_count, token_count, liquidity_count |
| Solve completed | INFO | auction_id, solution_count, duration_ms |
| Solve failed | ERROR | auction_id, error message |
| RPC call | DEBUG | method, target, duration_ms |
| Pool discovered | DEBUG | dex, token_pair, address |

**Metrics (simple counters, no Prometheus yet):**

```rust
pub struct SolverMetrics {
    pub auctions_received: AtomicU64,
    pub auctions_solved: AtomicU64,
    pub auctions_empty: AtomicU64,    // returned 0 solutions
    pub auctions_errored: AtomicU64,
    pub total_solve_time_ms: AtomicU64,
    pub rpc_calls: AtomicU64,
    pub rpc_errors: AtomicU64,
}
```

**Add `GET /metrics` endpoint** returning JSON counters.

**Acceptance criteria:**
- [ ] Every auction logs order count and solve duration
- [ ] RPC errors are logged with context (URL, method, error)
- [ ] `/metrics` returns current counter values
- [ ] Log output is JSON-structured (for future log aggregation)

---

## Sprint 1 Definition of Done

All of the following must be true:

1. `cargo build --release` compiles with zero errors and zero warnings
2. `cargo test` passes all unit and integration tests
3. `cargo run -p solver-engine` starts a server that accepts `POST /solve`
4. The solver correctly deserializes a real CoW auction instance
5. Pool monitors can fetch live data from Uniswap V2, V3, and Sushiswap
6. Local test script demonstrates end-to-end flow
7. All code has error handling (no `.unwrap()` in production paths)
8. README documents setup, running, and testing instructions
