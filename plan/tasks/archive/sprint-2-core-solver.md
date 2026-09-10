# Sprint 2: Core Solver Logic (Weeks 3-4)

**Goal:** Implement the actual solving logic - find optimal trade execution paths across DEX pools, compute valid solutions with uniform clearing prices, and submit competitive bids. By the end, the solver should win at least some auctions in shadow/staging competition.

**Outcome:** A solver that receives real CoW auctions, routes orders through Uniswap V2/V3 and Sushiswap, produces valid solutions with correct prices and interactions, and can compete in the CoW staging environment.

---

## S2-1: Single-Order Direct Routing

**Description:** Implement the simplest possible solving strategy: for each order in the auction, find the single best pool to fill it. No multi-hop, no splitting, no CoW matching yet - just direct swaps.

**Input:** `AuctionInstance` with orders and `PoolRegistry` with live pool data.

**Output:** `solver-engine/src/solver/direct.rs`

**Algorithm:**

```
for each order in auction.orders:
    1. Get all pools for (order.sell_token, order.buy_token) from registry
    2. For each pool, simulate the swap:
       - Calculate output amount for order.sell_amount
       - Check: output >= order.buy_amount (respects limit price)
       - Calculate surplus = output - order.buy_amount
    3. Pick the pool with the highest surplus
    4. If surplus > 0, create a Trade + Interaction for that pool
    5. Compute uniform clearing prices from the execution
```

**Functions to implement:**

```rust
pub struct DirectSolver {
    pool_registry: Arc<PoolRegistry>,
}

impl DirectSolver {
    /// Solve a single order by finding the best direct pool
    pub fn solve_order(
        &self,
        order: &Order,
        gas_price: U256,
    ) -> Result<Option<OrderSolution>>;

    /// Solve all orders in an auction independently
    pub fn solve_auction(
        &self,
        auction: &AuctionInstance,
    ) -> Result<SolveResponse>;
}

pub struct OrderSolution {
    pub order_uid: String,
    pub pool: Pool,
    pub amount_in: U256,
    pub amount_out: U256,
    pub surplus: U256,
    pub gas_estimate: u64,
}
```

**Acceptance criteria:**
- [ ] Given a WETH->USDC sell order, finds the best pool and returns a valid solution
- [ ] Solution respects limit price (never gives user less than buy_amount)
- [ ] Handles buy orders correctly (user wants exact output, variable input)
- [ ] Returns empty solution for orders with no profitable route
- [ ] Unit tests with mocked pool data (no RPC calls)

---

## S2-2: Uniform Clearing Price Computation

**Description:** CoW Protocol requires Uniform Directional Clearing Prices (UDCP): all orders trading the same token pair in the same direction must receive the same price. Implement price computation that satisfies this constraint.

**Output:** `solver-engine/src/solver/pricing.rs`

**Implementation:**

```rust
/// Compute uniform clearing prices for a set of executed trades
/// Returns a price vector: HashMap<Token, U256> where prices are relative to a numeraire
pub fn compute_clearing_prices(
    trades: &[OrderSolution],
    tokens: &HashMap<Address, TokenInfo>,
) -> Result<HashMap<Address, U256>>;

/// Verify that a solution satisfies UDCP constraint
/// All orders on same (sell_token, buy_token) direction must get same price ratio
pub fn verify_udcp(
    solution: &Solution,
    orders: &[Order],
) -> Result<bool>;

/// Adjust executed amounts to ensure UDCP compliance
/// May reduce surplus slightly to maintain uniform prices
pub fn enforce_udcp(
    trades: &mut [OrderSolution],
) -> Result<()>;
```

**Key rules from CoW Protocol:**
- Prices are denominated in a reference token (ETH or USDC)
- Price vector must be consistent: `price[sell_token] * sell_amount >= price[buy_token] * buy_amount` for each executed order
- All orders trading A->B must face the same price ratio
- The solution score is computed from these prices and executed amounts

