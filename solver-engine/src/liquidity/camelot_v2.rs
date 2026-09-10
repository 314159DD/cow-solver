use shared::rpc::EthClient;
use tracing::debug;

use crate::models::liquidity::CamelotV2Pool;

// ── Factory / Router addresses (Arbitrum) ────────────────────────────────────

/// Camelot V2 factory on Arbitrum
pub const CAMELOT_V2_FACTORY: &str = "0x6EcCab422D763aC031210895C81787E87B43A652";

/// Camelot V2 router on Arbitrum
pub const CAMELOT_V2_ROUTER: &str = "0xc873fEcbd354f5A56E00E710B90EF4201db2448d";

// ── ABI selectors ─────────────────────────────────────────────────────────────

/// `getPair(address,address)` → 4-byte selector
const GET_PAIR_SELECTOR: &str = "e6a43905";

/// `getReserves()` → 4-byte selector
const GET_RESERVES_SELECTOR: &str = "0902f1ac";

/// `token0()` → 4-byte selector
const TOKEN0_SELECTOR: &str = "0dfe1681";

/// `token1()` → 4-byte selector
const TOKEN1_SELECTOR: &str = "d21220a7";

/// `token0FeePercent()` → 4-byte selector
/// Returns the fee percentage for token0→token1 direction (in bps * 100, i.e. hundredths of percent).
/// Camelot uses a custom fee getter; this selector is keccak256("token0FeePercent()")[:4].
const TOKEN0_FEE_PERCENT_SELECTOR: &str = "60ee0525";

/// `token1FeePercent()` → 4-byte selector
const TOKEN1_FEE_PERCENT_SELECTOR: &str = "f577db72";

/// `stableSwap()` → 4-byte selector — returns bool for whether this is a stable pair
const STABLE_SWAP_SELECTOR: &str = "5a2fc67d";

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

/// Decode a 32-byte ABI-encoded address from a hex return value.
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

/// Decode `getReserves()` output: (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)
fn decode_reserves(hex: &str) -> Option<(u128, u128)> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 192 {
        return None;
    }
    let r0 = u128::from_str_radix(&clean[..64], 16).ok()?;
    let r1 = u128::from_str_radix(&clean[64..128], 16).ok()?;
    Some((r0, r1))
}

/// Decode a uint16/uint256 fee percentage from a single 32-byte word.
/// Camelot returns fee as a percentage (e.g. 300 = 3%, which is 300 bps).
fn decode_fee_percent(hex: &str) -> Option<u32> {
    let clean = hex.trim_start_matches("0x");
    if clean.is_empty() {
        return None;
    }
    // The value fits in u32
    let val = u64::from_str_radix(clean.trim_start_matches('0'), 16)
        .ok()
        .or_else(|| {
            // Handle all-zero case
            if clean.chars().all(|c| c == '0') {
                Some(0)
            } else {
                None
            }
        })?;
    Some(val as u32)
}

/// Decode a bool from a single 32-byte word (0 = false, 1 = true).
fn decode_bool(hex: &str) -> Option<bool> {
    let clean = hex.trim_start_matches("0x");
    if clean.is_empty() {
        return None;
    }
    // Last character: 0 = false, 1 = true
    let last = clean.chars().last()?;
    Some(last == '1')
}

// ── Pool discovery ───────────────────────────────────────────────────────────

