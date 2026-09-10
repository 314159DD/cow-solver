//! Trader Joe V2.1 (Liquidity Book) pool discovery, sync, and swap math.
//!
//! Trader Joe V2.1 uses a "Liquidity Book" model with discrete price bins
//! instead of continuous ticks. Each bin has a fixed price; liquidity is
//! concentrated in specific bins.
//!
//! Key concepts:
//! - **Bin step**: price increment between adjacent bins (in basis points)
//! - **Active bin**: the bin containing the current market price
//! - **Swap traversal**: consume liquidity in the active bin, then move to the next
//! - **Fees**: base fee + variable fee (based on volatility accumulator)
//!
//! Factory: 0x8e42f2F4101563bF679975178e880FD87d3eFd4e (Arbitrum)
//! Router: 0xb4315e873dBcf96Ffd0acd8EA43f689D8c20fB30 (Arbitrum)

use shared::rpc::EthClient;
use tracing::debug;

use crate::models::liquidity::TraderJoeV21Pool;

// ── Factory / Router addresses (Arbitrum) ────────────────────────────────────

/// Trader Joe V2.1 LBFactory on Arbitrum
pub const TRADER_JOE_V21_FACTORY: &str = "0x8e42f2F4101563bF679975178e880FD87d3eFd4e";

/// Trader Joe V2.1 LBRouter on Arbitrum
pub const TRADER_JOE_V21_ROUTER: &str = "0xb4315e873dBcf96Ffd0acd8EA43f689D8c20fB30";

/// Common bin steps used by Trader Joe V2.1 pools.
/// Each value is in basis points (e.g. 15 = 0.15% price increment per bin).
pub const COMMON_BIN_STEPS: &[u32] = &[1, 5, 10, 15, 20, 25];

// ── ABI selectors ────────────────────────────────────────────────────────────

/// `getLBPairInformation(address,address,uint256)` → returns (binStep, LBPair, bool, bool)
/// selector = keccak256("getLBPairInformation(address,address,uint256)")[:4]
const GET_LB_PAIR_INFORMATION_SELECTOR: &str = "704037bd";

/// `getActiveId()` → returns (uint24 activeId)
const GET_ACTIVE_ID_SELECTOR: &str = "44a185bb";

/// `getBin(uint24)` → returns (uint128 binReserveX, uint128 binReserveY)
const GET_BIN_SELECTOR: &str = "0abe9688";

/// `getTokenX()` → returns (address)
const GET_TOKEN_X_SELECTOR: &str = "0aded1c7";

/// `getTokenY()` → returns (address)
const GET_TOKEN_Y_SELECTOR: &str = "684e41f4";

/// `feeParameters()` → returns (baseFactor, filterPeriod, decayPeriod, reductionFactor,
///                               variableFeeControl, protocolShare, maxVolatilityAccumulated,
///                               volatilityAccumulated, volatilityReference, indexRef, time)
/// For V2.1, we use `getStaticFeeParameters()`.
const GET_STATIC_FEE_PARAMETERS_SELECTOR: &str = "7683dd36";

// ── ABI encoding helpers ─────────────────────────────────────────────────────

/// Pad an Ethereum address (strip 0x, left-pad to 32 bytes).
fn pad_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

/// Encode a uint256 value as a 32-byte hex word.
fn encode_uint256(value: u128) -> String {
    format!("{:064x}", value)
}

/// Decode a 32-byte ABI-encoded address from hex.
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

/// Decode a uint256 from a single 32-byte hex word.
fn decode_uint256(hex: &str) -> Option<u128> {
    let clean = hex.trim_start_matches("0x");
    if clean.is_empty() {
        return None;
    }
    let trimmed = clean.trim_start_matches('0');
    if trimmed.is_empty() {
        return Some(0);
    }
    u128::from_str_radix(trimmed, 16).ok()
}

/// Decode (uint128, uint128) from two consecutive 32-byte words.
fn decode_two_uint128(hex: &str) -> Option<(u128, u128)> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 128 {
        return None;
    }
    let a = decode_uint256(&format!("0x{}", &clean[..64]))?;
    let b = decode_uint256(&format!("0x{}", &clean[64..128]))?;
    Some((a, b))
}

/// Encode `getLBPairInformation(tokenX, tokenY, binStep)` call.
fn encode_get_lb_pair_information(token_x: &str, token_y: &str, bin_step: u32) -> String {
    format!(
        "0x{}{}{}{}",
        GET_LB_PAIR_INFORMATION_SELECTOR,
        pad_address(token_x),
        pad_address(token_y),
        encode_uint256(bin_step as u128),
    )
}

