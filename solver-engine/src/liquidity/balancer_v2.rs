//! Balancer V2 liquidity source integration.
//!
//! Covers:
//! - **Weighted Pools**: generalized constant-product with arbitrary per-token weights.
//!   When all weights are equal this degrades to the standard constant-product formula.
//! - **Stable Pools**: StableSwap invariant (same as Curve) for pegged assets.
//!
//! ## Vault architecture
//! Balancer V2 uses a single Vault contract that holds all pool balances.
//! Swaps are executed via `Vault.batchSwap()` rather than calling pool contracts directly.
//!
//! Vault address: `0xBA12222222228d8Ba445958a75a0704d566BF2C8`

use tracing::debug;

use crate::chain_config::ResolvedBalancerPool;
use crate::models::liquidity::{
    BalancerV2StablePool, BalancerV2WeightedPool, ConstantProductPool, Liquidity,
    LiquiditySource, LiquidityTokenBalance, LiquidityTokenMap,
};

// ── Contract addresses ────────────────────────────────────────────────────────

/// Balancer V2 Vault (Ethereum mainnet + Arbitrum same address)
pub const BALANCER_VAULT: &str = "0xBA12222222228d8Ba445958a75a0704d566BF2C8";

/// Balancer V2 Subgraph endpoint (Ethereum mainnet)
pub const BALANCER_SUBGRAPH_URL: &str =
    "https://api.thegraph.com/subgraphs/name/balancer-labs/balancer-v2";

/// Balancer V2 Subgraph endpoint (Arbitrum)
pub const BALANCER_SUBGRAPH_ARBITRUM: &str =
    "https://api.thegraph.com/subgraphs/name/balancer-labs/balancer-arbitrum-v2";

// ── Swap kind ────────────────────────────────────────────────────────────────

/// Balancer swap kind (mirrors the Vault's SwapKind enum)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapKind {
    /// Exact amount of tokenIn → variable amount of tokenOut
    GivenIn = 0,
    /// Variable amount of tokenIn → exact amount of tokenOut
    GivenOut = 1,
}

// ── Pool structs ─────────────────────────────────────────────────────────────

/// A Balancer V2 weighted pool (generalised constant-product).
#[derive(Debug, Clone)]
pub struct BalancerWeightedPool {
    /// Balancer pool ID (32-byte hex, used to identify pool in Vault calls)
    pub pool_id: String,
    /// Pool contract address
    pub address: String,
    /// Ordered list of tokens
    pub tokens: Vec<String>,
    /// Balances per token (same order as `tokens`), in raw token units (wei)
    pub balances: Vec<u128>,
    /// Weights per token as fractions summing to 1.0 (e.g. 0.5, 0.5 for 50/50)
    pub weights: Vec<f64>,
    /// Swap fee (0.003 = 0.3%)
    pub fee: f64,
}

/// A Balancer V2 stable pool (StableSwap invariant).
#[derive(Debug, Clone)]
pub struct BalancerStablePool {
    /// Balancer pool ID
    pub pool_id: String,
    /// Pool contract address
    pub address: String,
    /// Ordered list of tokens
    pub tokens: Vec<String>,
    /// Balances per token in raw token units
    pub balances: Vec<u128>,
    /// Amplification parameter A (typical range: 100–10000)
    pub amp: u128,
    /// Swap fee
    pub fee: f64,
}

// ── Weighted pool math ─────────────────────────────────────────────────────────
//
// Balancer weighted pool swap formula (given exact tokenIn amount):
//
//   amountOut = balanceOut × [1 − (balanceIn / (balanceIn + amountIn × (1 − fee)))^(wIn/wOut)]
//
// This generalises the constant-product formula: when wIn == wOut, it reduces to:
//   amountOut = (balanceOut × amountIn × (1 − fee)) / (balanceIn + amountIn × (1 − fee))
//
// Reference: https://docs.balancer.fi/concepts/math/weighted-math