**Acceptance criteria:**
- [ ] `compute_clearing_prices` produces valid price vectors for single-pair auctions
- [ ] `verify_udcp` correctly identifies violations
- [ ] `enforce_udcp` adjusts amounts to fix violations without changing which orders are filled
- [ ] Handles multi-pair auctions (ETH->USDC and USDC->DAI in same batch)
- [ ] Edge case: single order auction (prices trivially valid)

---

## S2-3: Interaction Encoding (Swap Calldata)

**Description:** When the solver decides to route an order through a specific pool, it must encode the actual on-chain transaction (the "interaction") that the settlement contract will execute. This means building the calldata for Uniswap V2 `swap()` and V3 `exactInputSingle()`.

**Output:** `solver-engine/src/interactions/mod.rs`, `solver-engine/src/interactions/uniswap_v2.rs`, `solver-engine/src/interactions/uniswap_v3.rs`

**For Uniswap V2:**
```rust
/// Encode a Uniswap V2 Router swap
/// Router address: 0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D
pub fn encode_v2_swap(
    router: Address,
    amount_in: U256,
    amount_out_min: U256,
    path: Vec<Address>,       // [token_in, token_out] or [token_in, intermediate, token_out]
    recipient: Address,       // Settlement contract address
    deadline: U256,
) -> Interaction;
```

**For Uniswap V3:**
```rust
/// Encode a Uniswap V3 SwapRouter exactInputSingle
/// Router address: 0xE592427A0AEce92De3Edee1F18E0157C05861564
pub fn encode_v3_exact_input_single(
    router: Address,
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    amount_out_min: U256,
    sqrt_price_limit_x96: U256,  // 0 for no limit
) -> Interaction;
```

**Key detail:** The `recipient` in all swaps must be the CoW Protocol Settlement contract (`0x9008D19f58AAbD9eD0D60971565AA8510560ab41` on mainnet). The settlement contract orchestrates the execution.

**Acceptance criteria:**
- [ ] Encoded V2 calldata matches what Etherscan shows for real Uniswap swaps
- [ ] Encoded V3 calldata matches real V3 swaps
- [ ] ABI encoding is correct (function selectors, parameter padding)
- [ ] Unit tests with known good calldata from real transactions
- [ ] Settlement contract address is configurable per chain

---

## S2-4: Solution Assembly Pipeline

**Description:** Build the pipeline that takes individual `OrderSolution`s and assembles them into a complete `SolveResponse` with correct prices, trades, interactions, and score.

**Output:** `solver-engine/src/solver/assembler.rs`

**Pipeline:**

```
1. Receive OrderSolutions from DirectSolver
2. Filter out unprofitable solutions (surplus < gas cost)
3. Compute uniform clearing prices
4. Verify UDCP compliance
5. Encode interactions for each trade
6. Compute solution score
7. Assemble SolveResponse
```

```rust
pub struct SolutionAssembler {
    settlement_contract: Address,
    chain_id: u64,
}

impl SolutionAssembler {
    /// Assemble a complete solution from individual order solutions
    pub fn assemble(
        &self,
        order_solutions: Vec<OrderSolution>,
        auction: &AuctionInstance,
    ) -> Result<SolveResponse>;

    /// Compute the score for a solution
    /// Score = sum of (surplus * reference_price) for all trades
    fn compute_score(
        &self,
        trades: &[Trade],
        prices: &HashMap<Address, U256>,
        tokens: &HashMap<Address, TokenInfo>,
    ) -> Score;

    /// Estimate gas cost for the full solution
    fn estimate_gas(
        &self,
        interactions: &[Interaction],
    ) -> u64;

    /// Filter solutions where surplus doesn't cover gas
    fn is_profitable(
        &self,
        solution: &OrderSolution,
        gas_price: U256,
    ) -> bool;
}
```

**Acceptance criteria:**
- [ ] Assembled solution deserializes back correctly (round-trip)
- [ ] Score computation matches CoW Protocol's formula
- [ ] Gas estimation is within 20% of actual (use conservative estimates initially)
- [ ] Unprofitable trades are filtered out
- [ ] Empty auctions return empty solutions (not errors)

