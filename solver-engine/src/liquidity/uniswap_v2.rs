use shared::rpc::EthClient;
use tracing::debug;

use crate::models::liquidity::{PoolKind, UniswapV2Pool};

// ── Factory addresses ─────────────────────────────────────────────────────────

pub const UNISWAP_V2_FACTORY: &str = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f";
pub const SUSHISWAP_FACTORY: &str = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac";

/// Fee for Uniswap V2 (0.3% = 30 bps)
const UNISWAP_V2_FEE_BPS: u32 = 30;
/// Fee for Sushiswap (0.3% = 30 bps)
const SUSHISWAP_FEE_BPS: u32 = 30;

// ── ABI selectors ─────────────────────────────────────────────────────────────

/// `getPair(address,address)` → 4-byte selector
const GET_PAIR_SELECTOR: &str = "e6a43905";

/// `getReserves()` → 4-byte selector
const GET_RESERVES_SELECTOR: &str = "0902f1ac";

/// `token0()` → 4-byte selector
const TOKEN0_SELECTOR: &str = "0dfe1681";

/// `token1()` → 4-byte selector
const TOKEN1_SELECTOR: &str = "d21220a7";

// ── ABI encoding helpers ──────────────────────────────────────────────────────

/// Encode a factory `getPair(token_a, token_b)` call.
fn encode_get_pair(token_a: &str, token_b: &str) -> String {
    let a = pad_address(token_a);
    let b = pad_address(token_b);
    format!("0x{GET_PAIR_SELECTOR}{a}{b}")
}

/// Pad an Ethereum address (strip 0x, left-pad to 32 bytes).
fn pad_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

/// Decode a 32-byte ABI-encoded address from a hex return value (0x-prefixed, 64 hex chars).
fn decode_address(hex: &str) -> Option<String> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 40 {
        return None;
    }
    // Address occupies the last 20 bytes (40 hex chars) of the 32-byte word
    let addr = &clean[clean.len() - 40..];
    let result = format!("0x{addr}");
    // Zero address → no pair
    if addr == "0000000000000000000000000000000000000000" {
        return None;
    }
    Some(result)
}

/// Decode `getReserves()` output: (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)
/// ABI encoding: three 32-byte words.
fn decode_reserves(hex: &str) -> Option<(u128, u128)> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 192 {
        // Need at least 3 × 64 hex chars
        return None;
    }
    let r0 = u128::from_str_radix(&clean[..64], 16).ok()?;
    let r1 = u128::from_str_radix(&clean[64..128], 16).ok()?;
    Some((r0, r1))
}

// ── Pool monitor ──────────────────────────────────────────────────────────────