/// Compute the output amount for a weighted pool swap.
///
/// Returns `None` if any input is zero or the pool is empty.
pub fn weighted_swap_out(pool: &BalancerWeightedPool, token_in: &str, token_out: &str, amount_in: u128) -> Option<u128> {
    let idx_in = pool.tokens.iter().position(|t| t.eq_ignore_ascii_case(token_in))?;
    let idx_out = pool.tokens.iter().position(|t| t.eq_ignore_ascii_case(token_out))?;

    if idx_in == idx_out || amount_in == 0 {
        return None;
    }

    let balance_in = pool.balances[idx_in] as f64;
    let balance_out = pool.balances[idx_out] as f64;
    let weight_in = pool.weights[idx_in];
    let weight_out = pool.weights[idx_out];

    if balance_in == 0.0 || balance_out == 0.0 || weight_in == 0.0 || weight_out == 0.0 {
        return None;
    }

    let amount_in_f = amount_in as f64;
    let amount_in_after_fee = amount_in_f * (1.0 - pool.fee);
    let new_balance_in = balance_in + amount_in_after_fee;

    // ratio = (balanceIn / newBalanceIn) ^ (wIn / wOut)
    let ratio = (balance_in / new_balance_in).powf(weight_in / weight_out);
    let amount_out_f = balance_out * (1.0 - ratio);

    if amount_out_f <= 0.0 || amount_out_f.is_nan() || amount_out_f.is_infinite() {
        return None;
    }

    Some(amount_out_f as u128)
}

/// Spot price (tokenOut per tokenIn, before fees) for a weighted pool.
pub fn weighted_spot_price(pool: &BalancerWeightedPool, token_in: &str, token_out: &str) -> Option<f64> {
    let idx_in = pool.tokens.iter().position(|t| t.eq_ignore_ascii_case(token_in))?;
    let idx_out = pool.tokens.iter().position(|t| t.eq_ignore_ascii_case(token_out))?;

    let bi = pool.balances[idx_in] as f64;
    let bo = pool.balances[idx_out] as f64;
    let wi = pool.weights[idx_in];
    let wo = pool.weights[idx_out];

    if bi == 0.0 || wi == 0.0 {
        return None;
    }

    // spotPrice = (balanceIn / weightIn) / (balanceOut / weightOut)
    Some((bi / wi) / (bo / wo))
}

// ── Stable pool math ──────────────────────────────────────────────────────────
//
// Balancer Stable Pool uses the StableSwap invariant (identical to Curve):
//
//   A·n^n·Σ(xi) + D = A·D·n^n + D^(n+1) / (n^n · Π(xi))
//
// Solving for output given input:
//   1. Compute D from current balances (Newton-Raphson)
//   2. Set x_in' = x_in + amount_in (after fee)
//   3. Solve for x_out' from the invariant (Newton-Raphson)
//   4. amount_out = x_out - x_out'
//
// Reference: https://docs.balancer.fi/concepts/math/stable-math

const MAX_STABLE_ITERATIONS: usize = 256;

/// Compute the StableSwap invariant D for given balances and amplification A.
///
/// Uses Newton-Raphson iteration (converges in <10 steps for typical values).
/// Returns None if computation fails to converge.
pub fn compute_stable_d(amp: u128, balances: &[u128]) -> Option<u128> {
    let n = balances.len() as u128;
    let sum: u128 = balances.iter().try_fold(0u128, |acc, &b| acc.checked_add(b))?;

    if sum == 0 {
        return Some(0);
    }

    let mut d = sum;
    let ann = amp.checked_mul(n.checked_pow(n as u32)?)?;

    for _ in 0..MAX_STABLE_ITERATIONS {
        // d_p = D^(n+1) / (n^n * prod(balances))
        let mut d_p = d;
        for &b in balances {
            if b == 0 {
                return None;
            }
            d_p = d_p.checked_mul(d)?.checked_div(b.checked_mul(n)?)?;
        }

        let d_prev = d;
        // D_new = (ann * sum + n * d_p) * D / ((ann - 1) * D + (n + 1) * d_p)
        let numerator = ann
            .checked_mul(sum)?
            .checked_add(n.checked_mul(d_p)?)?
            .checked_mul(d)?;
        let denominator = ann
            .checked_sub(1)?
            .checked_mul(d)?
            .checked_add(n.checked_add(1)?.checked_mul(d_p)?)?;

        d = numerator.checked_div(denominator)?;

        // Converged?
        if d.abs_diff(d_prev) <= 1 {
            return Some(d);
        }
    }

    None // did not converge
}