---

## S2-5: Multi-Hop Routing

**Description:** Add 2-hop and 3-hop routing. Many token pairs don't have direct pools, but can be routed through intermediaries (e.g., TOKEN->WETH->USDC). This dramatically increases the number of orders the solver can fill.

**Output:** `solver-engine/src/solver/router.rs`

**Common intermediate tokens (Ethereum mainnet):**
- WETH: `0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2`
- USDC: `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48`
- USDT: `0xdAC17F958D2ee523a2206206994597C13D831ec7`
- DAI: `0x6B175474E89094C44Da98b954EedeAC495271d0F`
- WBTC: `0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599`

**Algorithm:**

```
find_best_route(sell_token, buy_token, amount, pools):
    routes = []

    # Direct route
    routes.append(simulate_direct(sell_token, buy_token, amount))

    # 2-hop routes through intermediaries
    for mid in [WETH, USDC, USDT, DAI, WBTC]:
        if mid != sell_token and mid != buy_token:
            route = simulate_2hop(sell_token, mid, buy_token, amount)
            routes.append(route)

    # 3-hop routes (sell -> WETH -> USDC -> buy, etc.)
    for mid1 in [WETH, USDC]:
        for mid2 in [WETH, USDC, DAI]:
            if len(set([sell_token, mid1, mid2, buy_token])) == 4:
                route = simulate_3hop(sell_token, mid1, mid2, buy_token, amount)
                routes.append(route)

    return best(routes, key=output_amount)
```

```rust
pub struct Route {
    pub hops: Vec<Hop>,
    pub total_output: U256,
    pub total_gas: u64,
    pub net_surplus: U256,   // output - gas cost in output token terms
}

pub struct Hop {
    pub pool: Pool,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out: U256,
}

pub fn find_best_route(
    registry: &PoolRegistry,
    sell_token: Address,
    buy_token: Address,
    amount: U256,
    max_hops: usize,        // 2 or 3
    gas_price: U256,
) -> Result<Option<Route>>;
```

**Acceptance criteria:**
- [ ] For TOKEN/USDC pair with no direct pool, finds TOKEN->WETH->USDC route
- [ ] 2-hop route output is within 1% of 1inch API quote for same pair
- [ ] 3-hop routes are only used when they beat 2-hop (net of gas)
- [ ] Gas cost of multi-hop is correctly estimated (more hops = more gas)
- [ ] Performance: < 50ms to find best route across all intermediaries

---

## S2-6: Split Routing

**Description:** For large orders, a single pool may have too much price impact. Split the order across multiple pools/routes to get a better average price.

**Output:** `solver-engine/src/solver/split.rs`

**Algorithm:**

```
split_order(sell_token, buy_token, total_amount, pools):
    # Get all available routes
    routes = find_all_routes(sell_token, buy_token)

    # Binary search for optimal split
    # Start: 100% to best route
    # Try: 80/20 split to top 2 routes
    # Optimize: find split ratio that maximizes total output

    best_split = optimize_split(routes, total_amount)
    return best_split
```

**For Sprint 2, implement simple split only:**
- Split between at most 2 routes
- Use binary search to find optimal split ratio (10% increments)
- Only split if improvement > 0.1% over single-route

**Acceptance criteria:**
- [ ] For a 100 ETH sell order, splitting across V2 and V3 gives better price than either alone
- [ ] Small orders (< 1 ETH) are NOT split (gas cost outweighs benefit)
- [ ] Split ratio is logged for debugging
- [ ] Total executed amounts across splits sum to original order amount

---

## S2-7: Gas Cost Estimation

**Description:** Accurate gas estimation is critical - if the solver overestimates, it won't bid competitively. If it underestimates, it could lose money on execution. Implement per-interaction gas estimation.

**Output:** `solver-engine/src/gas/mod.rs`

**Gas cost table (approximate, for Ethereum mainnet):**

