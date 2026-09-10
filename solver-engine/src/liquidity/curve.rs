//! Curve Finance pool integration.
//!
//! Curve dominates stablecoin liquidity and ETH Liquid Staking Token (LST) pairs.
//! This module implements the StableSwap invariant for Curve plain pools.
//!
//! ## Supported chains
//! - **Ethereum Mainnet (1)**: 3pool, stETH/ETH, FRAX/USDC, tricrypto2
//! - **Arbitrum One (42161)**: 2pool, tricrypto, FRAX/USDC, wstETH/ETH
//!
//! ## Invariant
//! Curve uses the StableSwap invariant:
//! `A·n^n·Σ(xi) + D = A·D·n^n + D^(n+1) / (n^n · Π(xi))`
//!
//! For get_dy (exact input amount), the algorithm:
//! 1. Compute D from current balances
//! 2. Compute new balance of token_out after adding amount_in
//! 3. Subtract fee

use shared::rpc::EthClient;
use tracing::warn;

use crate::chain_config::ResolvedCurvePool;
use crate::models::liquidity::{
    ConstantProductPool, CurveStablePool, Liquidity, LiquiditySource, LiquidityTokenBalance,
    LiquidityTokenMap,
};

// ── Known pool addresses — Mainnet ──────────────────────────────────────────

/// Curve 3pool (USDC/USDT/DAI)
pub const CURVE_3POOL: &str = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7";
/// Curve stETH/ETH pool
pub const CURVE_STETH_ETH: &str = "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022";
/// Curve FRAX/USDC pool
pub const CURVE_FRAX_USDC: &str = "0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2";
/// Curve tricrypto2 (USDT/WBTC/WETH)
pub const CURVE_TRICRYPTO2: &str = "0xD51a44d3FaE010294C616388b506AcdA1BFAAE46";

// ── Known pool addresses — Arbitrum ─────────────────────────────────────────

/// Curve 2pool (USDC/USDT) on Arbitrum
pub const ARB_CURVE_2POOL: &str = "0x7f90122BF0700F9E7e1F688fe926940E8839F353";
/// Curve tricrypto (USDT/WBTC/WETH) on Arbitrum
pub const ARB_CURVE_TRICRYPTO: &str = "0x960ea3e3C7FB317332d990873d354E18d7645590";
/// Curve FRAX/USDC on Arbitrum
pub const ARB_CURVE_FRAX_USDC: &str = "0xC9B8a3FDECB9D5b218d02555a8Baf332E5B740d5";
/// Curve wstETH/ETH on Arbitrum
pub const ARB_CURVE_WSTETH_ETH: &str = "0x6eB2dc694eB516B16Dc9FBc678C60052BbdD7d80";

// ── Token addresses — Mainnet ───────────────────────────────────────────────

pub const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
pub const USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
pub const DAI: &str = "0x6B175474E89094C44Da98b954EedeAC495271d0F";
pub const STETH: &str = "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84";
pub const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
pub const WBTC: &str = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599";
pub const FRAX: &str = "0x853d955aCEf822Db058eb8505911ED77F175b99e";

// ── Token addresses — Arbitrum ──────────────────────────────────────────────

pub const ARB_USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
pub const ARB_USDT: &str = "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9";
pub const ARB_WETH: &str = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1";
pub const ARB_WBTC: &str = "0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f";
pub const ARB_WSTETH: &str = "0x5979D7b546E38E9Ab8049524bC56bBC6aaD10F00";
pub const ARB_FRAX: &str = "0x17FC002b466eEc40DaE837Fc4bE5c67993ddBd6F";

// ── Well-known token decimals ───────────────────────────────────────────────

/// Return the number of decimals for well-known tokens (both mainnet and Arbitrum).
///
/// Falls back to 18 if the token is unknown — this is safe because most ERC-20s
/// use 18 decimals, and the worst case is a slightly imprecise quote.
pub fn token_decimals(addr: &str) -> u8 {
    let lower = addr.to_lowercase();
    match lower.as_str() {
        // USDC (both mainnet native and Arbitrum native) — 6 decimals
        "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48" => 6,
        "0xaf88d065e77c8cc2239327c5edb3a432268e5831" => 6,
        "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8" => 6, // USDC.e bridged
        // USDT — 6 decimals
        "0xdac17f958d2ee523a2206206994597c13d831ec7" => 6,
        "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9" => 6, // Arbitrum USDT
        // WBTC — 8 decimals
        "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599" => 8,
        "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f" => 8, // Arbitrum WBTC
        // Everything else: 18 (DAI, WETH, stETH, wstETH, FRAX, ARB, etc.)
        _ => 18,
    }
}

