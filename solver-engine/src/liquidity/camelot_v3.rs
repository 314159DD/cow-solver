use shared::rpc::EthClient;
use tracing::debug;

use crate::models::liquidity::CamelotV3Pool;

// ── Factory address (Arbitrum) ───────────────────────────────────────────────

/// Camelot V3 (Algebra) factory on Arbitrum
pub const CAMELOT_V3_FACTORY: &str = "0x1a3c9B1d2F0529D97f2afC5136Cc23e58f1FD35B";

// ── ABI selectors ─────────────────────────────────────────────────────────────

/// `poolByPair(address,address)` → 4-byte selector
/// Algebra uses `poolByPair` instead of Uniswap V3's `getPool` (no fee tier param).
const POOL_BY_PAIR_SELECTOR: &str = "d9a641e1";

/// `globalState()` → 4-byte selector
/// Returns (uint160 price, int24 tick, uint16 fee, ...) — replaces Uniswap V3's slot0.
const GLOBAL_STATE_SELECTOR: &str = "e76c01e4";

/// `liquidity()` → 4-byte selector
const LIQUIDITY_SELECTOR: &str = "1a686502";

/// `token0()` → 4-byte selector
const TOKEN0_SELECTOR: &str = "0dfe1681";

/// `token1()` → 4-byte selector
const TOKEN1_SELECTOR: &str = "d21220a7";

// ── ABI encoding helpers ──────────────────────────────────────────────────────

/// Pad an Ethereum address (strip 0x, left-pad to 32 bytes).
fn pad_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

/// Encode a factory `poolByPair(token_a, token_b)` call.
fn encode_pool_by_pair(token_a: &str, token_b: &str) -> String {
    let a = pad_address(token_a);
    let b = pad_address(token_b);
    format!("0x{POOL_BY_PAIR_SELECTOR}{a}{b}")
}

/// Decode a 32-byte ABI-encoded address. Returns None for zero address.
fn decode_address(hex: &str) -> Option<String> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 40 {
        return None;
    }
    let addr = &clean[clean.len() - 40..];
    if addr == "0000000000000000000000000000000000000000" {
        return None;
    }
    Some(format!("0x{addr}"))
}

/// Decode `globalState()` output from the Algebra pool.
///
/// Returns `(sqrtPriceX96: u128, tick: i32, fee: u32)`.
///
/// Algebra's `globalState()` returns:
///   Word 0: uint160 price (sqrtPriceX96)
///   Word 1: int24 tick
///   Word 2: uint16 fee (in hundredths of a bip, same as Uniswap V3 convention)
///   Words 3-6: additional fields (timepointIndex, communityFeeToken0/1, unlocked)
fn decode_global_state(hex: &str) -> Option<(u128, i32, u32)> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 192 {
        // Need at least 3 × 64 hex chars
        return None;
    }
    // Word 0: sqrtPriceX96 (uint160)
    let sqrt_price = u128::from_str_radix(&clean[..64], 16).ok()?;
    // Word 1: tick (int24, sign-extended)
    let tick_raw = u32::from_str_radix(&clean[64..128], 16).ok()?;
    let tick = if tick_raw & 0x800000 != 0 {
        (tick_raw | 0xFF000000u32) as i32
    } else {
        (tick_raw & 0x7FFFFF) as i32
    };
    // Word 2: fee (uint16) — in hundredths of a bip
    let fee = u32::from_str_radix(&clean[128..192], 16).ok()?;

    Some((sqrt_price, tick, fee))
}

/// Decode `liquidity()` output — a single uint128.
fn decode_liquidity(hex: &str) -> Option<u128> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 32 {
        return None;
    }
    u128::from_str_radix(&clean[clean.len().saturating_sub(32)..], 16).ok()
}

// ── Pool discovery ───────────────────────────────────────────────────────────

