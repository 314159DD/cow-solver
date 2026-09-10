use shared::rpc::EthClient;
use tracing::debug;

use crate::models::liquidity::{UniswapV3Pool, V3Tick};

// ── Factory address ───────────────────────────────────────────────────────────

pub const UNISWAP_V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

/// Supported fee tiers (in hundredths of a bip, matching Uniswap V3 contract values).
pub const FEE_TIERS: [u32; 4] = [100, 500, 3_000, 10_000];

/// Return the tick spacing for a given fee tier.
///
/// Uniswap V3 uses fixed tick spacings per fee tier:
///   100  bps (0.01%) → tick spacing 1
///   500  bps (0.05%) → tick spacing 10
///   3000 bps (0.30%) → tick spacing 60
///   10000 bps (1.00%) → tick spacing 200
pub fn tick_spacing(fee: u32) -> i32 {
    match fee {
        100 => 1,
        500 => 10,
        3_000 => 60,
        10_000 => 200,
        _ => 60, // default to medium fee tier spacing
    }
}

/// Minimum tick index for Uniswap V3.
pub const MIN_TICK: i32 = -887272;
/// Maximum tick index for Uniswap V3.
pub const MAX_TICK: i32 = 887272;

// ── ABI selectors ─────────────────────────────────────────────────────────────

/// `getPool(address,address,uint24)` → 4-byte selector
const GET_POOL_SELECTOR: &str = "1698ee82";

/// `slot0()` → 4-byte selector
const SLOT0_SELECTOR: &str = "3850c7bd";

/// `liquidity()` → 4-byte selector
const LIQUIDITY_SELECTOR: &str = "1a686502";

/// `token0()` → 4-byte selector
const TOKEN0_SELECTOR: &str = "0dfe1681";

/// `token1()` → 4-byte selector
const TOKEN1_SELECTOR: &str = "d21220a7";

/// `tickBitmap(int16)` → 4-byte selector
const TICK_BITMAP_SELECTOR: &str = "5339c296";

/// `ticks(int24)` → 4-byte selector (returns tick info including liquidityNet)
const TICKS_SELECTOR: &str = "f30dba93";

/// `tickSpacing()` → 4-byte selector
const TICK_SPACING_SELECTOR: &str = "d0c93a7c";

// ── ABI encoding helpers ──────────────────────────────────────────────────────

/// Pad an Ethereum address (strip 0x, left-pad to 32 bytes).
fn pad_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

/// Encode a factory `getPool(token_a, token_b, fee)` call.
fn encode_get_pool(token_a: &str, token_b: &str, fee: u32) -> String {
    let a = pad_address(token_a);
    let b = pad_address(token_b);
    let f = format!("{:064x}", fee);
    format!("0x{GET_POOL_SELECTOR}{a}{b}{f}")
}

/// Decode a 32-byte ABI-encoded address.  Returns None for zero address.
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

/// Decode `slot0()` output.
///
/// Returns `(sqrtPriceX96: u128, tick: i32)`.
/// Word 0: sqrtPriceX96 (uint160), word 1: tick (int24).
fn decode_slot0(hex: &str) -> Option<(u128, i32)> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 128 {
        return None;
    }
    // Word 0: sqrtPriceX96 (uint160, fits in u128 for practical prices)
    let sqrt_price = u128::from_str_radix(&clean[..64], 16).ok()?;
    // Word 1: tick (int24, sign-extended from lower 24 bits)
    let tick_raw = u32::from_str_radix(&clean[64..128], 16).ok()?;
    let tick = if tick_raw & 0x800000 != 0 {
        // Sign-extend int24
        (tick_raw | 0xFF000000u32) as i32
    } else {
        (tick_raw & 0x7FFFFF) as i32
    };
    Some((sqrt_price, tick))
}

/// Decode `liquidity()` output — a single uint128.
fn decode_liquidity(hex: &str) -> Option<u128> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 32 {
        return None;
    }
    u128::from_str_radix(&clean[clean.len().saturating_sub(32)..], 16).ok()
}

// ── Tick bitmap helpers ──────────────────────────────────────────────────────

/// Compress a raw tick index to the bitmap coordinate system.
///
/// Uniswap V3 only stores initialized ticks at multiples of `tick_spacing`.
/// The bitmap maps compressed ticks (tick / tickSpacing) to bits in 256-bit words.
///
/// Returns `(word_pos: i16, bit_pos: u8)` where:
///   - `word_pos` = compressed_tick >> 8  (which 256-bit word)
///   - `bit_pos`  = compressed_tick & 0xFF (which bit within the word)
pub fn tick_bitmap_position(tick: i32, spacing: i32) -> (i16, u8) {
    // Compressed tick: integer division rounding towards negative infinity
    let compressed = if tick < 0 && tick % spacing != 0 {
        tick / spacing - 1
    } else {
        tick / spacing
    };
    let word_pos = (compressed >> 8) as i16;
    let bit_pos = (compressed & 0xFF) as u8;
    (word_pos, bit_pos)
}

/// Encode a `tickBitmap(int16)` call for a given word position.
fn encode_tick_bitmap(word_pos: i16) -> String {
    // int16 is sign-extended to 32 bytes in ABI encoding
    let encoded = if word_pos < 0 {
        // Sign-extend: negative int16 → 64 hex chars of ff-padded
        let abs = (-(word_pos as i32)) as u32;
        let twos_complement = (!abs).wrapping_add(1) & 0xFFFF;
        format!("{:0>64}", format!("{:x}", (twos_complement as u64) | 0xFFFFFFFFFFFF0000u64))
    } else {
        format!("{:064x}", word_pos as u64)
    };
    format!("0x{TICK_BITMAP_SELECTOR}{encoded}")
}

/// Encode a `ticks(int24)` call for a given tick index.
fn encode_ticks_call(tick: i32) -> String {
    // int24 sign-extended to 32 bytes
    let encoded = if tick < 0 {
        let abs = (-tick) as u32;
        let twos_complement = (!abs).wrapping_add(1) & 0xFFFFFF;
        format!("{:0>64}", format!("{:x}", (twos_complement as u64) | 0xFFFFFFFFFF000000u64))
    } else {
        format!("{:064x}", tick as u64)
    };
    format!("0x{TICKS_SELECTOR}{encoded}")
}