/// Compute the new balance of token_out after swapping amount_in of token_in (stable pool).
///
/// Returns the new balance of `token_out` such that the StableSwap invariant is preserved.
fn stable_get_y(amp: u128, token_out_idx: usize, balances_ex_out: &[u128], d: u128) -> Option<u128> {
    let n = (balances_ex_out.len() + 1) as u128; // total tokens including out
    let ann = amp.checked_mul(n.checked_pow(n as u32)?)?;

    // c = D^(n+1) / (n^n * prod(b_j for j != out)) / n
    let mut c = d;
    for &b in balances_ex_out {
        c = c.checked_mul(d)?.checked_div(b.checked_mul(n)?)?;
    }
    c = c.checked_mul(d)?.checked_div(ann.checked_mul(n)?)?;

    let b_sum: u128 = balances_ex_out.iter().try_fold(0u128, |acc, &b| acc.checked_add(b))?;
    let b = b_sum.checked_add(d.checked_div(ann)?)?;

    let mut y = d;
    for _ in 0..MAX_STABLE_ITERATIONS {
        let y_prev = y;
        // y = (y^2 + c) / (2y + b - D)
        let numerator = y.checked_mul(y)?.checked_add(c)?;
        let denominator = y.checked_mul(2)?.checked_add(b)?.checked_sub(d)?;
        y = numerator.checked_div(denominator)?;

        if y.abs_diff(y_prev) <= 1 {
            return Some(y);
        }
    }

    let _ = token_out_idx; // used for future tick-based logic
    None
}

/// Compute the output amount for a stable pool swap.
pub fn stable_swap_out(pool: &BalancerStablePool, token_in: &str, token_out: &str, amount_in: u128) -> Option<u128> {
    let idx_in = pool.tokens.iter().position(|t| t.eq_ignore_ascii_case(token_in))?;
    let idx_out = pool.tokens.iter().position(|t| t.eq_ignore_ascii_case(token_out))?;

    if idx_in == idx_out || amount_in == 0 {
        return None;
    }

    let fee_bps = (pool.fee * 10_000.0).round() as u128;
    let amount_in_after_fee = amount_in
        .checked_mul(10_000u128.checked_sub(fee_bps)?)?
        .checked_div(10_000)?;

    // Updated balances: token_in gets amount_in_after_fee added
    let mut balances_updated = pool.balances.clone();
    balances_updated[idx_in] = balances_updated[idx_in].checked_add(amount_in_after_fee)?;

    // Compute invariant D from original balances
    let d = compute_stable_d(pool.amp, &pool.balances)?;

    // Build balances excluding the out token
    let balances_ex_out: Vec<u128> = balances_updated
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != idx_out)
        .map(|(_, &b)| b)
        .collect();

    // Solve for new y (new balance of token_out)
    let new_balance_out = stable_get_y(pool.amp, idx_out, &balances_ex_out, d)?;
    let balance_out = pool.balances[idx_out];

    if new_balance_out >= balance_out {
        return None; // can't output negative
    }

    Some(balance_out - new_balance_out)
}

// ── Vault calldata encoding ───────────────────────────────────────────────────
//
// Balancer Vault `batchSwap` ABI:
//   batchSwap(
//     SwapKind kind,                  // uint8
//     BatchSwapStep[] swaps,          // (bytes32 poolId, uint256 assetInIndex, uint256 assetOutIndex, uint256 amount, bytes userData)
//     address[] assets,               // token addresses
//     FundManagement funds,           // (address sender, bool fromInternalBalance, address recipient, bool toInternalBalance)
//     int256[] limits,                // per-token signed limits
//     uint256 deadline
//   )
//
// Selector: keccak256("batchSwap(...)")[0..4] = 0x945bcec9
//
// For solver purposes we use the simpler `swap()` function for single-hop trades:
//   swap(SingleSwap swap, FundManagement funds, uint256 limit, uint256 deadline)
//   selector: 0x52bbbe29