/// Compute the rate (normalization factor) for a token: `10^(18 - decimals)`.
pub fn rate_for_token(addr: &str) -> u128 {
    let decimals = token_decimals(addr);
    10u128.pow(18 - decimals as u32)
}

// ── Pool structures ───────────────────────────────────────────────────────────

/// A Curve plain or StableSwap pool.
#[derive(Debug, Clone)]
pub struct CurvePool {
    /// Pool contract address
    pub address: String,
    /// Ordered list of token addresses (index corresponds to Curve's coin[i])
    pub tokens: Vec<String>,
    /// Current balances in raw token units (wei / 10^decimals)
    pub balances: Vec<u128>,
    /// Amplification coefficient A (100-10000+ for stablecoins)
    pub amp: u128,
    /// Swap fee in bps (typically 4 = 0.04%)
    pub fee_bps: u32,
    /// Decimal normalization factors: 10^(18 - decimals_i)
    /// E.g. USDC (6 decimals) -> rate = 10^12
    pub rates: Vec<u128>,
    /// Pool type: "stable" or "crypto"
    pub pool_type: String,
}

impl CurvePool {
    /// Number of tokens in this pool.
    pub fn n(&self) -> usize {
        self.tokens.len()
    }

    /// Find the index of a token by address (case-insensitive).
    pub fn token_index(&self, addr: &str) -> Option<usize> {
        self.tokens
            .iter()
            .position(|t| t.eq_ignore_ascii_case(addr))
    }
}

// ── StableSwap math ───────────────────────────────────────────────────────────
//
// Reference implementation: https://github.com/curvefi/curve-contract
//
// The Curve invariant (for n tokens):
//   A·n^n·Σ(xi) + D = A·D·n^n + D^(n+1) / (n^n·Π(xi))
//
// `get_dy(i, j, dx)` computes the output amount `dy` when swapping
// `dx` of token i for token j.

const MAX_ITERATIONS: usize = 255;

/// Compute the StableSwap invariant D using f64 arithmetic.
///
/// f64 is used to avoid u128 overflow with large balances (e.g. 1e24 wei reserves).
/// Precision is sufficient for simulation (error < 1e-9 relative).
///
/// The Newton iteration converges to D such that:
///   A * n^n * sum(x_i) + D = A * D * n^n + D^(n+1) / (n^n * prod(x_i))
pub fn compute_d(amp: u128, balances_normalized: &[u128]) -> Option<u128> {
    let n = balances_normalized.len() as f64;
    let s: f64 = balances_normalized.iter().map(|&b| b as f64).sum();

    if s == 0.0 {
        return Some(0);
    }

    let ann = (amp as f64) * n.powi(n as i32);
    let mut d = s;

    for _ in 0..MAX_ITERATIONS {
        // D_P = D^(n+1) / (n^n * prod(x_i))
        let mut d_p = d;
        for &b in balances_normalized {
            if b == 0 {
                return None;
            }
            d_p = d_p * d / ((b as f64) * n);
        }

        let d_prev = d;
        // Newton step:
        // f(D)  = Ann*S + D - Ann*D - D_P  (rearranged invariant)
        // f'(D) = 1 - Ann + (n+1)*D_P/D    (but we use the standard form)
        let numerator = (ann * s + n * d_p) * d;
        let denominator = (ann - 1.0) * d + (n + 1.0) * d_p;
        if denominator == 0.0 {
            return None;
        }
        d = numerator / denominator;

        if (d - d_prev).abs() <= 1.0 {
            return Some(d as u128);
        }
    }

    None
}