| Operation | Gas Cost |
|-----------|----------|
| Base settlement overhead | 100,000 |
| Per-order overhead | 50,000 |
| Uniswap V2 swap | 110,000 |
| Uniswap V3 swap (single tick) | 130,000 |
| Uniswap V3 swap (per extra tick) | 30,000 |
| Sushiswap swap | 110,000 |
| ERC20 approval | 50,000 |
| WETH wrap/unwrap | 30,000 |

```rust
pub struct GasEstimator {
    base_overhead: u64,
    per_order_overhead: u64,
    estimates: HashMap<InteractionType, u64>,
}

impl GasEstimator {
    /// Estimate total gas for a solution
    pub fn estimate_solution(&self, solution: &Solution) -> u64;

    /// Estimate gas for a single interaction
    pub fn estimate_interaction(&self, interaction: &Interaction) -> u64;

    /// Convert gas to ETH cost
    pub fn gas_to_eth(&self, gas: u64, gas_price: U256) -> U256;

    /// Convert gas cost to token terms (for surplus comparison)
    pub fn gas_cost_in_token(
        &self,
        gas: u64,
        gas_price: U256,
        token: Address,
        eth_price: f64,
    ) -> U256;
}
```

**Acceptance criteria:**
- [ ] Gas estimate for a single V2 swap is within 20% of actual (compare with Etherscan traces)
- [ ] Gas estimate for V3 swap accounts for tick crossings
- [ ] Multi-hop routes correctly sum per-hop gas + overhead
- [ ] `gas_cost_in_token` correctly converts ETH gas cost to USDC/DAI terms

---

## S2-8: Coincidence of Wants (CoW) Matching

**Description:** The signature feature of CoW Protocol: when two orders want opposite trades (Alice sells ETH for USDC, Bob sells USDC for ETH), match them directly without touching any pool. Zero gas for the swap, zero price impact, zero slippage.

**Output:** `solver-engine/src/solver/cow_matching.rs`

**Algorithm:**

```
find_cows(orders):
    # Group orders by token pair
    pairs = group_by(orders, key=(sell_token, buy_token))

    matches = []
    for (token_a, token_b) in pairs:
        sellers_a = pairs[(token_a, token_b)]  # selling A for B
        sellers_b = pairs[(token_b, token_a)]  # selling B for A

        if sellers_a and sellers_b:
            # Find overlapping price range
            # Match at midpoint price (maximizes combined surplus)
            match = compute_cow_match(sellers_a, sellers_b)
            if match.is_valid():
                matches.append(match)

    return matches
```

```rust
pub struct CowMatch {
    pub orders_a: Vec<(String, U256)>,  // (order_uid, fill_amount)
    pub orders_b: Vec<(String, U256)>,
    pub clearing_price_a: U256,
    pub clearing_price_b: U256,
    pub total_surplus: U256,
}

/// Find all CoW matches in an auction
pub fn find_cow_matches(orders: &[Order]) -> Vec<CowMatch>;

/// Compute optimal matching amounts and price for a pair of opposite order groups
pub fn compute_cow_match(
    sellers_a: &[Order],
    sellers_b: &[Order],
) -> Option<CowMatch>;
```