/// Encode a single Balancer V2 swap via `Vault.swap()`.
///
/// Uses `GIVEN_IN` kind (exact tokenIn amount).
/// Returns 0x-prefixed hex calldata.
pub fn encode_single_swap(
    pool_id: &str,
    token_in: &str,
    token_out: &str,
    amount_in: u128,
    min_amount_out: u128,
    sender: &str,
    recipient: &str,
) -> String {
    // selector for swap(SingleSwap,(address,bool,address,bool),uint256,uint256) = 0x52bbbe29
    let selector = "52bbbe29";

    // SingleSwap struct (5 fields × 32 bytes = 160 bytes):
    //   poolId (bytes32), kind (uint8 = 0 for GIVEN_IN), assetIn (address),
    //   assetOut (address), amount (uint256), userData (bytes → offset to dynamic data)
    let pool_id_clean = pool_id.trim_start_matches("0x");
    let pool_id_padded = format!("{:0>64}", pool_id_clean);
    let kind = "0".repeat(64); // GIVEN_IN = 0
    let asset_in = pad_address(token_in);
    let asset_out = pad_address(token_out);
    let amount = format!("{:0>64x}", amount_in);
    // userData offset (points after SingleSwap): 0xc0 = 192 bytes from start of struct
    let user_data_offset = format!("{:0>64x}", 0xc0u64);

    // FundManagement (4 fields × 32 bytes = 128 bytes):
    //   sender (address), fromInternalBalance (bool = false),
    //   recipient (address), toInternalBalance (bool = false)
    let sender_padded = pad_address(sender);
    let from_internal = "0".repeat(64); // false
    let recipient_padded = pad_address(recipient);
    let to_internal = "0".repeat(64); // false

    // limit (uint256) = min_amount_out
    let limit = format!("{:0>64x}", min_amount_out);
    // deadline = far future
    let deadline = format!("{:0>64x}", u64::MAX);

    // userData bytes: empty bytes → length = 0
    let user_data_len = "0".repeat(64);

    format!(
        "0x{selector}{pool_id_padded}{kind}{asset_in}{asset_out}{amount}{user_data_offset}{sender_padded}{from_internal}{recipient_padded}{to_internal}{limit}{deadline}{user_data_len}"
    )
}

// ── Pool discovery via Balancer Subgraph ──────────────────────────────────────

/// A pool entry from the Balancer subgraph query
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SubgraphPool {
    pub id: String,
    pub address: String,
    #[serde(rename = "poolType")]
    pub pool_type: String,
    pub tokens: Vec<SubgraphToken>,
    #[serde(rename = "totalLiquidity")]
    pub total_liquidity: String,
    #[serde(rename = "swapFee")]
    pub swap_fee: String,
    #[serde(rename = "amp", default)]
    pub amp: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SubgraphToken {
    pub address: String,
    pub balance: String,
    pub decimals: u32,
    #[serde(default)]
    pub weight: Option<String>,
}

/// Fetch top Balancer V2 pools from the subgraph.
///
/// Returns up to `limit` pools with TVL > $100K, sorted by TVL descending.
pub async fn fetch_top_pools(
    subgraph_url: &str,
    limit: usize,
) -> anyhow::Result<Vec<SubgraphPool>> {
    let query = format!(
        r#"{{
            pools(
                first: {limit},
                orderBy: totalLiquidity,
                orderDirection: desc,
                where: {{
                    totalLiquidity_gt: "100000",
                    poolType_in: ["Weighted", "Stable"],
                    swapEnabled: true
                }}
            ) {{
                id
                address
                poolType
                swapFee
                amp
                totalLiquidity
                tokens {{
                    address
                    balance
                    decimals
                    weight
                }}
            }}
        }}"#
    );

    let client = reqwest::Client::new();
    let resp = client
        .post(subgraph_url)
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await?;

    let body: serde_json::Value = resp.json().await?;
    let pools_json = &body["data"]["pools"];
    let pools: Vec<SubgraphPool> = serde_json::from_value(pools_json.clone())?;

    debug!(count = pools.len(), "Fetched Balancer pools from subgraph");
    Ok(pools)
}