/// Encode `getBin(uint24 id)` call.
fn encode_get_bin(bin_id: u32) -> String {
    format!(
        "0x{}{}",
        GET_BIN_SELECTOR,
        encode_uint256(bin_id as u128),
    )
}

// ── Pool discovery ────────────────────────────────────────────────────────────

/// Fetch a Trader Joe V2.1 LBPair for the given token pair and bin step.
///
/// Returns `None` if no pair exists for this bin step.
pub async fn fetch_pool(
    rpc: &EthClient,
    factory: &str,
    token_a: &str,
    token_b: &str,
    bin_step: u32,
) -> anyhow::Result<Option<TraderJoeV21Pool>> {
    // 1. Query factory for the LBPair address
    let call_data = encode_get_lb_pair_information(token_a, token_b, bin_step);
    let result_hex = rpc.call(factory, &call_data).await?;
    let clean = result_hex.trim_start_matches("0x");

    // getLBPairInformation returns a struct: (uint16 binStep, address LBPair, bool createdByOwner, bool ignoredForRouting)
    // ABI-encoded as 4 words: binStep, LBPair_address, createdByOwner, ignoredForRouting
    if clean.len() < 256 {
        debug!(
            factory,
            token_a, token_b, bin_step, "Trader Joe V2.1: short response from factory"
        );
        return Ok(None);
    }

    // The LBPair address is in the second word (offset 64..128)
    let pair_address = match decode_address(&format!("0x{}", &clean[64..128])) {
        Some(addr) => addr,
        None => {
            debug!(
                factory,
                token_a, token_b, bin_step, "No Trader Joe V2.1 pair found"
            );
            return Ok(None);
        }
    };

    debug!(
        pair = %pair_address,
        token_a, token_b, bin_step,
        "Found Trader Joe V2.1 LBPair"
    );

    // 2. Fetch tokenX, tokenY, activeId, and fee parameters
    let token_x_call = format!("0x{GET_TOKEN_X_SELECTOR}");
    let token_y_call = format!("0x{GET_TOKEN_Y_SELECTOR}");
    let active_id_call = format!("0x{GET_ACTIVE_ID_SELECTOR}");
    let fee_params_call = format!("0x{GET_STATIC_FEE_PARAMETERS_SELECTOR}");

    let (tx_hex, ty_hex, active_id_hex, fee_hex) = tokio::try_join!(
        rpc.call(&pair_address, &token_x_call),
        rpc.call(&pair_address, &token_y_call),
        rpc.call(&pair_address, &active_id_call),
        rpc.call(&pair_address, &fee_params_call),
    )?;

    let token_x = decode_address(&tx_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode tokenX from {}", pair_address))?;
    let token_y = decode_address(&ty_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode tokenY from {}", pair_address))?;
    let active_bin_id = decode_uint256(&active_id_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode activeId from {}", pair_address))?
        as u32;

    // 3. Fetch reserves in the active bin
    let bin_call = encode_get_bin(active_bin_id);
    let bin_hex = rpc.call(&pair_address, &bin_call).await?;
    let (reserve_x, reserve_y) = decode_two_uint128(&bin_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode bin reserves from {}", pair_address))?;

    // 4. Parse fee parameters
    // getStaticFeeParameters returns: (baseFactor, filterPeriod, decayPeriod, reductionFactor,
    //                                  variableFeeControl, protocolShare, maxVolatilityAccumulated)
    // baseFee = baseFactor * binStep * 10 (in 1e18 precision, but we want bps)
    // Simplified: base_fee_bps ≈ baseFactor * binStep / 1e4
    let fee_clean = fee_hex.trim_start_matches("0x");
    let base_factor = if fee_clean.len() >= 64 {
        decode_uint256(&format!("0x{}", &fee_clean[..64])).unwrap_or(15)
    } else {
        15 // Default base factor
    };

    // Trader Joe V2.1 base fee formula: baseFee = baseFactor * binStep * 1e10
    // In basis points: base_fee_bps = baseFactor * binStep / 1e4
    // Simplified: typical base factor is ~15, bin step 15 → base fee ≈ 0.225% = ~22 bps
    let base_fee_bps = ((base_factor as u64 * bin_step as u64) / 10_000).max(1) as u32;
    // Variable fee is dynamic, estimate it as a fraction of base fee for now
    let variable_fee_bps = base_fee_bps / 2; // Conservative estimate
    let total_fee_bps = base_fee_bps + variable_fee_bps;

    debug!(
        address = %pair_address,
        active_bin_id,
        bin_step,
        reserve_x = %reserve_x,
        reserve_y = %reserve_y,
        base_fee_bps,
        total_fee_bps,
        "Trader Joe V2.1 pool details"
    );

    Ok(Some(TraderJoeV21Pool {
        address: pair_address,
        token_x,
        token_y,
        bin_step,
        active_bin_id,
        reserve_x: reserve_x.to_string(),
        reserve_y: reserve_y.to_string(),
        total_fee_bps,
        base_fee_bps,
        variable_fee_bps,
    }))
}

/// Discover Trader Joe V2.1 pools for a token pair across all common bin steps.
///
/// Returns all pools that exist (different bin steps = different pools/fee levels).
pub async fn fetch_all_bin_steps(
    rpc: &EthClient,
    factory: &str,
    token_a: &str,
    token_b: &str,
) -> anyhow::Result<Vec<TraderJoeV21Pool>> {
    let mut pools = Vec::new();

    for &bin_step in COMMON_BIN_STEPS {
        match fetch_pool(rpc, factory, token_a, token_b, bin_step).await {
            Ok(Some(pool)) => pools.push(pool),
            Ok(None) => {} // No pool for this bin step
            Err(e) => {
                debug!(
                    error = %e,
                    token_a, token_b, bin_step,
                    "Trader Joe V2.1 pool discovery error for bin step"
                );
            }
        }
    }

    Ok(pools)
}

// ── Pool sync ────────────────────────────────────────────────────────────────

/// Refresh the active bin reserves and ID for an existing Trader Joe V2.1 pool.
pub async fn sync_pool(rpc: &EthClient, pool: &mut TraderJoeV21Pool) -> anyhow::Result<()> {
    // 1. Refresh active bin ID (may have shifted)
    let active_id_call = format!("0x{GET_ACTIVE_ID_SELECTOR}");
    let active_id_hex = rpc.call(&pool.address, &active_id_call).await?;
    let new_active_id = decode_uint256(&active_id_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode activeId from {}", pool.address))?
        as u32;

    pool.active_bin_id = new_active_id;

    // 2. Refresh reserves in the (possibly new) active bin
    let bin_call = encode_get_bin(new_active_id);
    let bin_hex = rpc.call(&pool.address, &bin_call).await?;
    let (reserve_x, reserve_y) = decode_two_uint128(&bin_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode bin reserves from {}", pool.address))?;

    pool.reserve_x = reserve_x.to_string();
    pool.reserve_y = reserve_y.to_string();

    debug!(
        address = %pool.address,
        active_bin_id = new_active_id,
        reserve_x = %pool.reserve_x,
        reserve_y = %pool.reserve_y,
        "Synced Trader Joe V2.1 pool"
    );
    Ok(())
}

// ── Spot price ───────────────────────────────────────────────────────────────

/// Returns the spot price of tokenX denominated in tokenY using the active bin.
///
/// In Trader Joe V2.1, the price at a bin is: price = (1 + binStep/10000) ^ (binId - 2^23)
/// This is a simplified approximation using the reserves in the active bin.
pub fn spot_price(pool: &TraderJoeV21Pool) -> Option<f64> {
    let rx = pool.reserve_x.parse::<f64>().ok()?;
    let ry = pool.reserve_y.parse::<f64>().ok()?;

    if rx == 0.0 && ry == 0.0 {
        return None;
    }

    // Use bin pricing formula: price = (1 + binStep/10000) ^ (activeId - 8388608)
    // where 8388608 = 2^23 is the "zero-price" bin
    let bin_step_factor = 1.0 + (pool.bin_step as f64) / 10_000.0;
    let exponent = pool.active_bin_id as f64 - 8_388_608.0;
    let price = bin_step_factor.powf(exponent);

    if price.is_finite() && price > 0.0 {
        Some(price)
    } else if rx > 0.0 {
        // Fallback to reserve ratio
        Some(ry / rx)
    } else {
        None
    }
}

// ── Swap math ────────────────────────────────────────────────────────────────

/// Compute the output amount for a Trader Joe V2.1 swap within the active bin.
///
/// This is a simplified single-bin approximation. For large swaps that cross
/// multiple bins, the output will be less accurate (and conservative).
///
/// `swap_for_y`: true = selling tokenX for tokenY (X→Y), false = Y→X.
///
/// The Liquidity Book swap formula within a single bin:
/// - Apply fee: amount_in_after_fee = amount_in * (1 - total_fee)
/// - Output is proportional to the available reserve in the active bin
pub fn get_amount_out(pool: &TraderJoeV21Pool, amount_in: u128, swap_for_y: bool) -> Option<u128> {
    if amount_in == 0 {
        return None;
    }

    let reserve_x = pool.reserve_x.parse::<u128>().ok()?;
    let reserve_y = pool.reserve_y.parse::<u128>().ok()?;

    let (reserve_in, reserve_out) = if swap_for_y {
        (reserve_x, reserve_y)
    } else {
        (reserve_y, reserve_x)
    };

    if reserve_out == 0 {
        return None;
    }

    // Apply fee
    let fee_multiplier = (10_000u128).checked_sub(pool.total_fee_bps as u128)?;
    let amount_in_after_fee = amount_in.checked_mul(fee_multiplier)? / 10_000;

    // Within a single bin, the price is fixed. The output is:
    // If the bin has enough liquidity, output = amount_in_after_fee * (reserve_out / reserve_in)
    // Capped at the available reserve_out
    if reserve_in == 0 {
        // Bin has no reserve on the input side — use the bin price to compute output
        // Price = (1 + binStep/10000) ^ (activeId - 2^23)
        let bin_step_factor = 1.0 + (pool.bin_step as f64) / 10_000.0;
        let exponent = pool.active_bin_id as f64 - 8_388_608.0;
        let price = bin_step_factor.powf(exponent);

        let output = if swap_for_y {
            // Selling X for Y: output = amount_in_after_fee * price
            (amount_in_after_fee as f64 * price) as u128
        } else {
            // Selling Y for X: output = amount_in_after_fee / price
            if price == 0.0 {
                return None;
            }
            (amount_in_after_fee as f64 / price) as u128
        };

        // Cap at available reserves
        Some(output.min(reserve_out))
    } else {
        // Constant-sum within the bin (fixed price per bin)
        // output = amount_in_after_fee * reserve_out / reserve_in
        let numerator = amount_in_after_fee.checked_mul(reserve_out)?;
        let output = numerator.checked_div(reserve_in)?;

        // Cap at available reserves in this bin
        Some(output.min(reserve_out))
    }
}

/// Compute the required input to receive a specific output amount.
///
/// Inverse of `get_amount_out`, using the same single-bin approximation.
pub fn get_amount_in(pool: &TraderJoeV21Pool, amount_out: u128, swap_for_y: bool) -> Option<u128> {
    if amount_out == 0 {
        return None;
    }

    let reserve_x = pool.reserve_x.parse::<u128>().ok()?;
    let reserve_y = pool.reserve_y.parse::<u128>().ok()?;

    let (reserve_in, reserve_out) = if swap_for_y {
        (reserve_x, reserve_y)
    } else {
        (reserve_y, reserve_x)
    };

    // Cannot get more than what's in the bin
    if amount_out >= reserve_out || reserve_out == 0 {
        return None;
    }

    // amount_out = (amount_in * fee_multiplier / 10000) * reserve_out / reserve_in
    // amount_in = (amount_out * reserve_in * 10000) / (reserve_out * fee_multiplier) + 1
    let fee_multiplier = (10_000u128).checked_sub(pool.total_fee_bps as u128)?;

    if reserve_in == 0 {
        // Use bin price formula
        let bin_step_factor = 1.0 + (pool.bin_step as f64) / 10_000.0;
        let exponent = pool.active_bin_id as f64 - 8_388_608.0;
        let price = bin_step_factor.powf(exponent);

        let input_before_fee = if swap_for_y {
            if price == 0.0 { return None; }
            amount_out as f64 / price
        } else {
            amount_out as f64 * price
        };

        // Add fee back: amount_in = input_before_fee * 10000 / fee_multiplier
        let amount_in = (input_before_fee * 10_000.0 / fee_multiplier as f64).ceil() as u128;
        Some(amount_in)
    } else {
        let numerator = amount_out
            .checked_mul(reserve_in)?
            .checked_mul(10_000)?;
        let denominator = reserve_out.checked_mul(fee_multiplier)?;
        numerator.checked_div(denominator)?.checked_add(1) // Round up
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pool(rx: u128, ry: u128, bin_step: u32, fee_bps: u32) -> TraderJoeV21Pool {
        TraderJoeV21Pool {
            address: "0x0000".into(),
            token_x: "0xtokenX".into(),
            token_y: "0xtokenY".into(),
            bin_step,
            active_bin_id: 8_388_608, // centered = price ~1.0
            reserve_x: rx.to_string(),
            reserve_y: ry.to_string(),
            total_fee_bps: fee_bps,
            base_fee_bps: fee_bps * 2 / 3,
            variable_fee_bps: fee_bps / 3,
        }
    }

    #[test]
    fn swap_basic_x_for_y() {
        let pool = make_pool(1_000_000_000, 1_000_000_000, 15, 30);
        let out = get_amount_out(&pool, 10_000, true).unwrap();
        assert!(out > 0);
        assert!(out <= 10_000); // can't get more than input at 1:1 price
    }

    #[test]
    fn swap_basic_y_for_x() {
        let pool = make_pool(1_000_000_000, 1_000_000_000, 15, 30);
        let out = get_amount_out(&pool, 10_000, false).unwrap();
        assert!(out > 0);
        assert!(out <= 10_000);
    }

    #[test]
    fn swap_respects_fee() {
        let pool_low = make_pool(1_000_000, 1_000_000, 15, 10);
        let pool_high = make_pool(1_000_000, 1_000_000, 15, 100);
        let out_low = get_amount_out(&pool_low, 10_000, true).unwrap();
        let out_high = get_amount_out(&pool_high, 10_000, true).unwrap();
        assert!(out_low > out_high, "Lower fee should give more output");
    }

    #[test]
    fn swap_capped_at_reserve() {
        let pool = make_pool(1_000_000, 100, 15, 30);
        let out = get_amount_out(&pool, 1_000_000_000, true).unwrap();
        assert!(out <= 100, "Output should be capped at reserve_y");
    }

    #[test]
    fn swap_zero_reserve_out_returns_none() {
        let pool = make_pool(1_000_000, 0, 15, 30);
        assert!(get_amount_out(&pool, 1000, true).is_none());
    }

    #[test]
    fn swap_zero_amount_returns_none() {
        let pool = make_pool(1_000_000, 1_000_000, 15, 30);
        assert!(get_amount_out(&pool, 0, true).is_none());
    }

    #[test]
    fn get_amount_in_round_trip() {
        let pool = make_pool(1_000_000_000, 1_000_000_000, 15, 30);
        let amount_in = 10_000u128;
        let out = get_amount_out(&pool, amount_in, true).unwrap();
        assert!(out > 0);
        let in_back = get_amount_in(&pool, out, true).unwrap();
        // Round-trip should require at least as much input
        assert!(in_back >= amount_in);
    }

    #[test]
    fn get_amount_in_exceeding_reserve_returns_none() {
        let pool = make_pool(1_000_000, 1_000_000, 15, 30);
        assert!(get_amount_in(&pool, 1_000_001, true).is_none());
    }

    #[test]
    fn spot_price_at_center_bin() {
        let pool = make_pool(1_000_000, 1_000_000, 15, 30);
        let price = spot_price(&pool).unwrap();
        // At center bin (8388608), price should be ~1.0
        assert!(
            (price - 1.0).abs() < 0.01,
            "Spot price at center bin should be ~1.0, got {price}"
        );
    }

    #[test]
    fn spot_price_zero_reserves_returns_none() {
        let pool = make_pool(0, 0, 15, 30);
        assert!(spot_price(&pool).is_none());
    }

    // ── ABI encoding tests ───────────────────────────────────────────────────

    #[test]
    fn encode_get_lb_pair_information_length() {
        let data = encode_get_lb_pair_information(
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            15,
        );
        // "0x" + 8 (selector) + 64*3 (three params) = 2 + 8 + 192 = 202
        assert_eq!(data.len(), 2 + 8 + 64 * 3);
    }

    #[test]
    fn encode_get_bin_length() {
        let data = encode_get_bin(8_388_608);
        // "0x" + 8 (selector) + 64 (binId) = 74
        assert_eq!(data.len(), 2 + 8 + 64);
    }

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
    fn decode_uint256_basic() {
        let hex = format!("0x{:064x}", 42u128);
        assert_eq!(decode_uint256(&hex), Some(42));
    }

    #[test]
    fn decode_uint256_zero() {
        let hex = format!("0x{:064x}", 0u128);
        assert_eq!(decode_uint256(&hex), Some(0));
    }

    #[test]
    fn decode_two_uint128_ok() {
        let r0_hex = format!("{:064x}", 1000u128);
        let r1_hex = format!("{:064x}", 2000u128);
        let hex = format!("0x{r0_hex}{r1_hex}");
        let (a, b) = decode_two_uint128(&hex).unwrap();
        assert_eq!(a, 1000);
        assert_eq!(b, 2000);
    }
}