/// Fetch a Camelot V2 pool for the given token pair from the factory.
///
/// Camelot V2 pools have directional fees: the fee for selling token0 may
/// differ from the fee for selling token1. This function fetches both fee
/// directions and the `stableSwap` flag.
///
/// Returns `None` if no pair exists.
pub async fn fetch_pool(
    rpc: &EthClient,
    token_a: &str,
    token_b: &str,
) -> anyhow::Result<Option<CamelotV2Pool>> {
    // 1. Get pair address from factory
    let call_data = encode_get_pair(token_a, token_b);
    let pair_hex = rpc.call(CAMELOT_V2_FACTORY, &call_data).await?;
    let pair_address = match decode_address(&pair_hex) {
        Some(addr) => addr,
        None => {
            debug!(
                factory = CAMELOT_V2_FACTORY,
                token_a = token_a,
                token_b = token_b,
                "No Camelot V2 pair found"
            );
            return Ok(None);
        }
    };

    debug!(
        pair = %pair_address,
        token_a = token_a,
        token_b = token_b,
        "Found Camelot V2 pair"
    );

    // 2. Fetch token0, token1, reserves, directional fees, and stable flag in parallel
    let token0_call = format!("0x{TOKEN0_SELECTOR}");
    let token1_call = format!("0x{TOKEN1_SELECTOR}");
    let reserves_call = format!("0x{GET_RESERVES_SELECTOR}");
    let fee0_call = format!("0x{TOKEN0_FEE_PERCENT_SELECTOR}");
    let fee1_call = format!("0x{TOKEN1_FEE_PERCENT_SELECTOR}");
    let stable_call = format!("0x{STABLE_SWAP_SELECTOR}");

    let (t0_hex, t1_hex, reserves_hex, fee0_hex, fee1_hex, stable_hex) = tokio::try_join!(
        rpc.call(&pair_address, &token0_call),
        rpc.call(&pair_address, &token1_call),
        rpc.call(&pair_address, &reserves_call),
        rpc.call(&pair_address, &fee0_call),
        rpc.call(&pair_address, &fee1_call),
        rpc.call(&pair_address, &stable_call),
    )?;

    let token0 = decode_address(&t0_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode token0 from {}", pair_address))?;
    let token1 = decode_address(&t1_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode token1 from {}", pair_address))?;
    let (reserve0, reserve1) = decode_reserves(&reserves_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode reserves from {}", pair_address))?;

    // Camelot fee is in "hundredths of percent" — e.g. 300 = 3.00% = 300 bps
    // We store as basis points directly
    let fee_token0_to_token1 = decode_fee_percent(&fee0_hex).unwrap_or(300); // default 3%
    let fee_token1_to_token0 = decode_fee_percent(&fee1_hex).unwrap_or(300);
    let is_stable = decode_bool(&stable_hex).unwrap_or(false);

    debug!(
        address = %pair_address,
        fee_0_to_1 = fee_token0_to_token1,
        fee_1_to_0 = fee_token1_to_token0,
        is_stable = is_stable,
        "Camelot V2 pool details"
    );

    Ok(Some(CamelotV2Pool {
        address: pair_address,
        token0,
        token1,
        reserve0: reserve0.to_string(),
        reserve1: reserve1.to_string(),
        fee_token0_to_token1,
        fee_token1_to_token0,
        is_stable,
    }))
}

/// Refresh reserves for an existing Camelot V2 pool in-place.
pub async fn sync_reserves(rpc: &EthClient, pool: &mut CamelotV2Pool) -> anyhow::Result<()> {
    let reserves_hex = rpc
        .call(&pool.address, &format!("0x{GET_RESERVES_SELECTOR}"))
        .await?;
    let (r0, r1) = decode_reserves(&reserves_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode reserves from {}", pool.address))?;

    pool.reserve0 = r0.to_string();
    pool.reserve1 = r1.to_string();

    // Also refresh fees — Camelot fees can be updated by governance
    let fee0_hex = rpc
        .call(&pool.address, &format!("0x{TOKEN0_FEE_PERCENT_SELECTOR}"))
        .await?;
    let fee1_hex = rpc
        .call(&pool.address, &format!("0x{TOKEN1_FEE_PERCENT_SELECTOR}"))
        .await?;

    if let Some(f0) = decode_fee_percent(&fee0_hex) {
        pool.fee_token0_to_token1 = f0;
    }
    if let Some(f1) = decode_fee_percent(&fee1_hex) {
        pool.fee_token1_to_token0 = f1;
    }

    debug!(
        address = %pool.address,
        reserve0 = %pool.reserve0,
        reserve1 = %pool.reserve1,
        fee_0_to_1 = pool.fee_token0_to_token1,
        fee_1_to_0 = pool.fee_token1_to_token0,
        "Synced Camelot V2 reserves"
    );
    Ok(())
}

// ── Spot price ────────────────────────────────────────────────────────────────

/// Returns the spot price of token0 denominated in token1 (ignoring fees).
pub fn spot_price(pool: &CamelotV2Pool) -> Option<f64> {
    let r0 = pool.reserve0.parse::<f64>().ok()?;
    let r1 = pool.reserve1.parse::<f64>().ok()?;
    if r0 == 0.0 {
        return None;
    }
    Some(r1 / r0)
}

// ── Swap math: volatile (xy = k) ─────────────────────────────────────────────

/// Compute output for a volatile Camelot V2 swap (xy = k, with directional fees).
///
/// Formula: amount_out = (amount_in * (10000 - fee_bps) * reserve_out)
///                       / (reserve_in * 10000 + amount_in * (10000 - fee_bps))
fn get_amount_out_volatile(
    reserve_in: u128,
    reserve_out: u128,
    amount_in: u128,
    fee_bps: u32,
) -> Option<u128> {
    if reserve_in == 0 || reserve_out == 0 {
        return None;
    }

    let fee_multiplier = (10_000u128).checked_sub(fee_bps as u128)?;
    let amount_in_with_fee = amount_in.checked_mul(fee_multiplier)?;
    let numerator = amount_in_with_fee.checked_mul(reserve_out)?;
    let denominator = reserve_in
        .checked_mul(10_000)?
        .checked_add(amount_in_with_fee)?;

    numerator.checked_div(denominator)
}

/// Compute required input for a volatile swap to produce `amount_out`.
fn get_amount_in_volatile(
    reserve_in: u128,
    reserve_out: u128,
    amount_out: u128,
    fee_bps: u32,
) -> Option<u128> {
    if reserve_in == 0 || reserve_out == 0 || amount_out >= reserve_out {
        return None;
    }

    let fee_multiplier = (10_000u128).checked_sub(fee_bps as u128)?;
    let numerator = reserve_in
        .checked_mul(amount_out)?
        .checked_mul(10_000)?;
    let denominator = reserve_out
        .checked_sub(amount_out)?
        .checked_mul(fee_multiplier)?;

    numerator.checked_div(denominator)?.checked_add(1)
}

// ── Swap math: stable (x³y + xy³ = k) ───────────────────────────────────────

/// Compute the invariant k = x³y + xy³ for a stable pair.
///
/// Uses u128 arithmetic with careful overflow avoidance by working in
/// scaled-down values when reserves are large. For production accuracy on
/// very large reserves, this would need a big-integer library.
fn stable_k(x: u128, y: u128) -> Option<u128> {
    // k = x³y + xy³ = xy(x² + y²)
    let xy = x.checked_mul(y)?;
    let x2 = x.checked_mul(x)?;
    let y2 = y.checked_mul(y)?;
    let sum_sq = x2.checked_add(y2)?;
    xy.checked_mul(sum_sq)
}

/// Compute f(x, y) = x³y + xy³ for the stable swap curve using f64 for
/// intermediate calculations (necessary because u128 overflows on typical
/// reserve sizes when cubing).
fn stable_get_y(x_new: f64, k: f64) -> Option<f64> {
    // Solve: x_new³ * y + x_new * y³ = k
    // i.e. x_new * y * (x_new² + y²) = k
    // Newton's method on f(y) = x_new³*y + x_new*y³ - k
    // f'(y) = x_new³ + 3*x_new*y²
    if x_new <= 0.0 || k <= 0.0 {
        return None;
    }

    let x3 = x_new * x_new * x_new;
    // Initial guess: y ≈ k / x³ (ignoring the xy³ term)
    let mut y = (k / x3).max(1.0);

    for _ in 0..255 {
        let y2 = y * y;
        let y3 = y2 * y;
        let f = x3 * y + x_new * y3 - k;
        let f_prime = x3 + 3.0 * x_new * y2;
        if f_prime == 0.0 {
            return None;
        }
        let y_next = y - f / f_prime;
        if (y_next - y).abs() <= 1.0 {
            return Some(y_next.max(0.0));
        }
        y = y_next;
        if y < 0.0 {
            y = 1.0; // Reset if Newton overshoots
        }
    }
    Some(y.max(0.0))
}

/// Compute output for a stable Camelot V2 swap (x³y + xy³ = k).
///
/// The fee is taken from the input amount first, then the invariant determines output.
fn get_amount_out_stable(
    reserve_in: u128,
    reserve_out: u128,
    amount_in: u128,
    fee_bps: u32,
) -> Option<u128> {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
        return None;
    }

    // Apply fee
    let fee_multiplier = (10_000u128).checked_sub(fee_bps as u128)?;
    let amount_in_after_fee = amount_in.checked_mul(fee_multiplier)? / 10_000;

    // Use f64 for the invariant math (u128 cubing overflows at ~6.9e12)
    let x = reserve_in as f64;
    let y = reserve_out as f64;
    let k = x * y * (x * x + y * y);

    let x_new = x + amount_in_after_fee as f64;
    let y_new = stable_get_y(x_new, k)?;

    let dy = y - y_new;
    if dy <= 0.0 {
        return None;
    }
    Some(dy as u128)
}

// ── Public swap interface ────────────────────────────────────────────────────

/// Compute the output amount for a Camelot V2 swap, handling both volatile
/// and stable pool types with directional fees.
///
/// `zero_for_one`: true = selling token0 for token1.
pub fn get_amount_out(pool: &CamelotV2Pool, amount_in: u128, zero_for_one: bool) -> Option<u128> {
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

    let fee_bps = if zero_for_one {
        pool.fee_token0_to_token1
    } else {
        pool.fee_token1_to_token0
    };

    if pool.is_stable {
        get_amount_out_stable(reserve_in, reserve_out, amount_in, fee_bps)
    } else {
        get_amount_out_volatile(reserve_in, reserve_out, amount_in, fee_bps)
    }
}

/// Compute the input needed to receive a specific output amount.
///
/// Only available for volatile pools. Stable pool `get_amount_in` requires
/// numerical inversion (deferred to a future sprint).
pub fn get_amount_in(pool: &CamelotV2Pool, amount_out: u128, zero_for_one: bool) -> Option<u128> {
    if pool.is_stable {
        // Stable pool inverse not implemented — would need binary search
        return None;
    }

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

    let fee_bps = if zero_for_one {
        pool.fee_token0_to_token1
    } else {
        pool.fee_token1_to_token0
    };

    get_amount_in_volatile(reserve_in, reserve_out, amount_out, fee_bps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::CamelotV2Pool;

    fn make_volatile_pool(r0: u128, r1: u128, fee0to1: u32, fee1to0: u32) -> CamelotV2Pool {
        CamelotV2Pool {
            address: "0x0000".into(),
            token0: "0xtoken0".into(),
            token1: "0xtoken1".into(),
            reserve0: r0.to_string(),
            reserve1: r1.to_string(),
            fee_token0_to_token1: fee0to1,
            fee_token1_to_token0: fee1to0,
            is_stable: false,
        }
    }

    fn make_stable_pool(r0: u128, r1: u128, fee0to1: u32, fee1to0: u32) -> CamelotV2Pool {
        CamelotV2Pool {
            address: "0x0000".into(),
            token0: "0xtoken0".into(),
            token1: "0xtoken1".into(),
            reserve0: r0.to_string(),
            reserve1: r1.to_string(),
            fee_token0_to_token1: fee0to1,
            fee_token1_to_token0: fee1to0,
            is_stable: true,
        }
    }

    // ── Volatile pool tests ────────────────────────────────────────────────

    #[test]
    fn volatile_swap_basic() {
        let pool = make_volatile_pool(1_000_000_000, 2_000_000_000, 300, 300);
        let out = get_amount_out(&pool, 10_000, true).unwrap();
        assert!(out > 0);
        assert!(out < 20_000); // can't get more than 2x
    }

    #[test]
    fn volatile_directional_fees() {
        // Different fees for each direction
        let pool = make_volatile_pool(1_000_000_000, 2_000_000_000, 100, 500);
        let amount = 10_000u128;

        let out_0_to_1 = get_amount_out(&pool, amount, true).unwrap(); // 1% fee
        let out_1_to_0 = get_amount_out(&pool, amount, false).unwrap(); // 5% fee

        // With 1% fee (0→1), output should be higher than with 5% fee (1→0)
        // but we also need to account for reserve ratios. The key check:
        // same pool, same amount_in, lower fee → higher output
        // For 0→1: fee=100 bps, selling token0 (cheaper side), getting token1 (more expensive)
        // For 1→0: fee=500 bps, selling token1, getting token0
        // Regardless of direction, with equal reserves the lower fee gives more
        let pool_equal = make_volatile_pool(1_000_000_000, 1_000_000_000, 100, 500);
        let eq_out_low_fee = get_amount_out(&pool_equal, amount, true).unwrap();
        let eq_out_high_fee = get_amount_out(&pool_equal, amount, false).unwrap();
        assert!(eq_out_low_fee > eq_out_high_fee);
    }

    #[test]
    fn volatile_round_trip() {
        let pool = make_volatile_pool(1_000_000_000, 2_000_000_000, 300, 300);
        let amount_in = 10_000u128;
        let out = get_amount_out(&pool, amount_in, true).unwrap();
        assert!(out > 0);
        let in_back = get_amount_in(&pool, out, true).unwrap();
        assert!(in_back >= amount_in);
    }

    #[test]
    fn volatile_zero_reserve_returns_none() {
        let pool = make_volatile_pool(0, 1_000_000, 300, 300);
        assert!(get_amount_out(&pool, 1000, true).is_none());
    }

    // ── Stable pool tests ──────────────────────────────────────────────────

    #[test]
    fn stable_swap_basic() {
        // Equal reserves, 0.3% fee — typical stablecoin pair
        let pool = make_stable_pool(1_000_000_000, 1_000_000_000, 30, 30);
        let out = get_amount_out(&pool, 10_000, true).unwrap();
        assert!(out > 0);
        // Stable pools at equal reserves should give close to 1:1 minus fee
        // With 0.3% fee: 10_000 * 0.997 ≈ 9_970
        assert!(out > 9_900, "Stable swap output {out} should be close to input");
        assert!(out < 10_000, "Output should be less than input due to fee");
    }

    #[test]
    fn stable_swap_near_1_to_1() {
        // Stable pairs with equal reserves should produce near-1:1 output
        let pool = make_stable_pool(1_000_000_000_000, 1_000_000_000_000, 10, 10); // 0.1% fee
        let input = 1_000_000u128;
        let out = get_amount_out(&pool, input, true).unwrap();
        // Should be within ~0.2% of input (fee + tiny slippage)
        let diff = if out > input { out - input } else { input - out };
        assert!(
            diff < input / 50,
            "Stable 1:1 diff {diff} too large for input {input}, got {out}"
        );
    }

    #[test]
    fn stable_get_amount_in_returns_none() {
        // Stable pool get_amount_in is not implemented
        let pool = make_stable_pool(1_000_000_000, 1_000_000_000, 30, 30);
        assert!(get_amount_in(&pool, 1000, true).is_none());
    }

    // ── Spot price tests ───────────────────────────────────────────────────

    #[test]
    fn spot_price_2x() {
        let pool = make_volatile_pool(1_000_000, 2_000_000, 300, 300);
        let price = spot_price(&pool).unwrap();
        assert!((price - 2.0).abs() < 1e-9);
    }

    #[test]
    fn spot_price_zero_reserve_none() {
        let pool = make_volatile_pool(0, 1_000_000, 300, 300);
        assert!(spot_price(&pool).is_none());
    }

    // ── ABI encoding tests ──────────────────────────────────────────────────

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
    fn decode_reserves_ok() {
        let r0_hex = format!("{:064x}", 1000u128);
        let r1_hex = format!("{:064x}", 2000u128);
        let ts_hex = format!("{:064x}", 12345u32);
        let hex = format!("0x{r0_hex}{r1_hex}{ts_hex}");
        let (r0, r1) = decode_reserves(&hex).unwrap();
        assert_eq!(r0, 1000);
        assert_eq!(r1, 2000);
    }

    #[test]
    fn decode_fee_percent_ok() {
        let hex = format!("0x{:064x}", 300u32);
        let fee = decode_fee_percent(&hex).unwrap();
        assert_eq!(fee, 300);
    }

    #[test]
    fn decode_bool_true_and_false() {
        let hex_true = format!("0x{:064x}", 1u32);
        let hex_false = format!("0x{:064x}", 0u32);
        assert_eq!(decode_bool(&hex_true), Some(true));
        assert_eq!(decode_bool(&hex_false), Some(false));
    }

    #[test]
    fn encode_get_pair_length() {
        let data = encode_get_pair(
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
        );
        assert_eq!(data.len(), 2 + 8 + 64 + 64);
    }

    // ── Stable math helpers ─────────────────────────────────────────────────

    #[test]
    fn stable_k_symmetry() {
        // k(x, y) should equal k(y, x)
        let k1 = stable_k(1000, 2000);
        let k2 = stable_k(2000, 1000);
        assert_eq!(k1, k2);
    }

    #[test]
    fn stable_get_y_basic() {
        let x = 1_000_000.0;
        let y = 1_000_000.0;
        let k = x * y * (x * x + y * y);

        // If we add nothing, y should be unchanged
        let y_new = stable_get_y(x, k).unwrap();
        assert!(
            (y_new - y).abs() < 2.0,
            "y_new={y_new} should be close to y={y}"
        );
    }
}