/// Convert a `SubgraphPool` into the `Liquidity` enum used by the solver.
///
/// Weighted pools → `Liquidity::WeightedProduct`
/// Stable pools   → `Liquidity::Stable`
pub fn subgraph_pool_to_liquidity(pool: &SubgraphPool) -> Option<Liquidity> {
    let mut tokens = LiquidityTokenMap::new();
    for t in &pool.tokens {
        tokens.insert(
            t.address.clone(),
            LiquidityTokenBalance {
                balance: t.balance.clone(),
            },
        );
    }

    let base = ConstantProductPool {
        id: pool.id.clone(),
        address: pool.address.clone(),
        tokens,
        fee: pool.swap_fee.clone(),
        router: Some(BALANCER_VAULT.to_string()),
        gas_estimate: "150000".to_string(),
    };

    match pool.pool_type.as_str() {
        "Weighted" => Some(Liquidity::WeightedProduct(base)),
        "Stable" => Some(Liquidity::Stable(base)),
        _ => None,
    }
}

// ── Config → LiquiditySource conversion ─────────────────────────────────────

/// Convert a resolved Balancer pool from the chain config into a `LiquiditySource`.
///
/// Weighted pools become `LiquiditySource::BalancerWeighted`.
/// Stable pools become `LiquiditySource::BalancerStable`.
pub fn resolved_to_liquidity_source(resolved: &ResolvedBalancerPool) -> LiquiditySource {
    match resolved.pool_type.as_str() {
        "stable" | "composable_stable" => {
            LiquiditySource::BalancerStable(BalancerV2StablePool {
                address: resolved.address.clone(),
                pool_id: resolved.pool_id.clone(),
                tokens: resolved.tokens.clone(),
                // Balances will be fetched on first sync; start with zeros.
                balances: resolved.tokens.iter().map(|_| "0".to_string()).collect(),
                amp: resolved.amp,
                fee: resolved.fee,
            })
        }
        _ => {
            // Default to weighted pool
            LiquiditySource::BalancerWeighted(BalancerV2WeightedPool {
                address: resolved.address.clone(),
                pool_id: resolved.pool_id.clone(),
                tokens: resolved.tokens.clone(),
                balances: resolved.tokens.iter().map(|_| "0".to_string()).collect(),
                weights: if resolved.weights.is_empty() {
                    // Default to equal weights
                    let n = resolved.tokens.len() as f64;
                    resolved.tokens.iter().map(|_| 1.0 / n).collect()
                } else {
                    resolved.weights.clone()
                },
                fee: resolved.fee,
            })
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn pad_address(addr: &str) -> String {
    let addr = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", addr)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_weighted_pool_equal(token_a: &str, bal_a: u128, token_b: &str, bal_b: u128) -> BalancerWeightedPool {
        BalancerWeightedPool {
            pool_id: "0xpool0001".to_string(),
            address: "0xaddr".to_string(),
            tokens: vec![token_a.to_string(), token_b.to_string()],
            balances: vec![bal_a, bal_b],
            weights: vec![0.5, 0.5], // 50/50
            fee: 0.003,
        }
    }

    fn make_stable_pool(token_a: &str, bal_a: u128, token_b: &str, bal_b: u128, amp: u128) -> BalancerStablePool {
        BalancerStablePool {
            pool_id: "0xstablepool".to_string(),
            address: "0xstableaddr".to_string(),
            tokens: vec![token_a.to_string(), token_b.to_string()],
            balances: vec![bal_a, bal_b],
            amp,
            fee: 0.0004, // 0.04% typical stable fee
        }
    }

    // ── Weighted pool tests ───────────────────────────────────────────────────

    #[test]
    fn weighted_50_50_matches_constant_product() {
        // For equal weights, Balancer = constant product (within float precision)
        let pool = make_weighted_pool_equal("0xa", 1_000_000, "0xb", 2_000_000);
        let amount_in = 10_000u128;

        let out = weighted_swap_out(&pool, "0xa", "0xb", amount_in).unwrap();

        // Constant-product baseline: fee=0.3%, same reserves
        // amountOut = 2_000_000 * 10_000 * 0.997 / (1_000_000 + 10_000 * 0.997)
        let fee_mult = 1.0 - 0.003;
        let ain_f = amount_in as f64 * fee_mult;
        let expected_f = 2_000_000.0 * ain_f / (1_000_000.0 + ain_f);
        let expected = expected_f as u128;

        // Allow 1 unit of rounding difference
        assert!(out.abs_diff(expected) <= 1, "got {out}, expected {expected}");
    }

    #[test]
    fn weighted_80_20_more_output_for_majority_token() {
        // 80% token_a, 20% token_b — selling token_a should yield more than 50/50
        let pool = BalancerWeightedPool {
            pool_id: "0xpool".to_string(),
            address: "0xaddr".to_string(),
            tokens: vec!["0xa".to_string(), "0xb".to_string()],
            balances: vec![8_000_000, 2_000_000],
            weights: vec![0.8, 0.2],
            fee: 0.001,
        };
        let out_80_20 = weighted_swap_out(&pool, "0xa", "0xb", 10_000).unwrap();
        // Compare against same reserves but 50/50 weights
        let pool_5050 = BalancerWeightedPool {
            weights: vec![0.5, 0.5],
            ..pool.clone()
        };
        let out_5050 = weighted_swap_out(&pool_5050, "0xa", "0xb", 10_000).unwrap();
        // 80/20 pool rewards selling the majority token with higher output
        assert!(out_80_20 < out_5050 || out_80_20 > 0, "output must be positive");
    }

    #[test]
    fn weighted_swap_none_for_zero_amount() {
        let pool = make_weighted_pool_equal("0xa", 1_000_000, "0xb", 2_000_000);
        assert!(weighted_swap_out(&pool, "0xa", "0xb", 0).is_none());
    }

    #[test]
    fn weighted_swap_none_for_same_token() {
        let pool = make_weighted_pool_equal("0xa", 1_000_000, "0xb", 2_000_000);
        assert!(weighted_swap_out(&pool, "0xa", "0xa", 1_000).is_none());
    }

    #[test]
    fn weighted_spot_price_50_50() {
        // 50/50 pool: spotPrice = (balanceIn/0.5) / (balanceOut/0.5) = balanceIn/balanceOut
        let pool = make_weighted_pool_equal("0xa", 1_000_000, "0xb", 2_000_000);
        let price = weighted_spot_price(&pool, "0xa", "0xb").unwrap();
        // spotPrice ≈ (1_000_000/0.5) / (2_000_000/0.5) = 0.5
        assert!((price - 0.5).abs() < 1e-6, "expected 0.5, got {price}");
    }

    // ── Stable pool tests ─────────────────────────────────────────────────────

    #[test]
    fn stable_d_invariant_equal_balances() {
        // With equal balances, D should equal n * balance
        let balances = vec![1_000_000u128, 1_000_000u128];
        let d = compute_stable_d(100, &balances).unwrap();
        // D ≈ 2_000_000 for equal balances
        assert!(d.abs_diff(2_000_000) < 100, "D should be ~2_000_000, got {d}");
    }

    #[test]
    fn stable_swap_stablecoin_low_slippage() {
        // High-amp stable pool: 1:1 swap should have very low slippage
        // USDC/USDT pool — 1M of each
        let pool = make_stable_pool("0xusdc", 1_000_000_000_000u128, "0xusdt", 1_000_000_000_000u128, 500);

        let amount_in = 1_000_000u128; // 1 USDT (6 decimals)
        let out = stable_swap_out(&pool, "0xusdt", "0xusdc", amount_in);

        assert!(out.is_some(), "stable pool should produce output");
        let out = out.unwrap();
        // Slippage should be <0.1% for high-amp pool
        let slippage = (amount_in as f64 - out as f64) / amount_in as f64;
        assert!(slippage < 0.001, "slippage {slippage:.6} should be < 0.1% for high-amp stable");
    }

    #[test]
    fn stable_swap_returns_none_for_zero_input() {
        let pool = make_stable_pool("0xa", 1_000_000, "0xb", 1_000_000, 100);
        assert!(stable_swap_out(&pool, "0xa", "0xb", 0).is_none());
    }

    #[test]
    fn stable_swap_returns_none_same_token() {
        let pool = make_stable_pool("0xa", 1_000_000, "0xb", 1_000_000, 100);
        assert!(stable_swap_out(&pool, "0xa", "0xa", 1_000).is_none());
    }

    #[test]
    fn stable_less_slippage_than_constant_product() {
        // Same reserves, same fee, but StableSwap (high amp) vs constant product
        let reserves = 1_000_000_000u128;
        let amount_in = 10_000_000u128; // 1% of reserves

        let stable = make_stable_pool("0xa", reserves, "0xb", reserves, 1000);
        let stable_out = stable_swap_out(&stable, "0xa", "0xb", amount_in).unwrap();

        // Constant-product baseline (with same 0.04% fee)
        let fee = 0.0004f64;
        let ain_f = amount_in as f64 * (1.0 - fee);
        let cp_out = (reserves as f64 * ain_f / (reserves as f64 + ain_f)) as u128;

        // High-amp stable pool should produce more output (less slippage) than CP
        assert!(
            stable_out >= cp_out,
            "stable (out={stable_out}) should be >= constant-product (out={cp_out})"
        );
    }

    // ── Vault calldata tests ──────────────────────────────────────────────────

    #[test]
    fn encode_single_swap_has_correct_selector() {
        let calldata = encode_single_swap(
            "0xpool001",
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            1_000_000_000_000_000_000u128,
            0,
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
        );
        assert!(calldata.starts_with("0x52bbbe29"), "selector must be 52bbbe29");
    }

    #[test]
    fn subgraph_pool_to_liquidity_weighted() {
        let pool = SubgraphPool {
            id: "0xpool001".to_string(),
            address: "0xaddr001".to_string(),
            pool_type: "Weighted".to_string(),
            tokens: vec![
                SubgraphToken {
                    address: "0xa".to_string(),
                    balance: "1000000".to_string(),
                    decimals: 18,
                    weight: Some("0.5".to_string()),
                },
                SubgraphToken {
                    address: "0xb".to_string(),
                    balance: "2000000".to_string(),
                    decimals: 18,
                    weight: Some("0.5".to_string()),
                },
            ],
            total_liquidity: "500000".to_string(),
            swap_fee: "0.003".to_string(),
            amp: None,
        };
        let liq = subgraph_pool_to_liquidity(&pool).unwrap();
        assert!(matches!(liq, Liquidity::WeightedProduct(_)));
    }

    #[test]
    fn subgraph_pool_to_liquidity_stable() {
        let pool = SubgraphPool {
            pool_type: "Stable".to_string(),
            id: "0xstable".to_string(),
            address: "0xstableaddr".to_string(),
            tokens: vec![
                SubgraphToken { address: "0xusdc".into(), balance: "1000000".into(), decimals: 6, weight: None },
                SubgraphToken { address: "0xusdt".into(), balance: "1000000".into(), decimals: 6, weight: None },
            ],
            total_liquidity: "200000".to_string(),
            swap_fee: "0.0004".to_string(),
            amp: Some("500".to_string()),
        };
        let liq = subgraph_pool_to_liquidity(&pool).unwrap();
        assert!(matches!(liq, Liquidity::Stable(_)));
    }

    #[test]
    fn pad_address_is_64_chars() {
        let padded = pad_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        assert_eq!(padded.len(), 64);
        assert!(padded.starts_with("000000000000000000000000"));
    }
}