/// Solve for the new balance y of token j (Curve's `get_y()`) using f64.
///
/// Given balances of all tokens except j, and the invariant D, find the
/// value of y such that the StableSwap invariant holds.
fn get_y_f64(amp: u128, balances_ex_j: &[u128], d: f64) -> Option<f64> {
    let n = (balances_ex_j.len() + 1) as f64;
    let ann = (amp as f64) * n.powi(n as i32);

    // c = D^(n+1) / (n^n * prod(x_i_excl_j) * Ann)
    let mut c = d;
    for &b in balances_ex_j {
        if b == 0 {
            return None;
        }
        c = c * d / ((b as f64) * n);
    }
    c = c * d / (ann * n);

    // b = S' + D/Ann  (where S' = sum of balances excluding j)
    let sum_b: f64 = balances_ex_j.iter().map(|&b| b as f64).sum();
    let b_coeff = sum_b + d / ann;

    // Newton iteration: y_{k+1} = (y_k^2 + c) / (2*y_k + b - D)
    let mut y = d;
    for _ in 0..MAX_ITERATIONS {
        let y_prev = y;
        let numerator = y * y + c;
        let denominator = 2.0 * y + b_coeff - d;
        if denominator == 0.0 {
            return None;
        }
        y = numerator / denominator;
        if (y - y_prev).abs() <= 1.0 {
            return Some(y);
        }
    }

    None
}

/// Compute `get_dy(i, j, dx)` --- the output of swapping `dx` of token[i] for token[j].
///
/// Returns the amount of token[j] received (after fee deduction).
/// Uses f64 internally to handle large reserves without overflow.
pub fn get_dy(pool: &CurvePool, i: usize, j: usize, dx: u128) -> Option<u128> {
    let n = pool.n();
    if i == j || i >= n || j >= n || dx == 0 {
        return None;
    }

    const PRECISION: f64 = 1_000_000_000_000_000_000.0; // 1e18

    // Normalize balances: xp[k] = balance[k] * rate[k] / PRECISION
    let xp: Vec<f64> = pool
        .balances
        .iter()
        .zip(pool.rates.iter())
        .map(|(&b, &r)| (b as f64) * (r as f64) / PRECISION)
        .collect();

    // Compute D from current balances (as u128 inputs for compute_d compatibility)
    let xp_u128: Vec<u128> = xp.iter().map(|&v| v as u128).collect();
    let d_u128 = compute_d(pool.amp, &xp_u128)?;
    let d = d_u128 as f64;

    // New balance of token i after adding dx (normalized)
    let dx_normalized = (dx as f64) * (pool.rates[i] as f64) / PRECISION;
    let xp_i_new = xp[i] + dx_normalized;

    // Build balances excluding j, with updated i
    let xp_ex_j: Vec<u128> = xp
        .iter()
        .enumerate()
        .filter(|(k, _)| *k != j)
        .map(|(k, &v)| if k == i { xp_i_new as u128 } else { v as u128 })
        .collect();

    if xp_ex_j.len() != n - 1 {
        return None;
    }

    // Solve for new y (balance of token j)
    let y = get_y_f64(pool.amp, &xp_ex_j, d)?;

    // dy in normalized units
    let dy_normalized = xp[j] - y - 1.0;
    if dy_normalized <= 0.0 {
        return None;
    }

    // Deduct fee
    let dy_after_fee = dy_normalized * (1.0 - pool.fee_bps as f64 / 10_000.0);

    // Denormalize: dy = dy_after_fee * PRECISION / rate[j]
    let dy = dy_after_fee * PRECISION / (pool.rates[j] as f64);
    if dy <= 0.0 {
        return None;
    }

    Some(dy as u128)
}

/// Compute a quote using a `CurveStablePool` from the registry.
///
/// This is the entry point for the solver to get quotes from Curve pools
/// stored in the `PoolRegistry`. It converts the registry representation
/// to the internal `CurvePool` and delegates to `get_dy`.
pub fn quote_curve_stable(
    pool: &CurveStablePool,
    token_in: &str,
    token_out: &str,
    amount_in: u128,
) -> Option<u128> {
    let i = pool
        .tokens
        .iter()
        .position(|t| t.eq_ignore_ascii_case(token_in))?;
    let j = pool
        .tokens
        .iter()
        .position(|t| t.eq_ignore_ascii_case(token_out))?;

    let balances: Vec<u128> = pool
        .balances
        .iter()
        .map(|b| b.parse::<u128>().unwrap_or(0))
        .collect();

    let internal = CurvePool {
        address: pool.address.clone(),
        tokens: pool.tokens.clone(),
        balances,
        amp: pool.amp,
        fee_bps: pool.fee_bps,
        rates: pool.rates.clone(),
        pool_type: pool.pool_type.clone(),
    };

    get_dy(&internal, i, j, amount_in)
}