/// Fetch a Camelot V3 (Algebra) pool for the given token pair.
///
/// Unlike Uniswap V3, there is only one pool per pair (no fee tiers).
/// The fee is dynamic and fetched from `globalState()`.
///
/// Returns `None` if no pool exists.
pub async fn fetch_pool(
    rpc: &EthClient,
    token_a: &str,
    token_b: &str,
) -> anyhow::Result<Option<CamelotV3Pool>> {
    // 1. Look up pool address from factory
    let call_data = encode_pool_by_pair(token_a, token_b);
    let pool_hex = rpc.call(CAMELOT_V3_FACTORY, &call_data).await?;
    let pool_address = match decode_address(&pool_hex) {
        Some(addr) => addr,
        None => {
            debug!(
                token_a = token_a,
                token_b = token_b,
                "No Camelot V3 pool found"
            );
            return Ok(None);
        }
    };

    debug!(
        pool = %pool_address,
        token_a = token_a,
        token_b = token_b,
        "Found Camelot V3 pool"
    );

    // 2. Fetch token0, token1, globalState, liquidity in parallel
    let token0_call = format!("0x{TOKEN0_SELECTOR}");
    let token1_call = format!("0x{TOKEN1_SELECTOR}");
    let state_call = format!("0x{GLOBAL_STATE_SELECTOR}");
    let liq_call = format!("0x{LIQUIDITY_SELECTOR}");

    let (t0_hex, t1_hex, state_hex, liq_hex) = tokio::try_join!(
        rpc.call(&pool_address, &token0_call),
        rpc.call(&pool_address, &token1_call),
        rpc.call(&pool_address, &state_call),
        rpc.call(&pool_address, &liq_call),
    )?;

    let token0 = decode_address(&t0_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode token0 from {}", pool_address))?;
    let token1 = decode_address(&t1_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode token1 from {}", pool_address))?;
    let (sqrt_price_x96, tick, fee) = decode_global_state(&state_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode globalState from {}", pool_address))?;
    let liquidity = decode_liquidity(&liq_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode liquidity from {}", pool_address))?;

    debug!(
        address = %pool_address,
        sqrt_price_x96 = sqrt_price_x96,
        tick = tick,
        fee = fee,
        liquidity = liquidity,
        "Camelot V3 pool details"
    );

    Ok(Some(CamelotV3Pool {
        address: pool_address,
        token0,
        token1,
        sqrt_price_x96: sqrt_price_x96.to_string(),
        tick,
        liquidity: liquidity.to_string(),
        fee,
    }))
}

/// Refresh globalState and liquidity for an existing Camelot V3 pool in-place.
///
/// The dynamic fee is also refreshed since it changes based on volatility.
pub async fn sync_pool(rpc: &EthClient, pool: &mut CamelotV3Pool) -> anyhow::Result<()> {
    let state_call = format!("0x{GLOBAL_STATE_SELECTOR}");
    let liq_call = format!("0x{LIQUIDITY_SELECTOR}");

    let (state_hex, liq_hex) = tokio::try_join!(
        rpc.call(&pool.address, &state_call),
        rpc.call(&pool.address, &liq_call),
    )?;

    let (sqrt_price_x96, tick, fee) = decode_global_state(&state_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode globalState from {}", pool.address))?;
    let liquidity = decode_liquidity(&liq_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode liquidity from {}", pool.address))?;

    pool.sqrt_price_x96 = sqrt_price_x96.to_string();
    pool.tick = tick;
    pool.fee = fee;
    pool.liquidity = liquidity.to_string();

    debug!(
        address = %pool.address,
        sqrt_price_x96 = %pool.sqrt_price_x96,
        tick = pool.tick,
        fee = pool.fee,
        liquidity = %pool.liquidity,
        "Synced Camelot V3 pool"
    );
    Ok(())
}

// ── Spot price ────────────────────────────────────────────────────────────────

/// Returns the spot price of token0 denominated in token1 using sqrtPriceX96.
/// price = (sqrtPriceX96 / 2^96)^2
pub fn spot_price(pool: &CamelotV3Pool) -> Option<f64> {
    let sqrt_price = pool.sqrt_price_x96.parse::<f64>().ok()?;
    if sqrt_price == 0.0 {
        return None;
    }
    let q96 = (1u128 << 96) as f64;
    let price = (sqrt_price / q96).powi(2);
    Some(price)
}

// ── Approximate swap math ────────────────────────────────────────────────────

/// Compute approximate output for a Camelot V3 (Algebra) pool.
///
/// This is a simplified approximation using the current price (same approach
/// as the Uniswap V3 approximation). Full tick traversal is deferred.
///
/// The key difference from Uniswap V3 is that the fee is dynamic (fetched
/// from `globalState()`) rather than a fixed tier.
pub fn get_amount_out_approx(
    sqrt_price_x96: u128,
    liquidity: u128,
    amount_in: u128,
    zero_for_one: bool,
    fee: u32,
) -> Option<u128> {
    if sqrt_price_x96 == 0 || liquidity == 0 || amount_in == 0 {
        return None;
    }

    let fee_factor = 1_000_000u128.checked_sub(fee as u128)?;
    let sqrt_scaled = sqrt_price_x96 >> 64;
    if sqrt_scaled == 0 {
        return None;
    }
    let price_x64 = sqrt_scaled.checked_mul(sqrt_scaled)?;
    let q64 = 1u128 << 64;

    if zero_for_one {
        // token0 → token1: multiply by price
        let out = amount_in
            .checked_mul(price_x64)?
            .checked_div(q64)?
            .checked_mul(fee_factor)?
            .checked_div(1_000_000)?;
        Some(out)
    } else {
        // token1 → token0: divide by price
        let out = amount_in
            .checked_mul(q64)?
            .checked_div(price_x64)?
            .checked_mul(fee_factor)?
            .checked_div(1_000_000)?;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::CamelotV3Pool;

    fn make_pool(sqrt_price_x96: u128, liquidity: u128, fee: u32) -> CamelotV3Pool {
        CamelotV3Pool {
            address: "0x0000".into(),
            token0: "0xtoken0".into(),
            token1: "0xtoken1".into(),
            sqrt_price_x96: sqrt_price_x96.to_string(),
            tick: 0,
            liquidity: liquidity.to_string(),
            fee,
        }
    }

    #[test]
    fn spot_price_one_to_one() {
        // sqrtPriceX96 = 2^96 → price = 1.0
        let q96 = 1u128 << 96;
        let pool = make_pool(q96, 1_000_000, 3_000);
        let price = spot_price(&pool).unwrap();
        assert!((price - 1.0).abs() < 1e-6);
    }

    #[test]
    fn spot_price_zero_returns_none() {
        let pool = make_pool(0, 1_000_000, 3_000);
        assert!(spot_price(&pool).is_none());
    }

    #[test]
    fn get_amount_out_approx_basic() {
        let q96 = 1u128 << 96;
        let out = get_amount_out_approx(q96, 1_000_000, 1_000, true, 3_000);
        assert!(out.is_some());
        assert!(out.unwrap() < 1_000); // fee reduces output
    }

    #[test]
    fn get_amount_out_approx_dynamic_fee() {
        // Higher fee → less output
        let q96 = 1u128 << 96;
        let out_low = get_amount_out_approx(q96, 1_000_000, 10_000, true, 500).unwrap();
        let out_high = get_amount_out_approx(q96, 1_000_000, 10_000, true, 10_000).unwrap();
        assert!(out_low > out_high, "Higher fee should give less output");
    }

    #[test]
    fn get_amount_out_approx_zero_inputs_none() {
        assert!(get_amount_out_approx(0, 1_000_000, 1_000, true, 3_000).is_none());
        assert!(get_amount_out_approx(1u128 << 96, 0, 1_000, true, 3_000).is_none());
        assert!(get_amount_out_approx(1u128 << 96, 1_000_000, 0, true, 3_000).is_none());
    }

    // ── ABI decoding tests ──────────────────────────────────────────────────

    #[test]
    fn decode_address_ok() {
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
    fn decode_global_state_basic() {
        let sqrt_price_hex = format!("{:064x}", 1u128 << 96);
        let tick_hex = format!("{:064x}", 1000u32);
        let fee_hex = format!("{:064x}", 3000u32);
        let hex = format!("0x{sqrt_price_hex}{tick_hex}{fee_hex}");
        let (sqrt_price, tick, fee) = decode_global_state(&hex).unwrap();
        assert_eq!(sqrt_price, 1u128 << 96);
        assert_eq!(tick, 1000);
        assert_eq!(fee, 3000);
    }

    #[test]
    fn decode_global_state_negative_tick() {
        let sqrt_price_hex = format!("{:064x}", 1u128 << 96);
        let tick_hex = format!("{:064x}", 0xFFFFFFu32); // -1 as int24
        let fee_hex = format!("{:064x}", 500u32);
        let hex = format!("0x{sqrt_price_hex}{tick_hex}{fee_hex}");
        let (_, tick, fee) = decode_global_state(&hex).unwrap();
        assert_eq!(tick, -1);
        assert_eq!(fee, 500);
    }

    #[test]
    fn decode_liquidity_ok() {
        let hex = format!("0x{:064x}", 999_999_999u128);
        let liq = decode_liquidity(&hex).unwrap();
        assert_eq!(liq, 999_999_999);
    }

    #[test]
    fn encode_pool_by_pair_length() {
        let data = encode_pool_by_pair(
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
        );
        // 0x + 8 (selector) + 64 (addr1) + 64 (addr2) = 138 chars
        assert_eq!(data.len(), 2 + 8 + 64 + 64);
    }
}