/// Fetch a Uniswap V2 (or Sushiswap) pool for the given token pair from the factory.
///
/// Returns `None` if no pair exists.  Token ordering is normalised so that
/// `token0 < token1` (by address, case-insensitively).
pub async fn fetch_pool(
    rpc: &EthClient,
    factory: &str,
    kind: PoolKind,
    token_a: &str,
    token_b: &str,
) -> anyhow::Result<Option<UniswapV2Pool>> {
    // 1. Get pair address from factory
    let call_data = encode_get_pair(token_a, token_b);
    let pair_hex = rpc.call(factory, &call_data).await?;
    let pair_address = match decode_address(&pair_hex) {
        Some(addr) => addr,
        None => {
            debug!(
                factory = factory,
                token_a = token_a,
                token_b = token_b,
                "No V2 pair found"
            );
            return Ok(None);
        }
    };

    debug!(
        pair = %pair_address,
        token_a = token_a,
        token_b = token_b,
        "Found V2 pair"
    );

    // 2. Fetch token0 / token1 from the pair contract (canonical ordering)
    let token0_call = format!("0x{TOKEN0_SELECTOR}");
    let token1_call = format!("0x{TOKEN1_SELECTOR}");
    let (t0_hex, t1_hex) = tokio::try_join!(
        rpc.call(&pair_address, &token0_call),
        rpc.call(&pair_address, &token1_call),
    )?;

    let token0 = decode_address(&t0_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode token0 from {}", pair_address))?;
    let token1 = decode_address(&t1_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode token1 from {}", pair_address))?;

    // 3. Fetch reserves
    let reserves_hex = rpc
        .call(&pair_address, &format!("0x{GET_RESERVES_SELECTOR}"))
        .await?;
    let (reserve0, reserve1) = decode_reserves(&reserves_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode reserves from {}", pair_address))?;

    let fee_bps = match kind {
        PoolKind::Sushiswap => SUSHISWAP_FEE_BPS,
        _ => UNISWAP_V2_FEE_BPS,
    };

    Ok(Some(UniswapV2Pool {
        address: pair_address,
        kind,
        token0,
        token1,
        reserve0: reserve0.to_string(),
        reserve1: reserve1.to_string(),
        fee_bps,
    }))
}

/// Refresh reserves for an existing pool in-place.
pub async fn sync_reserves(rpc: &EthClient, pool: &mut UniswapV2Pool) -> anyhow::Result<()> {
    let reserves_hex = rpc
        .call(&pool.address, &format!("0x{GET_RESERVES_SELECTOR}"))
        .await?;
    let (r0, r1) = decode_reserves(&reserves_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode reserves from {}", pool.address))?;

    pool.reserve0 = r0.to_string();
    pool.reserve1 = r1.to_string();

    debug!(
        address = %pool.address,
        reserve0 = %pool.reserve0,
        reserve1 = %pool.reserve1,
        "Synced V2 reserves"
    );
    Ok(())
}

// ── Spot price ────────────────────────────────────────────────────────────────

/// Returns the spot price of token0 denominated in token1.
/// i.e. how many token1 wei you get per 1 token0 wei (ignoring fees, exact at infinitesimal size).
pub fn spot_price(pool: &UniswapV2Pool) -> Option<f64> {
    let r0 = pool.reserve0.parse::<f64>().ok()?;
    let r1 = pool.reserve1.parse::<f64>().ok()?;
    if r0 == 0.0 {
        return None;
    }
    Some(r1 / r0)
}

// ── Swap math ─────────────────────────────────────────────────────────────────

/// Compute the output amount for a V2 constant-product swap.
///
/// Formula: amount_out = (amount_in * (10000 - fee_bps) * reserve_out)
///                       / (reserve_in * 10000 + amount_in * (10000 - fee_bps))
///
/// Returns None on overflow or zero reserves.
pub fn get_amount_out(pool: &UniswapV2Pool, amount_in: u128, zero_for_one: bool) -> Option<u128> {
    let (reserve_in, reserve_out) = if zero_for_one {
        (
            pool.reserve0.parse::<u128>().ok()?,
            pool.reserve1.parse::<u128>().ok()?,
        )
    } else {
        (
            pool.reserve1.parse::<u128>().ok()?,
            pool.reserve0.parse::<u128>().ok()?,
        )
    };

    if reserve_in == 0 || reserve_out == 0 {
        return None;
    }

    let fee_multiplier = (10_000u128).checked_sub(pool.fee_bps as u128)?;
    let amount_in_with_fee = amount_in.checked_mul(fee_multiplier)?;
    let numerator = amount_in_with_fee.checked_mul(reserve_out)?;
    let denominator = reserve_in
        .checked_mul(10_000)?
        .checked_add(amount_in_with_fee)?;

    numerator.checked_div(denominator)
}

/// Compute the input needed to receive a specific output amount.
pub fn get_amount_in(pool: &UniswapV2Pool, amount_out: u128, zero_for_one: bool) -> Option<u128> {
    let (reserve_in, reserve_out) = if zero_for_one {
        (
            pool.reserve0.parse::<u128>().ok()?,
            pool.reserve1.parse::<u128>().ok()?,
        )
    } else {
        (
            pool.reserve1.parse::<u128>().ok()?,
            pool.reserve0.parse::<u128>().ok()?,
        )
    };

    if reserve_in == 0 || reserve_out == 0 || amount_out >= reserve_out {
        return None;
    }

    let fee_multiplier = (10_000u128).checked_sub(pool.fee_bps as u128)?;
    let numerator = reserve_in
        .checked_mul(amount_out)?
        .checked_mul(10_000)?;
    let denominator = reserve_out
        .checked_sub(amount_out)?
        .checked_mul(fee_multiplier)?;

    numerator.checked_div(denominator)?.checked_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::{PoolKind, UniswapV2Pool};

    fn make_pool(r0: u128, r1: u128) -> UniswapV2Pool {
        UniswapV2Pool {
            address: "0x0000".into(),
            kind: PoolKind::UniswapV2,
            token0: "0xtoken0".into(),
            token1: "0xtoken1".into(),
            reserve0: r0.to_string(),
            reserve1: r1.to_string(),
            fee_bps: 30,
        }
    }

    #[test]
    fn swap_round_trip() {
        let pool = make_pool(1_000_000_000, 2_000_000_000);
        let amount_in = 10_000u128;
        let out = get_amount_out(&pool, amount_in, true).unwrap();
        assert!(out > 0);
        // amount_in needed to get `out` should be ≥ original amount_in (fees)
        let in_back = get_amount_in(&pool, out, true).unwrap();
        assert!(in_back >= amount_in);
    }

    #[test]
    fn zero_reserve_returns_none() {
        let pool = make_pool(0, 1_000_000);
        assert!(get_amount_out(&pool, 1000, true).is_none());
    }

    #[test]
    fn spot_price_2x() {
        // r1 = 2× r0 → spot price = 2.0
        let pool = make_pool(1_000_000, 2_000_000);
        let price = spot_price(&pool).unwrap();
        assert!((price - 2.0).abs() < 1e-9);
    }

    #[test]
    fn spot_price_zero_reserve_none() {
        let pool = make_pool(0, 1_000_000);
        assert!(spot_price(&pool).is_none());
    }

    #[test]
    fn decode_address_ok() {
        // 32-byte ABI word for address 0xAbCd...1234
        let hex = "0x000000000000000000000000abcd000000000000000000000000000000001234";
        let addr = decode_address(hex).unwrap();
        assert_eq!(addr, "0xabcd000000000000000000000000000000001234");
    }

    #[test]
    fn decode_address_zero_returns_none() {
        let hex = "0x0000000000000000000000000000000000000000000000000000000000000000";
        assert!(decode_address(hex).is_none());
    }

    #[test]
    fn decode_reserves_ok() {
        // reserve0 = 1000, reserve1 = 2000, timestamp = ignored
        let r0_hex = format!("{:064x}", 1000u128);
        let r1_hex = format!("{:064x}", 2000u128);
        let ts_hex = format!("{:064x}", 12345u32);
        let hex = format!("0x{r0_hex}{r1_hex}{ts_hex}");
        let (r0, r1) = decode_reserves(&hex).unwrap();
        assert_eq!(r0, 1000);
        assert_eq!(r1, 2000);
    }

    #[test]
    fn encode_get_pair_length() {
        let data = encode_get_pair(
            "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        );
        // 0x + 8 (selector) + 64 (addr1) + 64 (addr2) = 137 chars
        assert_eq!(data.len(), 2 + 8 + 64 + 64);
    }
}