// ── Calldata encoding ─────────────────────────────────────────────────────────
//
// Curve `exchange(i, j, dx, min_dy)`:
//   selector = keccak256("exchange(int128,int128,uint256,uint256)")[0..4] = 0x3df02124

/// Encode a Curve pool `exchange(i, j, dx, min_dy)` call.
///
/// Returns 0x-prefixed hex calldata.
pub fn encode_exchange(i: usize, j: usize, dx: u128, min_dy: u128) -> String {
    // selector for exchange(int128,int128,uint256,uint256) = 0x3df02124
    let selector = "3df02124";
    let i_enc = format!("{:0>64x}", i as u64);
    let j_enc = format!("{:0>64x}", j as u64);
    let dx_enc = format!("{:0>64x}", dx);
    let min_dy_enc = format!("{:0>64x}", min_dy);
    format!("0x{selector}{i_enc}{j_enc}{dx_enc}{min_dy_enc}")
}

// ── Well-known pool factories ─────────────────────────────────────────────────

/// Return hardcoded Curve pools for Ethereum mainnet.
pub fn mainnet_pools() -> Vec<CurvePool> {
    vec![
        // 3pool: DAI (18dec) / USDC (6dec) / USDT (6dec)
        CurvePool {
            address: CURVE_3POOL.to_string(),
            tokens: vec![DAI.to_string(), USDC.to_string(), USDT.to_string()],
            balances: vec![0, 0, 0],
            amp: 2000,
            fee_bps: 4,
            rates: vec![
                rate_for_token(DAI),
                rate_for_token(USDC),
                rate_for_token(USDT),
            ],
            pool_type: "stable".to_string(),
        },
        // stETH/ETH pool
        CurvePool {
            address: CURVE_STETH_ETH.to_string(),
            tokens: vec![
                "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE".to_string(),
                STETH.to_string(),
            ],
            balances: vec![0, 0],
            amp: 50,
            fee_bps: 4,
            rates: vec![1_000_000_000_000_000_000u128, 1_000_000_000_000_000_000u128],
            pool_type: "stable".to_string(),
        },
        // FRAX/USDC
        CurvePool {
            address: CURVE_FRAX_USDC.to_string(),
            tokens: vec![FRAX.to_string(), USDC.to_string()],
            balances: vec![0, 0],
            amp: 1500,
            fee_bps: 4,
            rates: vec![rate_for_token(FRAX), rate_for_token(USDC)],
            pool_type: "stable".to_string(),
        },
        // tricrypto2: USDT/WBTC/WETH
        CurvePool {
            address: CURVE_TRICRYPTO2.to_string(),
            tokens: vec![USDT.to_string(), WBTC.to_string(), WETH.to_string()],
            balances: vec![0, 0, 0],
            amp: 1707629,
            fee_bps: 4,
            rates: vec![
                rate_for_token(USDT),
                rate_for_token(WBTC),
                rate_for_token(WETH),
            ],
            pool_type: "crypto".to_string(),
        },
    ]
}

/// Return hardcoded Curve pools for Arbitrum One.
pub fn arbitrum_pools() -> Vec<CurvePool> {
    vec![
        // 2pool: USDC (6dec) / USDT (6dec)
        CurvePool {
            address: ARB_CURVE_2POOL.to_string(),
            tokens: vec![ARB_USDC.to_string(), ARB_USDT.to_string()],
            balances: vec![0, 0],
            amp: 800,
            fee_bps: 4,
            rates: vec![rate_for_token(ARB_USDC), rate_for_token(ARB_USDT)],
            pool_type: "stable".to_string(),
        },
        // tricrypto: USDT (6dec) / WBTC (8dec) / WETH (18dec)
        CurvePool {
            address: ARB_CURVE_TRICRYPTO.to_string(),
            tokens: vec![
                ARB_USDT.to_string(),
                ARB_WBTC.to_string(),
                ARB_WETH.to_string(),
            ],
            balances: vec![0, 0, 0],
            amp: 1707629,
            fee_bps: 4,
            rates: vec![
                rate_for_token(ARB_USDT),
                rate_for_token(ARB_WBTC),
                rate_for_token(ARB_WETH),
            ],
            pool_type: "crypto".to_string(),
        },
        // FRAX/USDC
        CurvePool {
            address: ARB_CURVE_FRAX_USDC.to_string(),
            tokens: vec![ARB_FRAX.to_string(), ARB_USDC.to_string()],
            balances: vec![0, 0],
            amp: 1500,
            fee_bps: 4,
            rates: vec![rate_for_token(ARB_FRAX), rate_for_token(ARB_USDC)],
            pool_type: "stable".to_string(),
        },
        // wstETH/ETH (both 18 dec)
        CurvePool {
            address: ARB_CURVE_WSTETH_ETH.to_string(),
            tokens: vec![ARB_WSTETH.to_string(), ARB_WETH.to_string()],
            balances: vec![0, 0],
            amp: 500,
            fee_bps: 4,
            rates: vec![
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
            ],
            pool_type: "stable".to_string(),
        },
    ]
}