**Acceptance criteria:**
- [ ] Detects a simple 2-order CoW (Alice sells 1 ETH for USDC, Bob sells 3800 USDC for ETH)
- [ ] Clearing price is set at the midpoint of both orders' limit prices
- [ ] Partially fillable orders are correctly handled (fill only what's matchable)
- [ ] CoW matches have zero interactions (no pool swaps needed)
- [ ] CoW matches score higher than equivalent pool-based solutions (no gas cost)

---

## S2-9: Combined Solver Strategy

**Description:** Combine all solving strategies (direct, multi-hop, split, CoW) into a unified solver that tries everything and picks the best combination.

**Output:** `solver-engine/src/solver/mod.rs`, update `solver-engine/src/routes/solve.rs`

**Strategy:**

```
solve(auction):
    solutions = []

    # Strategy 1: CoW matching (cheapest - no gas)
    cow_matches = find_cow_matches(auction.orders)
    if cow_matches:
        solutions.push(build_cow_solution(cow_matches))

    # Strategy 2: Individual order routing (remaining unfilled orders)
    remaining = orders_not_in_cow(auction.orders, cow_matches)
    for order in remaining:
        route = find_best_route(order)
        if route.is_profitable():
            solutions.push(build_routed_solution(order, route))

    # Strategy 3: Combined (CoW + routing for remainder)
    combined = merge_solutions(cow_solution, routed_solutions)
    solutions.push(combined)

    # Return up to 3 solutions ranked by score
    return top_n(solutions, 3)
```

**Acceptance criteria:**
- [ ] Solver returns multiple solution candidates (driver picks best)
- [ ] CoW matches are always preferred over pool routing when available
- [ ] Remaining orders after CoW matching are still routed through pools
- [ ] Combined solutions have correct UDCP prices across all trades
- [ ] Solver respects `MAX_SOLVE_TIME_MS` deadline (returns best found so far)

---

## S2-10: Shadow Competition Testing

**Description:** Connect to the CoW Protocol shadow competition (a test environment where solutions are scored but not executed on-chain). Validate that our solver produces valid, competitive solutions.

**Output:** `scripts/shadow_test.sh`, configuration updates

**Steps:**

1. Contact CoW Solvers Telegram group to get shadow competition access
2. Configure solver endpoint to be accessible (ngrok for local testing, or deploy to VPS)
3. Run solver and monitor logs for incoming auctions
4. Track: auctions received, solutions submitted, score vs winning score

**Metrics to track:**

```rust
pub struct CompetitionMetrics {
    pub auctions_received: u64,
    pub solutions_submitted: u64,
    pub solutions_valid: u64,         // passed CoW validation
    pub solutions_invalid: u64,       // rejected by CoW
    pub solutions_winning: u64,       // would have won
    pub avg_score: f64,
    pub avg_winning_score: f64,
    pub score_gap_pct: f64,           // how far from winning
}
```

**Acceptance criteria:**
- [ ] Solver receives live auctions from shadow competition
- [ ] At least 50% of submitted solutions pass validation
- [ ] Invalid solutions are logged with rejection reason for debugging
- [ ] Competition metrics are tracked and logged every hour
- [ ] Score gap analysis identifies which order types we're losing on

---

## S2-11: Token Approval Management

**Description:** Before the settlement contract can move tokens through DEXs, the appropriate approvals must exist. Implement approval checking and encoding.

**Output:** `solver-engine/src/interactions/approvals.rs`

```rust
/// Check if settlement contract has approval to spend tokens on a router
pub async fn check_approval(
    client: &EthClient,
    token: Address,
    owner: Address,      // Settlement contract
    spender: Address,    // DEX router
) -> Result<U256>;

/// Encode an ERC20 approve interaction
pub fn encode_approval(
    token: Address,
    spender: Address,
    amount: U256,
) -> Interaction;

/// For a solution, compute all required approvals and prepend them
pub fn prepend_approvals(
    solution: &mut Solution,
    required: Vec<(Address, Address, U256)>,  // (token, spender, amount)
);
```

**Acceptance criteria:**
- [ ] Correctly identifies when approval is needed vs already exists
- [ ] Approval interactions are placed BEFORE swap interactions
- [ ] Uses `type(uint256).max` for approvals (one-time, gas efficient)
- [ ] Gas estimate includes approval costs

---

## Sprint 2 Definition of Done

1. Solver finds optimal routes across Uniswap V2, V3, and Sushiswap (direct and multi-hop)
2. Solutions have correct uniform clearing prices
3. Interactions encode valid swap calldata
4. CoW matching works for opposite orders
5. Combined strategy tries all approaches and returns best solutions
6. Gas estimation is within 20% of actual
7. Solver connected to shadow competition and producing valid solutions
8. At least 50% of solutions pass CoW Protocol validation
9. All code has unit tests; integration tests against mainnet fork
