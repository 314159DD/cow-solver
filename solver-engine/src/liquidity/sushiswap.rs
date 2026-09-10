//! SushiSwap liquidity source integration.
//!
//! SushiSwap is a Uniswap V2 fork with identical `x·y = k` math.
//! The only differences are:
//! - Different factory addresses per chain
//! - Different router addresses per chain
//! - Same 0.3% fee (30 bps)
//!
//! We reuse all swap math from `uniswap_v2` and just provide SushiSwap-specific
//! factory/router addresses and discovery helpers.

use shared::rpc::EthClient;
use tracing::debug;

use crate::liquidity::uniswap_v2;
use crate::models::liquidity::{PoolKind, UniswapV2Pool};

// ── Factory addresses per chain ─────────────────────────────────────────────

/// SushiSwap factory on Ethereum mainnet
pub const MAINNET_FACTORY: &str = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac";

/// SushiSwap factory on Arbitrum One
pub const ARBITRUM_FACTORY: &str = "0xc35DADB65012eC5796536bD9864eD8773aBc74C4";

// ── Router addresses per chain ──────────────────────────────────────────────

/// SushiSwap router on Ethereum mainnet
pub const MAINNET_ROUTER: &str = "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F";

/// SushiSwap router on Arbitrum One
pub const ARBITRUM_ROUTER: &str = "0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506";

/// SushiSwap fee: 0.3% = 30 bps (identical to Uniswap V2)
pub const FEE_BPS: u32 = 30;

// ── Key pairs to discover on Arbitrum ───────────────────────────────────────

/// High-volume SushiSwap pairs on Arbitrum.
pub const ARBITRUM_KEY_PAIRS: &[(&str, &str)] = &[
    // WETH/USDC
    (
        "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
        "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
    ),
    // WETH/USDT
    (
        "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
        "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
    ),
    // WETH/ARB
    (
        "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
        "0x912CE59144191C1204E64559FE8253a0e49E6548",
    ),
];

// ── Discovery ───────────────────────────────────────────────────────────────

/// Return the SushiSwap factory address for a given chain ID.
pub fn factory_for_chain(chain_id: u64) -> &'static str {
    match chain_id {
        42161 => ARBITRUM_FACTORY,
        _ => MAINNET_FACTORY,
    }
}

/// Return the SushiSwap router address for a given chain ID.
pub fn router_for_chain(chain_id: u64) -> &'static str {
    match chain_id {
        42161 => ARBITRUM_ROUTER,
        _ => MAINNET_ROUTER,
    }
}

/// Fetch a SushiSwap pool for the given token pair.
///
/// Delegates to the Uniswap V2 `fetch_pool` function with the SushiSwap factory
/// and `PoolKind::Sushiswap`.
pub async fn fetch_pool(
    rpc: &EthClient,
    factory: &str,
    token_a: &str,
    token_b: &str,
) -> anyhow::Result<Option<UniswapV2Pool>> {
    uniswap_v2::fetch_pool(rpc, factory, PoolKind::Sushiswap, token_a, token_b).await
}

/// Refresh reserves for an existing SushiSwap pool.
///
/// Identical to Uniswap V2 reserve sync since the pair contract ABI is the same.
pub async fn sync_reserves(rpc: &EthClient, pool: &mut UniswapV2Pool) -> anyhow::Result<()> {
    uniswap_v2::sync_reserves(rpc, pool).await
}

// ── Swap math (re-exported from uniswap_v2) ────────────────────────────────

/// Compute SushiSwap output amount. Same `x·y = k` formula as Uniswap V2.
pub fn get_amount_out(pool: &UniswapV2Pool, amount_in: u128, zero_for_one: bool) -> Option<u128> {
    uniswap_v2::get_amount_out(pool, amount_in, zero_for_one)
}

/// Compute SushiSwap input needed for a target output. Same formula as Uniswap V2.
pub fn get_amount_in(pool: &UniswapV2Pool, amount_out: u128, zero_for_one: bool) -> Option<u128> {
    uniswap_v2::get_amount_in(pool, amount_out, zero_for_one)
}

/// Spot price for a SushiSwap pool. Same formula as Uniswap V2.
pub fn spot_price(pool: &UniswapV2Pool) -> Option<f64> {
    uniswap_v2::spot_price(pool)
}

/// Discover SushiSwap pools for a list of token pairs.
///
/// Returns all successfully discovered pools.
pub async fn discover_pools(
    rpc: &EthClient,
    factory: &str,
    pairs: &[(&str, &str)],
) -> Vec<UniswapV2Pool> {
    let mut pools = Vec::new();
    for &(token_a, token_b) in pairs {
        match fetch_pool(rpc, factory, token_a, token_b).await {
            Ok(Some(pool)) => {
                debug!(
                    address = %pool.address,
                    token0 = %pool.token0,
                    token1 = %pool.token1,
                    "Discovered SushiSwap pool"
                );
                pools.push(pool);
            }
            Ok(None) => {}
            Err(e) => {
                debug!(error = %e, token_a, token_b, "SushiSwap pool discovery error");
            }
        }
    }
    pools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::{PoolKind, UniswapV2Pool};

    fn make_sushi_pool(r0: u128, r1: u128) -> UniswapV2Pool {
        UniswapV2Pool {
            address: "0xSushiPair".into(),
            kind: PoolKind::Sushiswap,
            token0: "0xtoken0".into(),
            token1: "0xtoken1".into(),
            reserve0: r0.to_string(),
            reserve1: r1.to_string(),
            fee_bps: FEE_BPS,
        }
    }

    #[test]
    fn sushi_swap_math_matches_uni_v2() {
        let sushi = make_sushi_pool(1_000_000_000, 2_000_000_000);
        let uni = UniswapV2Pool {
            kind: PoolKind::UniswapV2,
            ..sushi.clone()
        };

        let amount_in = 50_000u128;
        let sushi_out = get_amount_out(&sushi, amount_in, true).unwrap();
        let uni_out = uniswap_v2::get_amount_out(&uni, amount_in, true).unwrap();

        assert_eq!(sushi_out, uni_out, "SushiSwap and Uni V2 math must be identical");
    }

    #[test]
    fn sushi_get_amount_in_round_trip() {
        let pool = make_sushi_pool(1_000_000_000, 2_000_000_000);
        let amount_in = 10_000u128;
        let out = get_amount_out(&pool, amount_in, true).unwrap();
        let in_back = get_amount_in(&pool, out, true).unwrap();
        assert!(in_back >= amount_in);
    }

    #[test]
    fn factory_for_chain_arbitrum() {
        assert_eq!(factory_for_chain(42161), ARBITRUM_FACTORY);
    }

    #[test]
    fn factory_for_chain_mainnet() {
        assert_eq!(factory_for_chain(1), MAINNET_FACTORY);
    }

    #[test]
    fn spot_price_works() {
        let pool = make_sushi_pool(1_000_000, 3_000_000);
        let price = spot_price(&pool).unwrap();
        assert!((price - 3.0).abs() < 1e-9);
    }
}