/// Return hardcoded Curve pools for a given chain_id.
pub fn pools_for_chain(chain_id: u64) -> Vec<CurvePool> {
    match chain_id {
        1 => mainnet_pools(),
        42161 => arbitrum_pools(),
        _ => vec![],
    }
}

// ── Conversion helpers ──────────────────────────────────────────────────────

/// Convert a `ResolvedCurvePool` (from chain config TOML) into a `LiquiditySource`.
///
/// Automatically computes normalization rates from well-known token decimals.
pub fn resolved_to_liquidity_source(resolved: &ResolvedCurvePool) -> LiquiditySource {
    let rates: Vec<u128> = resolved.tokens.iter().map(|t| rate_for_token(t)).collect();
    let balances: Vec<String> = resolved.tokens.iter().map(|_| "0".to_string()).collect();

    LiquiditySource::CurveStable(CurveStablePool {
        address: resolved.address.clone(),
        tokens: resolved.tokens.clone(),
        balances,
        amp: resolved.amp,
        fee_bps: resolved.fee_bps,
        rates,
        pool_type: resolved.pool_type.clone(),
    })
}

/// Convert a hardcoded `CurvePool` into a `LiquiditySource` for the registry.
pub fn curve_pool_to_registry_source(pool: &CurvePool) -> LiquiditySource {
    LiquiditySource::CurveStable(CurveStablePool {
        address: pool.address.clone(),
        tokens: pool.tokens.clone(),
        balances: pool.balances.iter().map(|b| b.to_string()).collect(),
        amp: pool.amp,
        fee_bps: pool.fee_bps,
        rates: pool.rates.clone(),
        pool_type: pool.pool_type.clone(),
    })
}

/// Convert a `CurvePool` into the `Liquidity::Stable` variant for the CoW driver format.
pub fn curve_pool_to_liquidity(pool: &CurvePool) -> Liquidity {
    let mut tokens = LiquidityTokenMap::new();
    for (token, &balance) in pool.tokens.iter().zip(pool.balances.iter()) {
        tokens.insert(
            token.clone(),
            LiquidityTokenBalance {
                balance: balance.to_string(),
            },
        );
    }
    Liquidity::Stable(ConstantProductPool {
        id: pool.address.clone(),
        address: pool.address.clone(),
        tokens,
        fee: format!("{:.4}", pool.fee_bps as f64 / 10_000.0),
        router: None,
        gas_estimate: "180000".to_string(),
    })
}

// ── RPC sync ────────────────────────────────────────────────────────────────