/// Decode a uint256 bitmap word from hex return data.
fn decode_bitmap_word(hex: &str) -> Option<[u8; 32]> {
    let clean = hex.trim_start_matches("0x");
    if clean.len() < 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

/// Decode the `ticks(int24)` return data.
///
/// Returns `liquidityNet` (int128) from the tick info struct.
/// The return layout is:
///   word 0: liquidityGross (uint128)
///   word 1: liquidityNet (int128)
///   word 2: feeGrowthOutside0X128 (uint256)
///   word 3: feeGrowthOutside1X128 (uint256)
///   word 4: tickCumulativeOutside (int56)
///   word 5: secondsPerLiquidityOutsideX128 (uint160)
///   word 6: secondsOutside (uint32)
///   word 7: initialized (bool)
fn decode_tick_info(hex: &str) -> Option<i128> {
    let clean = hex.trim_start_matches("0x");
    // Need at least 2 words (128 hex chars)
    if clean.len() < 128 {
        return None;
    }
    // Word 1: liquidityNet (int128), occupies lower 128 bits of the 256-bit word
    let word1 = &clean[64..128];
    let raw = u128::from_str_radix(word1, 16).ok()?;
    // Convert u128 to i128 (two's complement)
    Some(raw as i128)
}

/// Check if a specific bit is set in a 256-bit word (big-endian byte array).
///
/// Bit 0 is the least significant bit (rightmost), bit 255 is the most significant.
fn is_bit_set(word: &[u8; 32], bit_pos: u8) -> bool {
    // word[31] has bits 0-7, word[30] has bits 8-15, etc.
    let byte_index = 31 - (bit_pos / 8) as usize;
    let bit_in_byte = bit_pos % 8;
    (word[byte_index] >> bit_in_byte) & 1 == 1
}

/// Find the next initialized tick at or below `bit_pos` in a bitmap word.
///
/// Scans from `bit_pos` downward to bit 0.
/// Returns `Some(bit_position)` of the first set bit, or `None` if no bits are set.
fn next_initialized_tick_within_word_lte(word: &[u8; 32], bit_pos: u8) -> Option<u8> {
    // Create a mask: all bits at positions <= bit_pos
    // Then AND with the word and find the most significant set bit
    for pos in (0..=bit_pos).rev() {
        if is_bit_set(word, pos) {
            return Some(pos);
        }
    }
    None
}

/// Find the next initialized tick at or above `bit_pos` in a bitmap word.
///
/// Scans from `bit_pos` upward to bit 255.
/// Returns `Some(bit_position)` of the first set bit, or `None` if no bits are set.
fn next_initialized_tick_within_word_gte(word: &[u8; 32], bit_pos: u8) -> Option<u8> {
    for pos in bit_pos..=255 {
        if is_bit_set(word, pos) {
            return Some(pos);
        }
    }
    None
}

/// Reconstruct the actual tick index from a word position and bit position.
fn tick_from_bitmap_pos(word_pos: i16, bit_pos: u8, spacing: i32) -> i32 {
    let compressed = (word_pos as i32) * 256 + (bit_pos as i32);
    compressed * spacing
}

// ── On-chain tick bitmap fetching ───────────────────────────────────────────

/// Fetch initialized ticks from the pool's tick bitmap within a range around
/// the current tick.
///
/// This performs bitmap word-level traversal: it reads bitmap words from the pool
/// contract, finds which bits are set (initialized ticks), then batch-fetches the
/// `liquidityNet` for each initialized tick.
///
/// # Arguments
/// * `rpc` - Ethereum RPC client
/// * `pool_address` - Address of the Uniswap V3 pool
/// * `current_tick` - The current tick of the pool
/// * `fee` - Fee tier (used to determine tick spacing)
/// * `num_words` - Number of bitmap words to scan in each direction from current tick
///
/// # Returns
/// A sorted vector of `V3Tick` entries with their `liquidityNet` values.
pub async fn fetch_tick_bitmap(
    rpc: &EthClient,
    pool_address: &str,
    current_tick: i32,
    fee: u32,
    num_words: u16,
) -> anyhow::Result<Vec<V3Tick>> {
    let spacing = tick_spacing(fee);
    let (center_word, _) = tick_bitmap_position(current_tick, spacing);

    // Determine the range of word positions to scan
    let word_start = (center_word as i32).saturating_sub(num_words as i32).max(i16::MIN as i32) as i16;
    let word_end = (center_word as i32).saturating_add(num_words as i32).min(i16::MAX as i32) as i16;

    // Phase 1: Batch-fetch all bitmap words
    let mut bitmap_calls = Vec::new();
    for wp in word_start..=word_end {
        bitmap_calls.push(shared::rpc::CallRequest {
            to: pool_address.to_string(),
            data: encode_tick_bitmap(wp),
        });
    }

    let bitmap_results = rpc.multicall(bitmap_calls).await?;

    // Phase 2: Scan bitmap words for initialized ticks
    let mut initialized_ticks = Vec::new();
    for (i, result) in bitmap_results.iter().enumerate() {
        if !result.success {
            continue;
        }
        let word = match decode_bitmap_word(&result.return_data) {
            Some(w) => w,
            None => continue,
        };

        // Check if the entire word is zero (no initialized ticks)
        if word == [0u8; 32] {
            continue;
        }

        let wp = word_start + i as i16;
        // Scan all 256 bits
        for bit in 0..=255u8 {
            if is_bit_set(&word, bit) {
                let tick = tick_from_bitmap_pos(wp, bit, spacing);
                initialized_ticks.push(tick);
            }
        }
    }

    if initialized_ticks.is_empty() {
        return Ok(Vec::new());
    }

    debug!(
        pool = pool_address,
        count = initialized_ticks.len(),
        "Found initialized ticks from bitmap"
    );

    // Phase 3: Batch-fetch liquidityNet for each initialized tick
    let tick_calls: Vec<shared::rpc::CallRequest> = initialized_ticks
        .iter()
        .map(|&tick| shared::rpc::CallRequest {
            to: pool_address.to_string(),
            data: encode_ticks_call(tick),
        })
        .collect();

    let tick_results = rpc.multicall(tick_calls).await?;

    let mut ticks = Vec::with_capacity(initialized_ticks.len());
    for (tick_index, result) in initialized_ticks.iter().zip(tick_results.iter()) {
        if !result.success {
            continue;
        }
        if let Some(liquidity_net) = decode_tick_info(&result.return_data) {
            ticks.push(V3Tick {
                index: *tick_index,
                liquidity_net,
            });
        }
    }

    // Sort ascending by tick index (required by the swap simulation)
    ticks.sort_by_key(|t| t.index);

    debug!(
        pool = pool_address,
        tick_count = ticks.len(),
        range = format!("[{}, {}]",
            ticks.first().map(|t| t.index).unwrap_or(0),
            ticks.last().map(|t| t.index).unwrap_or(0)),
        "Fetched tick liquidityNet data"
    );

    Ok(ticks)
}

/// Fetch tick bitmap data and populate a pool's tick array.
///
/// Uses `num_words = 4` by default, which covers ~2048 ticks in each direction
/// (with tick spacing 60, that's ~122,880 tick-index range, roughly ±6000% price range).
pub async fn populate_pool_ticks(
    rpc: &EthClient,
    pool: &mut UniswapV3Pool,
) -> anyhow::Result<()> {
    let ticks = fetch_tick_bitmap(
        rpc,
        &pool.address,
        pool.tick,
        pool.fee,
        4, // scan 4 words in each direction
    )
    .await?;

    pool.ticks = if ticks.is_empty() { None } else { Some(ticks) };
    Ok(())
}

// ── Pool monitor ──────────────────────────────────────────────────────────────

/// Fetch a Uniswap V3 pool for the given token pair and fee tier from the factory.
///
/// Returns `None` if no pool exists for this fee tier.
pub async fn fetch_pool(
    rpc: &EthClient,
    token_a: &str,
    token_b: &str,
    fee: u32,
) -> anyhow::Result<Option<UniswapV3Pool>> {
    // 1. Lookup pool address from factory
    let call_data = encode_get_pool(token_a, token_b, fee);
    let pool_hex = rpc.call(UNISWAP_V3_FACTORY, &call_data).await?;
    let pool_address = match decode_address(&pool_hex) {
        Some(addr) => addr,
        None => {
            debug!(
                token_a = token_a,
                token_b = token_b,
                fee = fee,
                "No V3 pool found"
            );
            return Ok(None);
        }
    };

    debug!(
        pool = %pool_address,
        token_a = token_a,
        token_b = token_b,
        fee = fee,
        "Found V3 pool"
    );

    // 2. Fetch token0, token1, slot0, liquidity in parallel
    let token0_call = format!("0x{TOKEN0_SELECTOR}");
    let token1_call = format!("0x{TOKEN1_SELECTOR}");
    let slot0_call = format!("0x{SLOT0_SELECTOR}");
    let liq_call = format!("0x{LIQUIDITY_SELECTOR}");

    let (t0_hex, t1_hex, slot0_hex, liq_hex) = tokio::try_join!(
        rpc.call(&pool_address, &token0_call),
        rpc.call(&pool_address, &token1_call),
        rpc.call(&pool_address, &slot0_call),
        rpc.call(&pool_address, &liq_call),
    )?;

    let token0 = decode_address(&t0_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode token0 from {}", pool_address))?;
    let token1 = decode_address(&t1_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode token1 from {}", pool_address))?;
    let (sqrt_price_x96, tick) = decode_slot0(&slot0_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode slot0 from {}", pool_address))?;
    let liquidity = decode_liquidity(&liq_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode liquidity from {}", pool_address))?;

    Ok(Some(UniswapV3Pool {
        address: pool_address,
        token0,
        token1,
        sqrt_price_x96: sqrt_price_x96.to_string(),
        tick,
        liquidity: liquidity.to_string(),
        fee,
        ticks: None, // Tick bitmap fetched separately when needed
    }))
}

/// Fetch all available fee tiers for a token pair and return the pools that exist.
pub async fn fetch_all_fee_tiers(
    rpc: &EthClient,
    token_a: &str,
    token_b: &str,
) -> anyhow::Result<Vec<UniswapV3Pool>> {
    let mut pools = Vec::new();
    for &fee in &FEE_TIERS {
        match fetch_pool(rpc, token_a, token_b, fee).await {
            Ok(Some(pool)) => pools.push(pool),
            Ok(None) => {}
            Err(e) => {
                debug!(fee = fee, error = %e, "V3 fee tier fetch failed, skipping");
            }
        }
    }
    Ok(pools)
}

/// Refresh slot0 and liquidity for an existing V3 pool in-place.
pub async fn sync_pool(rpc: &EthClient, pool: &mut UniswapV3Pool) -> anyhow::Result<()> {
    let slot0_call = format!("0x{SLOT0_SELECTOR}");
    let liq_call = format!("0x{LIQUIDITY_SELECTOR}");

    let (slot0_hex, liq_hex) = tokio::try_join!(
        rpc.call(&pool.address, &slot0_call),
        rpc.call(&pool.address, &liq_call),
    )?;

    let (sqrt_price_x96, tick) = decode_slot0(&slot0_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode slot0 from {}", pool.address))?;
    let liquidity = decode_liquidity(&liq_hex)
        .ok_or_else(|| anyhow::anyhow!("Failed to decode liquidity from {}", pool.address))?;

    pool.sqrt_price_x96 = sqrt_price_x96.to_string();
    pool.tick = tick;
    pool.liquidity = liquidity.to_string();

    debug!(
        address = %pool.address,
        sqrt_price_x96 = %pool.sqrt_price_x96,
        tick = pool.tick,
        liquidity = %pool.liquidity,
        "Synced V3 pool"
    );
    Ok(())
}

// ── Spot price ────────────────────────────────────────────────────────────────

/// Returns the spot price of token0 denominated in token1 using sqrtPriceX96.
/// price = (sqrtPriceX96 / 2^96)^2
pub fn spot_price(pool: &UniswapV3Pool) -> Option<f64> {
    let sqrt_price = pool.sqrt_price_x96.parse::<f64>().ok()?;
    if sqrt_price == 0.0 {
        return None;
    }
    let q96 = (1u128 << 96) as f64;
    let price = (sqrt_price / q96).powi(2);
    Some(price)
}

// ── Approximate swap math (full tick traversal deferred to Sprint 2) ──────────

/// Compute approximate output for a V3 pool using sqrtPriceX96.
/// This is a simplified approximation — real implementation needs tick traversal.
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

    // V3 single-tick-range approximation WITH price impact.
    //
    // Within one tick range, V3 behaves like a constant-product pool with
    // virtual reserves derived from L (liquidity) and √P (sqrtPrice).
    //
    // For zero_for_one (token0 → token1):
    //   virtual_reserve0 = L / √P
    //   virtual_reserve1 = L × √P
    //   amount_out = reserve1 × amount_in / (reserve0 + amount_in)  [constant product]
    //
    // For one_for_zero (token1 → token0):
    //   Same formula with reserves swapped.
    //
    // This correctly models price impact from the swap size relative to liquidity.
    // Previous version assumed infinite liquidity (no price impact).

    let fee_factor = 1_000_000u128.checked_sub(fee as u128)?;
    let amount_in_after_fee = (amount_in as f64) * (fee_factor as f64) / 1_000_000.0;

    // Convert sqrtPriceX96 to float for the virtual reserve calculation
    let sqrt_p = sqrt_price_x96 as f64 / (1u128 << 96) as f64;
    if sqrt_p <= 0.0 {
        return None;
    }
    let l = liquidity as f64;

    if zero_for_one {
        // token0 → token1
        let virtual_reserve0 = l / sqrt_p;
        let virtual_reserve1 = l * sqrt_p;
        if virtual_reserve0 <= 0.0 || virtual_reserve1 <= 0.0 {
            return None;
        }
        // Constant-product formula with price impact
        let out = virtual_reserve1 * amount_in_after_fee / (virtual_reserve0 + amount_in_after_fee);
        if out <= 0.0 || out > virtual_reserve1 {
            return None;
        }
        Some(out as u128)
    } else {
        // token1 → token0
        let virtual_reserve0 = l / sqrt_p;
        let virtual_reserve1 = l * sqrt_p;
        if virtual_reserve0 <= 0.0 || virtual_reserve1 <= 0.0 {
            return None;
        }
        let out = virtual_reserve0 * amount_in_after_fee / (virtual_reserve1 + amount_in_after_fee);
        if out <= 0.0 || out > virtual_reserve0 {
            return None;
        }
        Some(out as u128)
    }
}

// ── Tick-traversal swap simulation (production V3 math) ─────────────────────
//
// Uniswap V3 swap math:
//   Within a tick range the pool behaves like a constant-product pool
//   parameterised by the active liquidity L and the current sqrtPrice.
//
//   zeroForOne (selling token0, price decreases):
//     dx = L * (1/sqrt_P_new - 1/sqrt_P_old)     [token0 input]
//     dy = L * (sqrt_P_old - sqrt_P_new)           [token1 output]
//
//   oneForZero (selling token1, price increases):
//     dy = L * (sqrt_P_new - sqrt_P_old)           [token1 input]
//     dx = L * (1/sqrt_P_old - 1/sqrt_P_new)       [token0 output]
//
//   When the price reaches an initialized tick boundary, the active liquidity
//   is updated:  L_new = L_old + liquidityNet  (for left-to-right crossing)
//                L_new = L_old - liquidityNet  (for right-to-left crossing)
//
//   All sqrtPrice values are in Q64.96 fixed-point (u128).

/// Result of a tick-traversal swap simulation.
#[derive(Debug, Clone)]
pub struct V3SwapResult {
    /// Total output amount.
    pub amount_out: u128,
    /// Final sqrtPriceX96 after the swap.
    pub sqrt_price_x96_after: u128,
    /// Number of initialized ticks actually crossed during the swap.
    pub ticks_crossed: u32,
}

/// Compute the sqrtPriceX96 at a given tick index.
///
/// `sqrt(1.0001^tick) * 2^96`
///
/// For production accuracy we use the standard Uniswap approximation:
/// `log2(sqrt(1.0001)) = 0.5 * log2(1.0001) ≈ 7.2134e-5`
/// then reconstruct via `2^(96 + tick * 0.5 * log2(1.0001))`.
///
/// This function is only used for boundary prices during simulation.
fn sqrt_price_x96_at_tick(tick: i32) -> Option<u128> {
    // sqrt(1.0001) ≈ 1.00004999875 → ln(sqrt(1.0001)) ≈ 4.99987500e-5
    // sqrtPriceX96 = 2^96 * sqrt(1.0001)^tick = 2^96 * exp(tick * ln(sqrt(1.0001)))
    let exponent = (tick as f64) * (1.0001_f64.sqrt().ln());
    let val = (2.0_f64.powi(96)) * exponent.exp();
    if val.is_finite() && val >= 1.0 && val <= u128::MAX as f64 {
        Some(val as u128)
    } else if val < 1.0 {
        Some(1) // minimum representable
    } else {
        None // overflow
    }
}

/// Q96 constant: 2^96
const Q96: u128 = 1u128 << 96;

/// Maximum amount of token0 that can be extracted from the range [sqrt_P_lower, sqrt_P_current]
/// with the given liquidity. Used for zeroForOne swaps.
///
/// `dx = L * (1/sqrt_P_lower - 1/sqrt_P_upper)`
///      = L * (sqrt_P_upper - sqrt_P_lower) / (sqrt_P_lower * sqrt_P_upper)
///
/// We compute in u128 with intermediate shifts to avoid overflow.
fn max_token0_in_range(liquidity: u128, sqrt_p_lower: u128, sqrt_p_upper: u128) -> Option<u128> {
    if sqrt_p_lower == 0 || sqrt_p_upper == 0 || sqrt_p_lower >= sqrt_p_upper {
        return Some(0);
    }
    // numerator = L * (sqrt_p_upper - sqrt_p_lower) * Q96
    // denominator = sqrt_p_lower * sqrt_p_upper
    //
    // To avoid overflow, compute in stages:
    // dx = L * Q96 * (sqrt_p_upper - sqrt_p_lower) / (sqrt_p_lower * sqrt_p_upper)
    //
    // We use: L * Q96 / sqrt_p_upper * (delta / sqrt_p_lower)
    // But even L * Q96 can overflow u128 when L > ~2^32.
    //
    // Strategy: use f64 for amounts that would overflow u128, falling back gracefully.
    let delta = sqrt_p_upper.checked_sub(sqrt_p_lower)?;

    // Try integer math first (works when values are small enough).
    // L * delta can overflow, so check.
    if let Some(l_times_delta) = liquidity.checked_mul(delta) {
        // l_times_delta * Q96 / (sqrt_p_lower * sqrt_p_upper)
        // sqrt_p_lower * sqrt_p_upper may overflow u128, so use separate divisions.
        // dx = (l_times_delta / sqrt_p_lower) * Q96 / sqrt_p_upper
        // (losing some precision on first division but keeping u128 range)
        let step1 = l_times_delta / sqrt_p_lower; // truncating division
        let step2 = step1.checked_mul(Q96)?;
        return Some(step2 / sqrt_p_upper);
    }

    // Fallback: f64 computation (loses ~3 ulp but won't overflow)
    let dx = (liquidity as f64) * (delta as f64) * (Q96 as f64)
        / ((sqrt_p_lower as f64) * (sqrt_p_upper as f64));
    if dx.is_finite() && dx >= 0.0 && dx <= u128::MAX as f64 {
        Some(dx as u128)
    } else {
        None
    }
}

/// Maximum amount of token1 that can be extracted from the range [sqrt_P_current, sqrt_P_upper]
/// with the given liquidity. Used for oneForZero swaps.
///
/// `dy = L * (sqrt_P_upper - sqrt_P_lower) / Q96`
fn max_token1_in_range(liquidity: u128, sqrt_p_lower: u128, sqrt_p_upper: u128) -> Option<u128> {
    if sqrt_p_lower >= sqrt_p_upper {
        return Some(0);
    }
    let delta = sqrt_p_upper.checked_sub(sqrt_p_lower)?;
    if let Some(l_times_delta) = liquidity.checked_mul(delta) {
        Some(l_times_delta / Q96)
    } else {
        // f64 fallback
        let dy = (liquidity as f64) * (delta as f64) / (Q96 as f64);
        if dy.is_finite() && dy >= 0.0 && dy <= u128::MAX as f64 {
            Some(dy as u128)
        } else {
            None
        }
    }
}

/// Compute the token1 output for a given token0 input within a single tick range (zeroForOne).
///
/// Given: current sqrtP, target sqrtP (lower bound of tick range), liquidity, amount_in of token0.
/// Returns (amount0_consumed, amount1_out, new_sqrt_price).
fn compute_step_zero_for_one(
    sqrt_p_current: u128,
    sqrt_p_target: u128, // lower price boundary
    liquidity: u128,
    amount_in_remaining: u128,
) -> Option<(u128, u128, u128)> {
    if liquidity == 0 || sqrt_p_current <= sqrt_p_target {
        return Some((0, 0, sqrt_p_current));
    }

    // Max token0 that can be consumed to reach sqrt_p_target
    let max_dx = max_token0_in_range(liquidity, sqrt_p_target, sqrt_p_current)?;

    if amount_in_remaining >= max_dx {
        // We consume the entire range and cross the tick
        let dy = max_token1_in_range(liquidity, sqrt_p_target, sqrt_p_current)?;
        Some((max_dx, dy, sqrt_p_target))
    } else {
        // Partial fill within this range — compute new sqrtPrice
        // new_sqrt_price = sqrt_p_current * L * Q96 / (L * Q96 + amount_in * sqrt_p_current)
        //
        // Simplified: 1/sqrt_P_new = 1/sqrt_P_old + dx / (L * Q96)
        // => sqrt_P_new = sqrt_P_old * L_q96 / (L_q96 + dx * sqrt_P_old / Q96)
        //
        // We rearrange to avoid overflow:
        // sqrt_P_new = L * Q96 * sqrt_P_old / (L * Q96 + dx * sqrt_P_old)
        // But L * Q96 overflows u128. Use f64 for the price update.
        let l_q96 = (liquidity as f64) * (Q96 as f64);
        let dx_sp = (amount_in_remaining as f64) * (sqrt_p_current as f64);
        let new_sp = l_q96 * (sqrt_p_current as f64) / (l_q96 + dx_sp);
        let new_sqrt_price = if new_sp.is_finite() && new_sp >= 1.0 {
            let v = new_sp as u128;
            // Clamp: new price must be between target and current
            v.max(sqrt_p_target).min(sqrt_p_current)
        } else {
            return None;
        };

        // Output: dy = L * (sqrt_p_old - sqrt_p_new) / Q96
        let dy = max_token1_in_range(liquidity, new_sqrt_price, sqrt_p_current)?;
        Some((amount_in_remaining, dy, new_sqrt_price))
    }
}

/// Compute the token0 output for a given token1 input within a single tick range (oneForZero).
///
/// Given: current sqrtP, target sqrtP (upper bound), liquidity, amount_in of token1.
/// Returns (amount1_consumed, amount0_out, new_sqrt_price).
fn compute_step_one_for_zero(
    sqrt_p_current: u128,
    sqrt_p_target: u128, // upper price boundary
    liquidity: u128,
    amount_in_remaining: u128,
) -> Option<(u128, u128, u128)> {
    if liquidity == 0 || sqrt_p_current >= sqrt_p_target {
        return Some((0, 0, sqrt_p_current));
    }

    // Max token1 that can be consumed to reach sqrt_p_target
    let max_dy = max_token1_in_range(liquidity, sqrt_p_current, sqrt_p_target)?;

    if amount_in_remaining >= max_dy {
        // We consume the entire range
        let dx = max_token0_in_range(liquidity, sqrt_p_current, sqrt_p_target)?;
        Some((max_dy, dx, sqrt_p_target))
    } else {
        // Partial fill — compute new sqrtPrice
        // sqrt_P_new = sqrt_P_old + dy * Q96 / L
        // (dy pushes price up since we're adding token1)
        let delta_sp = if let Some(dy_q96) = (amount_in_remaining as u128).checked_mul(Q96) {
            dy_q96 / liquidity
        } else {
            let val = (amount_in_remaining as f64) * (Q96 as f64) / (liquidity as f64);
            if val.is_finite() && val >= 0.0 && val <= u128::MAX as f64 {
                val as u128
            } else {
                return None;
            }
        };

        let new_sqrt_price = sqrt_p_current.checked_add(delta_sp)?
            .min(sqrt_p_target)
            .max(sqrt_p_current);

        let dx = max_token0_in_range(liquidity, sqrt_p_current, new_sqrt_price)?;
        Some((amount_in_remaining, dx, new_sqrt_price))
    }
}

/// Simulate a Uniswap V3 swap with full tick traversal.
///
/// # Arguments
/// * `sqrt_price_x96` — current pool sqrtPriceX96 (Q64.96)
/// * `current_tick` — current tick index
/// * `liquidity` — current active liquidity (L)
/// * `ticks` — slice of initialized ticks sorted ascending by index, with their liquidityNet
/// * `amount_in` — gross input amount (before fees)
/// * `zero_for_one` — true if selling token0 for token1 (price decreases)
/// * `fee` — pool fee in millionths (e.g. 3000 for 0.3%)
///
/// Returns `None` if the math fails (overflow, zero inputs, etc.).
pub fn get_amount_out_with_ticks(
    sqrt_price_x96: u128,
    current_tick: i32,
    liquidity: u128,
    ticks: &[V3Tick],
    amount_in: u128,
    zero_for_one: bool,
    fee: u32,
) -> Option<V3SwapResult> {
    if sqrt_price_x96 == 0 || amount_in == 0 {
        return None;
    }

    // Apply fee to input: amount_in_after_fee = amount_in * (1_000_000 - fee) / 1_000_000
    let fee_factor = 1_000_000u128.checked_sub(fee as u128)?;
    let amount_in_after_fee = amount_in
        .checked_mul(fee_factor)?
        .checked_div(1_000_000)?;

    if amount_in_after_fee == 0 {
        return None;
    }

    let mut remaining = amount_in_after_fee;
    let mut total_out: u128 = 0;
    let mut current_sqrt_price = sqrt_price_x96;
    let mut active_liquidity = liquidity;
    let mut ticks_crossed: u32 = 0;

    if zero_for_one {
        // Price is decreasing. We need ticks below current_tick, in descending order.
        // Find all initialized ticks <= current_tick, iterate from highest to lowest.
        //
        // Uniswap V3 convention: for zeroForOne, the current tick's sqrtPrice is the
        // starting price. We swap toward lower prices. A tick is "already crossed" only
        // if the current price is STRICTLY below its sqrtPrice. When the price equals
        // a tick's sqrtPrice, we haven't crossed it yet — we need to consume liquidity
        // in the range above it first.
        let mut relevant: Vec<&V3Tick> = ticks.iter()
            .filter(|t| t.index <= current_tick)
            .collect();
        relevant.sort_by(|a, b| b.index.cmp(&a.index)); // descending

        for tick in &relevant {
            if remaining == 0 {
                break;
            }

            let tick_sqrt_price = sqrt_price_x96_at_tick(tick.index)?;

            // Strictly below this tick's price — already past it, cross immediately
            if current_sqrt_price < tick_sqrt_price {
                // When moving left (price decreasing), subtract liquidityNet
                let new_liq = (active_liquidity as i128)
                    .checked_sub(tick.liquidity_net)?;
                active_liquidity = if new_liq > 0 { new_liq as u128 } else { 0 };
                ticks_crossed = ticks_crossed.saturating_add(1);
                continue;
            }

            if active_liquidity == 0 {
                // No liquidity in this range — skip to the tick boundary
                current_sqrt_price = tick_sqrt_price;
                // Cross the tick
                let new_liq = (active_liquidity as i128)
                    .checked_sub(tick.liquidity_net)?;
                active_liquidity = if new_liq > 0 { new_liq as u128 } else { 0 };
                ticks_crossed = ticks_crossed.saturating_add(1);
                continue;
            }

            let (consumed, output, new_price) = compute_step_zero_for_one(
                current_sqrt_price,
                tick_sqrt_price,
                active_liquidity,
                remaining,
            )?;

            remaining = remaining.saturating_sub(consumed);
            total_out = total_out.saturating_add(output);
            current_sqrt_price = new_price;

            if current_sqrt_price <= tick_sqrt_price {
                // We reached the tick boundary — cross it
                let new_liq = (active_liquidity as i128)
                    .checked_sub(tick.liquidity_net)?;
                active_liquidity = if new_liq > 0 { new_liq as u128 } else { 0 };
                ticks_crossed = ticks_crossed.saturating_add(1);
            }
        }

        // If there's remaining input and still liquidity, consume what we can
        // against the next "virtual" tick at MIN_TICK price (≈ 0).
        if remaining > 0 && active_liquidity > 0 {
            let min_sqrt_price = sqrt_price_x96_at_tick(MIN_TICK).unwrap_or(1);
            let (_consumed, output, new_price) = compute_step_zero_for_one(
                current_sqrt_price,
                min_sqrt_price,
                active_liquidity,
                remaining,
            )?;
            total_out = total_out.saturating_add(output);
            current_sqrt_price = new_price;
        }
    } else {
        // oneForZero: price is increasing. We need ticks above current_tick, ascending.
        //
        // For oneForZero, a tick is "already crossed" only if current price is
        // STRICTLY above its sqrtPrice. When equal, we haven't reached it yet.
        let mut relevant: Vec<&V3Tick> = ticks.iter()
            .filter(|t| t.index > current_tick)
            .collect();
        relevant.sort_by(|a, b| a.index.cmp(&b.index)); // ascending

        for tick in &relevant {
            if remaining == 0 {
                break;
            }

            let tick_sqrt_price = sqrt_price_x96_at_tick(tick.index)?;

            // Strictly above this tick's price — already past it, cross immediately
            if current_sqrt_price > tick_sqrt_price {
                // When moving right (price increasing), add liquidityNet
                let new_liq = (active_liquidity as i128)
                    .checked_add(tick.liquidity_net)?;
                active_liquidity = if new_liq > 0 { new_liq as u128 } else { 0 };
                ticks_crossed = ticks_crossed.saturating_add(1);
                continue;
            }

            if active_liquidity == 0 {
                current_sqrt_price = tick_sqrt_price;
                let new_liq = (active_liquidity as i128)
                    .checked_add(tick.liquidity_net)?;
                active_liquidity = if new_liq > 0 { new_liq as u128 } else { 0 };
                ticks_crossed = ticks_crossed.saturating_add(1);
                continue;
            }

            let (consumed, output, new_price) = compute_step_one_for_zero(
                current_sqrt_price,
                tick_sqrt_price,
                active_liquidity,
                remaining,
            )?;

            remaining = remaining.saturating_sub(consumed);
            total_out = total_out.saturating_add(output);
            current_sqrt_price = new_price;

            if current_sqrt_price >= tick_sqrt_price {
                // Cross tick
                let new_liq = (active_liquidity as i128)
                    .checked_add(tick.liquidity_net)?;
                active_liquidity = if new_liq > 0 { new_liq as u128 } else { 0 };
                ticks_crossed = ticks_crossed.saturating_add(1);
            }
        }

        // Remaining input against unbounded upper range
        if remaining > 0 && active_liquidity > 0 {
            // Use a very high sqrt price as upper bound (tick ~887272, the max)
            let max_sqrt_price = sqrt_price_x96_at_tick(887272).unwrap_or(u128::MAX);
            let (consumed, output, new_price) = compute_step_one_for_zero(
                current_sqrt_price,
                max_sqrt_price,
                active_liquidity,
                remaining,
            )?;
            let _ = consumed;
            total_out = total_out.saturating_add(output);
            current_sqrt_price = new_price;
        }
    }

    Some(V3SwapResult {
        amount_out: total_out,
        sqrt_price_x96_after: current_sqrt_price,
        ticks_crossed,
    })
}

/// Convenience wrapper: simulate a V3 swap using a pool struct.
///
/// If the pool has tick data, uses full tick-traversal simulation.
/// Otherwise falls back to the simplified constant-price approximation.
///
/// Returns `(amount_out, ticks_crossed)`.
pub fn get_amount_out(pool: &UniswapV3Pool, amount_in: u128, zero_for_one: bool) -> Option<(u128, u32)> {
    let sqrt_price: u128 = pool.sqrt_price_x96.parse().ok()?;
    let liquidity: u128 = pool.liquidity.parse().ok()?;

    if let Some(ref ticks) = pool.ticks {
        if !ticks.is_empty() {
            let result = get_amount_out_with_ticks(
                sqrt_price,
                pool.tick,
                liquidity,
                ticks,
                amount_in,
                zero_for_one,
                pool.fee,
            )?;
            return Some((result.amount_out, result.ticks_crossed));
        }
    }

    // Fallback to simplified approximation (no tick data)
    let out = get_amount_out_approx(sqrt_price, liquidity, amount_in, zero_for_one, pool.fee)?;
    Some((out, 0))
}

// ── Gas estimation with tick awareness ──────────────────────────────────────

/// Estimate gas for a V3 swap given the number of ticks actually crossed.
///
/// Uses the constants from `crate::gas`:
///   base = 130_000 + ticks_crossed * 30_000
pub fn estimate_v3_gas_with_ticks(ticks_crossed: u32) -> u64 {
    crate::gas::GAS_UNISWAP_V3_SWAP_BASE
        + (ticks_crossed as u64) * crate::gas::GAS_UNISWAP_V3_PER_TICK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::{UniswapV3Pool, V3Tick};

    fn make_pool(sqrt_price_x96: u128, liquidity: u128) -> UniswapV3Pool {
        UniswapV3Pool {
            address: "0x0000".into(),
            token0: "0xtoken0".into(),
            token1: "0xtoken1".into(),
            sqrt_price_x96: sqrt_price_x96.to_string(),
            tick: 0,
            liquidity: liquidity.to_string(),
            fee: 3_000,
            ticks: None,
        }
    }

    fn make_pool_with_ticks(
        sqrt_price_x96: u128,
        tick: i32,
        liquidity: u128,
        ticks: Vec<V3Tick>,
    ) -> UniswapV3Pool {
        UniswapV3Pool {
            address: "0x0000".into(),
            token0: "0xtoken0".into(),
            token1: "0xtoken1".into(),
            sqrt_price_x96: sqrt_price_x96.to_string(),
            tick,
            liquidity: liquidity.to_string(),
            fee: 3_000,
            ticks: Some(ticks),
        }
    }

    #[test]
    fn spot_price_one_to_one() {
        // sqrtPriceX96 = 2^96 → price = 1.0
        let q96 = 1u128 << 96;
        let pool = make_pool(q96, 1_000_000);
        let price = spot_price(&pool).unwrap();
        assert!((price - 1.0).abs() < 1e-6);
    }

    #[test]
    fn spot_price_zero_returns_none() {
        let pool = make_pool(0, 1_000_000);
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
    fn decode_address_zero_returns_none() {
        let hex = "0x0000000000000000000000000000000000000000000000000000000000000000";
        assert!(decode_address(hex).is_none());
    }

    #[test]
    fn decode_address_ok() {
        let hex = "0x000000000000000000000000abcd000000000000000000000000000000001234";
        let addr = decode_address(hex).unwrap();
        assert_eq!(addr, "0xabcd000000000000000000000000000000001234");
    }

    #[test]
    fn decode_liquidity_ok() {
        let hex = format!("0x{:064x}", 999_999_999u128);
        let liq = decode_liquidity(&hex).unwrap();
        assert_eq!(liq, 999_999_999);
    }

    #[test]
    fn encode_get_pool_length() {
        let data = encode_get_pool(
            "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            3_000,
        );
        // 0x + 8 (selector) + 64 (addr1) + 64 (addr2) + 64 (fee) = 202 chars
        assert_eq!(data.len(), 2 + 8 + 64 + 64 + 64);
    }

    #[test]
    fn decode_slot0_tick_positive() {
        // tick = 1000 decimal = 0x3e8
        let sqrt_price_hex = format!("{:064x}", 1u128 << 96);
        let tick_hex = format!("{:064x}", 1000u32);
        let hex = format!("0x{sqrt_price_hex}{tick_hex}");
        let (_, tick) = decode_slot0(&hex).unwrap();
        assert_eq!(tick, 1000);
    }

    #[test]
    fn decode_slot0_tick_negative() {
        // tick = -1 encoded as int24: 0xFFFFFF → after sign extension = -1
        let sqrt_price_hex = format!("{:064x}", 1u128 << 96);
        let tick_hex = format!("{:064x}", 0xFFFFFFu32);
        let hex = format!("0x{sqrt_price_hex}{tick_hex}");
        let (_, tick) = decode_slot0(&hex).unwrap();
        assert_eq!(tick, -1);
    }

    // ── Tick-traversal swap tests ───────────────────────────────────────────

    #[test]
    fn sqrt_price_at_tick_zero_is_q96() {
        let sp = sqrt_price_x96_at_tick(0).unwrap();
        let q96 = 1u128 << 96;
        // Should be very close to 2^96
        let ratio = sp as f64 / q96 as f64;
        assert!(
            (ratio - 1.0).abs() < 1e-6,
            "sqrt_price at tick 0 should be ~2^96, got ratio {ratio}"
        );
    }

    #[test]
    fn sqrt_price_at_tick_increases_with_tick() {
        let sp_neg = sqrt_price_x96_at_tick(-1000).unwrap();
        let sp_zero = sqrt_price_x96_at_tick(0).unwrap();
        let sp_pos = sqrt_price_x96_at_tick(1000).unwrap();
        assert!(sp_neg < sp_zero, "negative tick should have lower sqrtPrice");
        assert!(sp_zero < sp_pos, "positive tick should have higher sqrtPrice");
    }

    #[test]
    fn small_swap_within_single_tick_zero_for_one() {
        // Pool at price = 1.0 (sqrtPrice = 2^96), tick 0, large liquidity
        // Single tick range from tick -60 to tick 60 (tickSpacing=60 for 0.3% fee)
        let q96 = 1u128 << 96;
        let liquidity = 10_000_000_000_000u128; // 10T units of liquidity
        let ticks = vec![
            V3Tick { index: -60, liquidity_net: liquidity as i128 },
            V3Tick { index: 60, liquidity_net: -(liquidity as i128) },
        ];

        // Small swap: 1000 token0 in, should get ~997 token1 out (1:1 price, 0.3% fee)
        let result = get_amount_out_with_ticks(
            q96, 0, liquidity, &ticks, 1_000, true, 3_000,
        ).unwrap();

        // With 0.3% fee, output should be ≈997 (for price=1 and small swap)
        assert!(result.amount_out > 0, "should produce output");
        assert!(result.amount_out <= 997, "fee should reduce output below 997, got {}", result.amount_out);
        assert!(result.amount_out >= 990, "output should be close to 997, got {}", result.amount_out);
        assert_eq!(result.ticks_crossed, 0, "small swap should not cross any ticks");
    }

    #[test]
    fn small_swap_within_single_tick_one_for_zero() {
        let q96 = 1u128 << 96;
        let liquidity = 10_000_000_000_000u128;
        let ticks = vec![
            V3Tick { index: -60, liquidity_net: liquidity as i128 },
            V3Tick { index: 60, liquidity_net: -(liquidity as i128) },
        ];

        let result = get_amount_out_with_ticks(
            q96, 0, liquidity, &ticks, 1_000, false, 3_000,
        ).unwrap();

        assert!(result.amount_out > 0, "should produce output");
        assert!(result.amount_out <= 997, "fee should reduce output");
        assert!(result.amount_out >= 990, "output should be close to 997, got {}", result.amount_out);
        assert_eq!(result.ticks_crossed, 0, "small swap should not cross ticks");
    }

    #[test]
    fn large_swap_crosses_multiple_ticks_zero_for_one() {
        // Set up a pool with multiple tick ranges, each with moderate liquidity.
        // This forces a large swap to cross several ticks.
        let q96 = 1u128 << 96;
        let liq_per_range = 1_000_000_000u128; // 1B liquidity per range

        // 5 tick ranges: [-300,-240], [-240,-180], [-180,-120], [-120,-60], [-60,0]
        // Current tick = 0, current price = 1.0
        // Each range boundary adds/removes liquidity.
        let ticks = vec![
            V3Tick { index: -300, liquidity_net: liq_per_range as i128 },
            V3Tick { index: -240, liquidity_net: liq_per_range as i128 },
            V3Tick { index: -180, liquidity_net: liq_per_range as i128 },
            V3Tick { index: -120, liquidity_net: liq_per_range as i128 },
            V3Tick { index: -60, liquidity_net: liq_per_range as i128 },
            V3Tick { index: 0, liquidity_net: -(5 * liq_per_range as i128) }, // all liquidity exits at tick 0
        ];

        // Active liquidity at tick 0 = 5 * liq_per_range (all 5 ranges are active)
        let active_liq = 5 * liq_per_range;

        // Large swap: 10B token0 in — should cross multiple ticks downward
        let amount_in = 10_000_000_000u128;
        let result = get_amount_out_with_ticks(
            q96, 0, active_liq, &ticks, amount_in, true, 3_000,
        ).unwrap();

        assert!(result.amount_out > 0, "should produce output for large swap");
        assert!(
            result.ticks_crossed >= 3,
            "large swap should cross 3+ ticks, only crossed {}",
            result.ticks_crossed
        );
        // Output should be less than input * 0.997 due to price impact from crossing ticks
        let max_no_impact = amount_in * 997 / 1000;
        assert!(
            result.amount_out < max_no_impact,
            "output {} should be less than no-impact output {} due to slippage",
            result.amount_out,
            max_no_impact
        );
    }

    #[test]
    fn large_swap_crosses_multiple_ticks_one_for_zero() {
        let q96 = 1u128 << 96;
        let liq_per_range = 1_000_000_000u128;

        // Tick ranges above current price
        let ticks = vec![
            V3Tick { index: 0, liquidity_net: 5 * liq_per_range as i128 },
            V3Tick { index: 60, liquidity_net: -(liq_per_range as i128) },
            V3Tick { index: 120, liquidity_net: -(liq_per_range as i128) },
            V3Tick { index: 180, liquidity_net: -(liq_per_range as i128) },
            V3Tick { index: 240, liquidity_net: -(liq_per_range as i128) },
            V3Tick { index: 300, liquidity_net: -(liq_per_range as i128) },
        ];

        let active_liq = 5 * liq_per_range;
        let amount_in = 10_000_000_000u128;

        let result = get_amount_out_with_ticks(
            q96, 0, active_liq, &ticks, amount_in, false, 3_000,
        ).unwrap();

        assert!(result.amount_out > 0, "should produce output");
        assert!(
            result.ticks_crossed >= 3,
            "large one_for_zero swap should cross 3+ ticks, crossed {}",
            result.ticks_crossed
        );
    }

    #[test]
    fn swap_exhausts_all_liquidity() {
        // Pool with very small liquidity — even a moderate swap exhausts everything
        let q96 = 1u128 << 96;
        let liquidity = 1_000u128; // tiny liquidity

        let ticks = vec![
            V3Tick { index: -60, liquidity_net: liquidity as i128 },
            V3Tick { index: 60, liquidity_net: -(liquidity as i128) },
        ];

        // Large swap that should exhaust all liquidity
        let amount_in = 1_000_000_000_000u128; // 1T tokens
        let result = get_amount_out_with_ticks(
            q96, 0, liquidity, &ticks, amount_in, true, 3_000,
        ).unwrap();

        // Output should be much less than input (liquidity exhausted)
        assert!(
            result.amount_out < amount_in / 1000,
            "with tiny liquidity, output should be very small relative to input"
        );
    }

    #[test]
    fn get_amount_out_wrapper_uses_ticks_when_present() {
        let q96 = 1u128 << 96;
        let liquidity = 10_000_000_000_000u128;
        let pool = make_pool_with_ticks(
            q96,
            0,
            liquidity,
            vec![
                V3Tick { index: -60, liquidity_net: liquidity as i128 },
                V3Tick { index: 60, liquidity_net: -(liquidity as i128) },
            ],
        );

        let (out, ticks_crossed) = get_amount_out(&pool, 1_000, true).unwrap();
        assert!(out > 0, "wrapper should produce output with tick data");
        assert_eq!(ticks_crossed, 0, "small swap should cross 0 ticks");
    }

    #[test]
    fn get_amount_out_wrapper_falls_back_without_ticks() {
        let q96 = 1u128 << 96;
        let pool = make_pool(q96, 1_000_000);

        let (out, ticks_crossed) = get_amount_out(&pool, 1_000, true).unwrap();
        assert!(out > 0, "fallback should produce output");
        assert_eq!(ticks_crossed, 0, "fallback always reports 0 ticks crossed");
    }

    #[test]
    fn get_amount_out_with_ticks_returns_none_for_zero_input() {
        let q96 = 1u128 << 96;
        let ticks = vec![V3Tick { index: 0, liquidity_net: 1000 }];
        assert!(get_amount_out_with_ticks(q96, 0, 1000, &ticks, 0, true, 3000).is_none());
    }

    #[test]
    fn get_amount_out_with_ticks_returns_none_for_zero_sqrt_price() {
        let ticks = vec![V3Tick { index: 0, liquidity_net: 1000 }];
        assert!(get_amount_out_with_ticks(0, 0, 1000, &ticks, 1000, true, 3000).is_none());
    }

    #[test]
    fn estimate_v3_gas_scales_with_ticks() {
        let gas_0 = estimate_v3_gas_with_ticks(0);
        let gas_1 = estimate_v3_gas_with_ticks(1);
        let gas_5 = estimate_v3_gas_with_ticks(5);

        assert_eq!(gas_0, crate::gas::GAS_UNISWAP_V3_SWAP_BASE);
        assert_eq!(gas_1, crate::gas::GAS_UNISWAP_V3_SWAP_BASE + crate::gas::GAS_UNISWAP_V3_PER_TICK);
        assert_eq!(gas_5, crate::gas::GAS_UNISWAP_V3_SWAP_BASE + 5 * crate::gas::GAS_UNISWAP_V3_PER_TICK);
        assert!(gas_5 > gas_1, "more ticks should cost more gas");
    }

    #[test]
    fn tick_traversal_gives_less_output_than_approx_for_large_swap() {
        // The simplified approx ignores price impact, so for large swaps it
        // overestimates output. Tick traversal should give less.
        let q96 = 1u128 << 96;
        let liquidity = 1_000_000_000u128;

        // Large input relative to liquidity
        let amount_in = 500_000_000u128;

        let approx = get_amount_out_approx(q96, liquidity, amount_in, true, 3_000).unwrap();

        let ticks = vec![
            V3Tick { index: -600, liquidity_net: liquidity as i128 },
            V3Tick { index: 0, liquidity_net: -(liquidity as i128) },
        ];

        // Use a tick range that the current price is clearly inside (not on boundary)
        // to avoid boundary edge cases. Current tick -30 is between -600 and 0.
        let current_tick = -30;
        let current_sqrt_price = sqrt_price_x96_at_tick(current_tick).unwrap();

        let ticks = vec![
            V3Tick { index: -600, liquidity_net: liquidity as i128 },
            V3Tick { index: 0, liquidity_net: -(liquidity as i128) },
        ];

        let approx_for_this = get_amount_out_approx(current_sqrt_price, liquidity, amount_in, true, 3_000).unwrap();

        let result = get_amount_out_with_ticks(
            current_sqrt_price, current_tick, liquidity, &ticks, amount_in, true, 3_000,
        ).expect("tick traversal should produce a result");

        assert!(
            result.amount_out <= approx_for_this,
            "tick traversal ({}) should give <= approx ({}) due to price impact",
            result.amount_out,
            approx_for_this
        );
    }

    #[test]
    fn symmetry_small_swap_both_directions() {
        // For price=1 (sqrtPrice=2^96), a small swap of X token0→token1
        // and X token1→token0 should give approximately the same output.
        let q96 = 1u128 << 96;
        let liquidity = 10_000_000_000_000u128;
        let ticks = vec![
            V3Tick { index: -600, liquidity_net: liquidity as i128 },
            V3Tick { index: 600, liquidity_net: -(liquidity as i128) },
        ];

        let amount = 10_000u128;

        let r0 = get_amount_out_with_ticks(q96, 0, liquidity, &ticks, amount, true, 3_000).unwrap();
        let r1 = get_amount_out_with_ticks(q96, 0, liquidity, &ticks, amount, false, 3_000).unwrap();

        let diff = (r0.amount_out as i128 - r1.amount_out as i128).unsigned_abs();
        let avg = (r0.amount_out + r1.amount_out) / 2;
        // Allow 1% deviation due to rounding in different directions
        assert!(
            diff <= avg / 100 + 1,
            "small swap should be ~symmetric at price=1: zero_for_one={}, one_for_zero={}",
            r0.amount_out,
            r1.amount_out
        );
    }

    // ── Tick bitmap helper tests ────────────────────────────────────────────

    #[test]
    fn tick_spacing_for_fee_tiers() {
        assert_eq!(tick_spacing(100), 1);
        assert_eq!(tick_spacing(500), 10);
        assert_eq!(tick_spacing(3_000), 60);
        assert_eq!(tick_spacing(10_000), 200);
    }

    #[test]
    fn tick_bitmap_position_positive_tick() {
        // Tick 120 with spacing 60 → compressed = 2
        // word_pos = 2 >> 8 = 0, bit_pos = 2 & 0xFF = 2
        let (wp, bp) = tick_bitmap_position(120, 60);
        assert_eq!(wp, 0);
        assert_eq!(bp, 2);
    }

    #[test]
    fn tick_bitmap_position_negative_tick() {
        // Tick -120 with spacing 60 → compressed = -2
        // In two's complement: -2 >> 8 = -1, -2 & 0xFF = 254
        let (wp, bp) = tick_bitmap_position(-120, 60);
        assert_eq!(wp, -1);
        assert_eq!(bp, 254);
    }

    #[test]
    fn tick_bitmap_position_zero() {
        let (wp, bp) = tick_bitmap_position(0, 60);
        assert_eq!(wp, 0);
        assert_eq!(bp, 0);
    }

    #[test]
    fn tick_bitmap_position_negative_not_multiple() {
        // Tick -1 with spacing 60: not a multiple, rounds down to -1
        // compressed = -1/60 - 1 = -1  (since -1 % 60 != 0)
        let (wp, bp) = tick_bitmap_position(-1, 60);
        assert_eq!(wp, -1);
        assert_eq!(bp, 255);
    }

    #[test]
    fn bitmap_roundtrip() {
        // Verify that tick_from_bitmap_pos inverts tick_bitmap_position
        // for ticks that are multiples of spacing
        let spacing = 60;
        for tick in [-3600, -600, -60, 0, 60, 600, 3600] {
            let (wp, bp) = tick_bitmap_position(tick, spacing);
            let reconstructed = tick_from_bitmap_pos(wp, bp, spacing);
            assert_eq!(
                reconstructed, tick,
                "roundtrip failed for tick {tick}: wp={wp}, bp={bp}, got {reconstructed}"
            );
        }
    }

    #[test]
    fn is_bit_set_checks() {
        let mut word = [0u8; 32];
        // Set bit 0 (least significant bit of last byte)
        word[31] = 0x01;
        assert!(is_bit_set(&word, 0));
        assert!(!is_bit_set(&word, 1));

        // Set bit 8 (least significant bit of second-to-last byte)
        word[30] = 0x01;
        assert!(is_bit_set(&word, 8));

        // Set bit 255 (most significant bit of first byte)
        word[0] = 0x80;
        assert!(is_bit_set(&word, 255));
    }

    #[test]
    fn next_initialized_tick_within_word_lte_finds_correct_bit() {
        let mut word = [0u8; 32];
        // Set bits at positions 5, 10, 20
        word[31] = 0x20; // bit 5
        word[30] = 0x04; // bit 10
        word[29] = 0x10; // bit 20

        // Looking from bit 25 downward, should find bit 20
        assert_eq!(next_initialized_tick_within_word_lte(&word, 25), Some(20));
        // Looking from bit 10, should find bit 10 itself
        assert_eq!(next_initialized_tick_within_word_lte(&word, 10), Some(10));
        // Looking from bit 4, should find nothing
        assert_eq!(next_initialized_tick_within_word_lte(&word, 4), None);
    }

    #[test]
    fn next_initialized_tick_within_word_gte_finds_correct_bit() {
        let mut word = [0u8; 32];
        // Set bits at positions 5, 10, 20
        word[31] = 0x20; // bit 5
        word[30] = 0x04; // bit 10
        word[29] = 0x10; // bit 20

        // Looking from bit 0 upward, should find bit 5
        assert_eq!(next_initialized_tick_within_word_gte(&word, 0), Some(5));
        // Looking from bit 6 upward, should find bit 10
        assert_eq!(next_initialized_tick_within_word_gte(&word, 6), Some(10));
        // Looking from bit 21 upward, should find nothing
        assert_eq!(next_initialized_tick_within_word_gte(&word, 21), None);
    }

    #[test]
    fn decode_tick_info_positive_liquidity_net() {
        // liquidityGross = 1000 (word 0), liquidityNet = 500 (word 1)
        let word0 = format!("{:064x}", 1000u128);
        let word1 = format!("{:064x}", 500u128);
        // Pad remaining words
        let hex = format!("0x{word0}{word1}{}", "0".repeat(64 * 6));
        let net = decode_tick_info(&hex).unwrap();
        assert_eq!(net, 500);
    }

    #[test]
    fn decode_tick_info_negative_liquidity_net() {
        // liquidityNet = -500 in two's complement u128
        let word0 = format!("{:064x}", 1000u128);
        let neg_500_u128 = (-500i128) as u128;
        let word1 = format!("{:064x}", neg_500_u128);
        let hex = format!("0x{word0}{word1}{}", "0".repeat(64 * 6));
        let net = decode_tick_info(&hex).unwrap();
        assert_eq!(net, -500);
    }

    #[test]
    fn encode_tick_bitmap_positive() {
        let data = encode_tick_bitmap(0);
        // Should be 0x + 8 selector + 64 hex = 74 chars
        assert_eq!(data.len(), 2 + 8 + 64);
        assert!(data.starts_with("0x5339c296"));
    }

    #[test]
    fn encode_tick_bitmap_negative() {
        let data = encode_tick_bitmap(-1);
        assert_eq!(data.len(), 2 + 8 + 64);
        assert!(data.starts_with("0x5339c296"));
        // -1 in two's complement should have trailing ffff
        assert!(data.ends_with("ffff"));
    }

    #[test]
    fn encode_ticks_call_positive() {
        let data = encode_ticks_call(60);
        assert_eq!(data.len(), 2 + 8 + 64);
        assert!(data.starts_with("0xf30dba93"));
        assert!(data.ends_with("3c")); // 60 = 0x3c
    }

    #[test]
    fn encode_ticks_call_negative() {
        let data = encode_ticks_call(-60);
        assert_eq!(data.len(), 2 + 8 + 64);
        assert!(data.starts_with("0xf30dba93"));
        // -60 in int24 two's complement: 0xFFFFC4, sign-extended
        assert!(data.ends_with("ffffc4"));
    }
}