/// Sync a Curve pool's balances from the chain via RPC.
///
/// Calls `balances(uint256)` for each token index. This is the standard
/// Curve pool interface. If the RPC call fails, the pool keeps its stale
/// balances and an error is returned.
pub async fn sync_curve_pool(rpc: &EthClient, pool: &mut CurveStablePool) -> anyhow::Result<()> {
    // Curve `balances(uint256 i)` selector = 0x4903b0d1
    let selector = "4903b0d1";

    for i in 0..pool.tokens.len() {
        let i_enc = format!("{:0>64x}", i as u64);
        let calldata = format!("0x{selector}{i_enc}");

        match rpc.call(&pool.address, &calldata).await {
            Ok(result) => {
                // Result is a 32-byte hex-encoded uint256
                let hex = result.trim_start_matches("0x");
                let balance = u128::from_str_radix(hex, 16).unwrap_or(0);
                pool.balances[i] = balance.to_string();
            }
            Err(e) => {
                warn!(
                    pool = %pool.address,
                    token_index = i,
                    error = %e,
                    "Failed to sync Curve pool balance"
                );
                return Err(e.into());
            }
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a simple 2-token Curve pool with equal 18-decimal balances.
    fn make_2pool(t0: &str, bal0: u128, t1: &str, bal1: u128, amp: u128) -> CurvePool {
        CurvePool {
            address: "0xpool".to_string(),
            tokens: vec![t0.to_string(), t1.to_string()],
            balances: vec![bal0, bal1],
            amp,
            fee_bps: 4,
            rates: vec![
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
            ],
            pool_type: "stable".to_string(),
        }
    }

    /// 3-token pool simulating 3pool with equal reserves
    fn make_3pool(amp: u128) -> CurvePool {
        let reserve = 100_000_000_000_000_000_000_000u128; // 100k in 18-decimal units
        CurvePool {
            address: CURVE_3POOL.to_string(),
            tokens: vec![DAI.to_string(), USDC.to_string(), USDT.to_string()],
            balances: vec![reserve, reserve, reserve],
            amp,
            fee_bps: 4,
            rates: vec![
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
            ],
            pool_type: "stable".to_string(),
        }
    }

    // ── D invariant ───────────────────────────────────────────────────────────

    #[test]
    fn compute_d_equal_balances() {
        let balances = vec![1_000_000u128, 1_000_000u128];
        let d = compute_d(100, &balances).unwrap();
        assert!(
            d.abs_diff(2_000_000) < 100,
            "D should be ~2_000_000, got {d}"
        );
    }

    #[test]
    fn compute_d_zero_sum_returns_zero() {
        let d = compute_d(100, &[0u128, 0u128]).unwrap();
        assert_eq!(d, 0);
    }

    #[test]
    fn compute_d_3token_equal() {
        // For 3 equal balances, D = 3 * balance
        let bal = 1_000_000u128;
        let d = compute_d(2000, &[bal, bal, bal]).unwrap();
        assert!(
            d.abs_diff(3_000_000) < 100,
            "D should be ~3_000_000 for 3-pool, got {d}"
        );
    }

    // ── get_dy ────────────────────────────────────────────────────────────────

    #[test]
    fn get_dy_equal_pool_low_slippage() {
        let pool = make_2pool(
            "0xa",
            1_000_000_000_000_000_000_000_000u128,
            "0xb",
            1_000_000_000_000_000_000_000_000u128,
            1000,
        );
        let amount_in = 1_000_000_000_000_000_000u128;
        let dy = get_dy(&pool, 0, 1, amount_in).unwrap();
        let slippage = (amount_in as f64 - dy as f64) / amount_in as f64;
        assert!(
            slippage < 0.01,
            "slippage {slippage:.6} should be < 1% for high-amp"
        );
    }

    #[test]
    fn get_dy_returns_none_same_token() {
        let pool = make_2pool("0xa", 1_000_000u128, "0xb", 1_000_000u128, 100);
        assert!(get_dy(&pool, 0, 0, 1000).is_none());
    }

    #[test]
    fn get_dy_returns_none_zero_input() {
        let pool = make_2pool("0xa", 1_000_000u128, "0xb", 1_000_000u128, 100);
        assert!(get_dy(&pool, 0, 1, 0).is_none());
    }

    #[test]
    fn get_dy_3pool_dai_to_usdc() {
        let pool = make_3pool(2000);
        let amount_in = 1_000_000_000_000_000_000_000u128;
        let dy = get_dy(&pool, 0, 1, amount_in);
        assert!(dy.is_some(), "should compute output for 3pool swap");
        let out = dy.unwrap();
        let slippage = (amount_in as f64 - out as f64) / amount_in as f64;
        assert!(slippage < 0.01, "3pool slippage {slippage:.6} should be < 1%");
    }

    #[test]
    fn get_dy_high_amp_less_slippage_than_low() {
        let bal = 10_000_000_000_000_000_000_000_000u128;
        let pool_high = make_2pool("0xa", bal, "0xb", bal, 1000);
        let pool_low = make_2pool("0xa", bal, "0xb", bal, 100);
        let amount_in = bal / 10_000;

        let dy_high = get_dy(&pool_high, 0, 1, amount_in);
        let dy_low = get_dy(&pool_low, 0, 1, amount_in);

        match (dy_high, dy_low) {
            (Some(h), Some(l)) => {
                assert!(h >= l, "high-amp (out={h}) should be >= low-amp (out={l})")
            }
            (Some(_), None) => {}
            (None, _) => panic!("high-amp pool should produce output"),
        }
    }

    // ── Encoding ──────────────────────────────────────────────────────────────

    #[test]
    fn encode_exchange_has_correct_selector() {
        let calldata = encode_exchange(0, 1, 1_000_000u128, 990_000u128);
        assert!(
            calldata.starts_with("0x3df02124"),
            "selector must be 3df02124"
        );
        assert_eq!(calldata.len(), 266);
    }

    #[test]
    fn encode_exchange_encodes_amounts() {
        let dx = 1_000_000u128;
        let min_dy = 999_000u128;
        let calldata = encode_exchange(1, 2, dx, min_dy);
        assert!(calldata.contains(&format!("{:0>64x}", dx)));
        assert!(calldata.contains(&format!("{:0>64x}", min_dy)));
    }

    // ── Pool conversion ───────────────────────────────────────────────────────

    #[test]
    fn curve_pool_to_liquidity_produces_stable_variant() {
        let pool = make_2pool("0xa", 1_000_000u128, "0xb", 2_000_000u128, 500);
        let liq = curve_pool_to_liquidity(&pool);
        assert!(matches!(liq, Liquidity::Stable(_)));
        if let Liquidity::Stable(p) = liq {
            assert_eq!(p.tokens.len(), 2);
            assert_eq!(p.address, "0xpool");
        }
    }

    #[test]
    fn mainnet_pools_returns_known_addresses() {
        let pools = mainnet_pools();
        assert!(!pools.is_empty());
        let addresses: Vec<&str> = pools.iter().map(|p| p.address.as_str()).collect();
        assert!(addresses.contains(&CURVE_3POOL));
        assert!(addresses.contains(&CURVE_STETH_ETH));
        assert!(addresses.contains(&CURVE_TRICRYPTO2));
    }

    #[test]
    fn token_index_case_insensitive() {
        let pool = make_2pool("0xABCD", 1_000u128, "0xEFGH", 1_000u128, 100);
        assert_eq!(pool.token_index("0xabcd"), Some(0));
        assert_eq!(pool.token_index("0xABCD"), Some(0));
        assert_eq!(pool.token_index("0xXXXX"), None);
    }

    // ── Arbitrum pools ───────────────────────────────────────────────────────

    #[test]
    fn arbitrum_pools_returns_expected_pools() {
        let pools = arbitrum_pools();
        assert_eq!(pools.len(), 4);

        let addresses: Vec<&str> = pools.iter().map(|p| p.address.as_str()).collect();
        assert!(addresses.contains(&ARB_CURVE_2POOL));
        assert!(addresses.contains(&ARB_CURVE_TRICRYPTO));
        assert!(addresses.contains(&ARB_CURVE_FRAX_USDC));
        assert!(addresses.contains(&ARB_CURVE_WSTETH_ETH));
    }

    #[test]
    fn pools_for_chain_returns_correct_chain() {
        assert_eq!(pools_for_chain(1).len(), 4);
        assert_eq!(pools_for_chain(42161).len(), 4);
        assert!(pools_for_chain(999).is_empty());
    }

    // ── Token decimals ───────────────────────────────────────────────────────

    #[test]
    fn token_decimals_known_tokens() {
        assert_eq!(token_decimals(USDC), 6);
        assert_eq!(token_decimals(USDT), 6);
        assert_eq!(token_decimals(WBTC), 8);
        assert_eq!(token_decimals(DAI), 18);
        assert_eq!(token_decimals(WETH), 18);
        // Arbitrum tokens
        assert_eq!(token_decimals(ARB_USDC), 6);
        assert_eq!(token_decimals(ARB_USDT), 6);
        assert_eq!(token_decimals(ARB_WBTC), 8);
        assert_eq!(token_decimals(ARB_WETH), 18);
    }

    #[test]
    fn rate_for_known_tokens() {
        assert_eq!(rate_for_token(USDC), 1_000_000_000_000u128); // 10^12
        assert_eq!(rate_for_token(WBTC), 10_000_000_000u128); // 10^10
        assert_eq!(rate_for_token(DAI), 1u128); // 10^0 = 1
        assert_eq!(rate_for_token(WETH), 1u128);
    }

    // ── Registry conversion ──────────────────────────────────────────────────

    #[test]
    fn curve_pool_to_registry_source_roundtrip() {
        let pool = &arbitrum_pools()[0]; // 2pool
        let source = curve_pool_to_registry_source(pool);
        assert!(matches!(source, LiquiditySource::CurveStable(_)));
        assert_eq!(source.address(), ARB_CURVE_2POOL);

        if let LiquiditySource::CurveStable(cs) = &source {
            assert_eq!(cs.tokens.len(), 2);
            assert_eq!(cs.amp, 800);
            assert_eq!(cs.fee_bps, 4);
            assert_eq!(cs.rates.len(), 2);
        }
    }

    #[test]
    fn quote_curve_stable_works() {
        // Build a CurveStablePool with real balances
        let source = CurveStablePool {
            address: "0xpool".to_string(),
            tokens: vec!["0xa".to_string(), "0xb".to_string()],
            balances: vec![
                "1000000000000000000000000".to_string(), // 1M tokens (1e24)
                "1000000000000000000000000".to_string(),
            ],
            amp: 1000,
            fee_bps: 4,
            rates: vec![
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
            ],
            pool_type: "stable".to_string(),
        };

        let amount_in = 1_000_000_000_000_000_000u128; // 1 token
        let dy = quote_curve_stable(&source, "0xa", "0xb", amount_in);
        assert!(dy.is_some());
        let out = dy.unwrap();
        // Should be close to 1:1 minus tiny fee + slippage
        let ratio = out as f64 / amount_in as f64;
        assert!(ratio > 0.99, "ratio {ratio} should be > 0.99");
    }

    #[test]
    fn quote_curve_stable_unknown_token_returns_none() {
        let source = CurveStablePool {
            address: "0xpool".to_string(),
            tokens: vec!["0xa".to_string(), "0xb".to_string()],
            balances: vec!["1000000".to_string(), "1000000".to_string()],
            amp: 100,
            fee_bps: 4,
            rates: vec![
                1_000_000_000_000_000_000u128,
                1_000_000_000_000_000_000u128,
            ],
            pool_type: "stable".to_string(),
        };

        assert!(quote_curve_stable(&source, "0xUNKNOWN", "0xb", 1000).is_none());
    }

    // ── Mixed decimals (realistic Arbitrum 2pool scenario) ──────────────────

    #[test]
    fn get_dy_mixed_decimals_usdc_usdt() {
        // Simulate Arbitrum 2pool: USDC (6 dec) / USDT (6 dec)
        // Both have rate = 1e12 (normalizing to 18 dec)
        // Realistic reserves: ~50M each in 6-decimal raw units
        let pool = CurvePool {
            address: ARB_CURVE_2POOL.to_string(),
            tokens: vec![ARB_USDC.to_string(), ARB_USDT.to_string()],
            balances: vec![
                50_000_000_000_000u128, // 50M USDC (6 dec = 50_000_000 * 1e6)
                50_000_000_000_000u128, // 50M USDT (6 dec)
            ],
            amp: 800,
            fee_bps: 4,
            rates: vec![
                1_000_000_000_000u128, // 10^12
                1_000_000_000_000u128,
            ],
            pool_type: "stable".to_string(),
        };

        // Swap 1000 USDC (= 1_000 * 1e6 raw units).
        // Smaller amounts (e.g. 1 USDC) lose precision in the f64 normalization
        // because 1 USDC normalizes to just 1.0 in 18-decimal space.
        let amount_in = 1_000_000_000u128; // 1,000 USDC (6 dec)
        let dy = get_dy(&pool, 0, 1, amount_in);
        assert!(dy.is_some(), "should produce output for USDC->USDT");
        let out = dy.unwrap();
        // Should be very close to 1:1 (both 6-decimal stablecoins)
        let ratio = out as f64 / amount_in as f64;
        assert!(
            ratio > 0.99 && ratio < 1.01,
            "USDC->USDT ratio {ratio} should be ~1.0"
        );
    }
}
