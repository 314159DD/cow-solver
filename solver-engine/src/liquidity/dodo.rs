use shared::rpc::EthClient;
use tracing::debug;

use crate::models::liquidity::DodoPool;

// ── Factory / Pool addresses (Arbitrum) ──────────────────────────────────────

/// DODO DSP (Stable Pool) factory on Arbitrum
pub const DODO_DSP_FACTORY: &str = "0x5a0C840a7089aa222c4458b0792Ef188E01339c4";

// ── ABI selectors ─────────────────────────────────────────────────────────────

/// `_BASE_TOKEN_()` → 4-byte selector
const BASE_TOKEN_SELECTOR: &str = "4a248d2a";

/// `_QUOTE_TOKEN_()` → 4-byte selector
const QUOTE_TOKEN_SELECTOR: &str = "d4b97046";

/// `_I_()` → 4-byte selector (oracle price)
const I_SELECTOR: &str = "d909cae3";

/// `_K_()` → 4-byte selector (slippage factor)
const K_SELECTOR: &str = "89bb3e4c";

/// `_BASE_RESERVE_()` → 4-byte selector
const BASE_RESERVE_SELECTOR: &str = "7d721504";

/// `_QUOTE_RESERVE_()` → 4-byte selector
const QUOTE_RESERVE_SELECTOR: &str = "0e792b5e";

/// `_BASE_TARGET_()` → 4-byte selector
const BASE_TARGET_SELECTOR: &str = "e539ef01";

/// `_QUOTE_TARGET_()` → 4-byte selector
const QUOTE_TARGET_SELECTOR: &str = "f5b91b7b";

/// `_LP_FEE_RATE_()` → 4-byte selector
const LP_FEE_RATE_SELECTOR: &str = "5765a5cc";

/// `decimals()` on ERC-20 → 4-byte selector
const DECIMALS_SELECTOR: &str = "313ce567";

// ── ABI encoding helpers ──────────────────────────────────────────────────────

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

/// Decode a uint256 from a single 32-byte hex word into a u128.
fn decode_uint256(hex: &str) -> Option<u128> {
    let clean = hex.trim_start_matches("0x");
    if clean.is_empty() {
        return None;
    }
    // Try parsing the full value — if > u128, return None (shouldn't happen for these params)
    u128::from_str_radix(clean.trim_start_matches('0'), 16)
        .ok()
        .or_else(|| {
            if clean.chars().all(|c| c == '0') {
                Some(0)
            } else {
                None
            }
        })
}

/// Decode a uint8 (decimals) from a 32-byte hex word.
fn decode_uint8(hex: &str) -> Option<u8> {
    decode_uint256(hex).map(|v| v as u8)
}

// ── Pool discovery ───────────────────────────────────────────────────────────

/// Fetch a DODO pool's full state from on-chain data.
///
/// `pool_address` must be a known DODO pool (DSP or DPP).
/// Unlike factory-based discovery (Uniswap), DODO pools are typically
/// discovered via subgraph or config. This function reads the pool state.
pub async fn fetch_pool(
    rpc: &EthClient,
    pool_address: &str,
    pool_type: &str,
) -> anyhow::Result<Option<DodoPool>> {
    // Fetch all pool parameters in parallel
    let base_call = format!("0x{BASE_TOKEN_SELECTOR}");
    let quote_call = format!("0x{QUOTE_TOKEN_SELECTOR}");
    let i_call = format!("0x{I_SELECTOR}");
    let k_call = format!("0x{K_SELECTOR}");
    let base_reserve_call = format!("0x{BASE_RESERVE_SELECTOR}");
    let quote_reserve_call = format!("0x{QUOTE_RESERVE_SELECTOR}");
    let base_target_call = format!("0x{BASE_TARGET_SELECTOR}");
    let quote_target_call = format!("0x{QUOTE_TARGET_SELECTOR}");
    let lp_fee_call = format!("0x{LP_FEE_RATE_SELECTOR}");
    let (
        base_hex,
        quote_hex,
        i_hex,
        k_hex,
        base_reserve_hex,
        quote_reserve_hex,
        base_target_hex,
        quote_target_hex,
        lp_fee_hex,
    ) = tokio::try_join!(
        rpc.call(pool_address, &base_call),
        rpc.call(pool_address, &quote_call),
        rpc.call(pool_address, &i_call),
        rpc.call(pool_address, &k_call),
        rpc.call(pool_address, &base_reserve_call),
        rpc.call(pool_address, &quote_reserve_call),
        rpc.call(pool_address, &base_target_call),
        rpc.call(pool_address, &quote_target_call),
        rpc.call(pool_address, &lp_fee_call),
    )?;

    let base_token = match decode_address(&base_hex) {
        Some(addr) => addr,
        None => return Ok(None),
    };
    let quote_token = match decode_address(&quote_hex) {
        Some(addr) => addr,
        None => return Ok(None),
    };

    let i = decode_uint256(&i_hex).unwrap_or(0);
    let k = decode_uint256(&k_hex).unwrap_or(0);
    let base_reserve = decode_uint256(&base_reserve_hex).unwrap_or(0);
    let quote_reserve = decode_uint256(&quote_reserve_hex).unwrap_or(0);
    let base_target = decode_uint256(&base_target_hex).unwrap_or(0);
    let quote_target = decode_uint256(&quote_target_hex).unwrap_or(0);
    let lp_fee_rate = decode_uint256(&lp_fee_hex).unwrap_or(0);

    // Fetch decimals for both tokens
    let dec_call = format!("0x{DECIMALS_SELECTOR}");
    let (base_dec_hex, quote_dec_hex) = tokio::try_join!(
        rpc.call(&base_token, &dec_call),
        rpc.call(&quote_token, &dec_call),
    )?;
    let base_decimals = decode_uint8(&base_dec_hex).unwrap_or(18);
    let quote_decimals = decode_uint8(&quote_dec_hex).unwrap_or(18);

    debug!(
        address = pool_address,
        base = %base_token,
        quote = %quote_token,
        i = i,
        k = k,
        base_reserve = base_reserve,
        quote_reserve = quote_reserve,
        "Fetched DODO pool"
    );

    Ok(Some(DodoPool {
        address: pool_address.to_string(),
        base_token,
        quote_token,
        i: i.to_string(),
        k: k.to_string(),
        base_reserve: base_reserve.to_string(),
        quote_reserve: quote_reserve.to_string(),
        base_target: base_target.to_string(),
        quote_target: quote_target.to_string(),
        base_decimals,
        quote_decimals,
        lp_fee_rate: lp_fee_rate.to_string(),
        pool_type: pool_type.to_string(),
    }))
}

/// Refresh DODO pool reserves and targets in-place.
pub async fn sync_pool(rpc: &EthClient, pool: &mut DodoPool) -> anyhow::Result<()> {
    let br_call = format!("0x{BASE_RESERVE_SELECTOR}");
    let qr_call = format!("0x{QUOTE_RESERVE_SELECTOR}");
    let bt_call = format!("0x{BASE_TARGET_SELECTOR}");
    let qt_call = format!("0x{QUOTE_TARGET_SELECTOR}");
    let i_call = format!("0x{I_SELECTOR}");
    let (base_reserve_hex, quote_reserve_hex, base_target_hex, quote_target_hex, i_hex) =
        tokio::try_join!(
            rpc.call(&pool.address, &br_call),
            rpc.call(&pool.address, &qr_call),
            rpc.call(&pool.address, &bt_call),
            rpc.call(&pool.address, &qt_call),
            rpc.call(&pool.address, &i_call),
        )?;

    if let Some(v) = decode_uint256(&base_reserve_hex) {
        pool.base_reserve = v.to_string();
    }
    if let Some(v) = decode_uint256(&quote_reserve_hex) {
        pool.quote_reserve = v.to_string();
    }
    if let Some(v) = decode_uint256(&base_target_hex) {
        pool.base_target = v.to_string();
    }
    if let Some(v) = decode_uint256(&quote_target_hex) {
        pool.quote_target = v.to_string();
    }
    if let Some(v) = decode_uint256(&i_hex) {
        pool.i = v.to_string();
    }

    debug!(
        address = %pool.address,
        base_reserve = %pool.base_reserve,
        quote_reserve = %pool.quote_reserve,
        "Synced DODO pool"
    );
    Ok(())
}

// ── PMM Math ─────────────────────────────────────────────────────────────────

/// 1e18 constant for fixed-point math
const ONE: f64 = 1e18;

/// DODO PMM: compute output for selling base tokens (base → quote).
///
/// PMM formula for sell base:
///   price = i * (1 - k + k * (B0/B)^2)
///   where B is new base amount after adding input
///
/// The integral from B to B+amount gives the quote output.
pub fn get_amount_out_sell_base(pool: &DodoPool, amount_in: u128) -> Option<u128> {
    let i = pool.i.parse::<f64>().ok()? / ONE;
    let k = pool.k.parse::<f64>().ok()? / ONE;
    let b = pool.base_reserve.parse::<f64>().ok()?;
    let b0 = pool.base_target.parse::<f64>().ok()?;
    let fee_rate = pool.lp_fee_rate.parse::<f64>().ok()? / ONE;

    if b <= 0.0 || b0 <= 0.0 || amount_in == 0 {
        return None;
    }

    let amount_in_f = amount_in as f64;

    // Integrate the PMM curve: output = integral of marginal price from B to B+amount_in
    // For sell base: we're adding base, so B increases, and we compute quote output
    let b_new = b + amount_in_f;

    // Output = i * amount_in * (1 - k + k * B0^2 / (B * B_new))
    let output = if k < 1e-12 {
        // k ≈ 0: constant price pool
        i * amount_in_f
    } else {
        // PMM integral: ΔQ = i * (1-k) * Δx + i * k * B0² * (1/B - 1/B_new)
        let term1 = i * (1.0 - k) * amount_in_f;
        let term2 = i * k * b0 * b0 * (1.0 / b - 1.0 / b_new);
        term1 + term2
    };

    if output <= 0.0 {
        return None;
    }

    // Apply fee
    let output_after_fee = output * (1.0 - fee_rate);
    if output_after_fee <= 0.0 {
        return None;
    }

    Some(output_after_fee as u128)
}

/// DODO PMM: compute output for selling quote tokens (quote → base).
///
/// Symmetric to sell_base but operates on the quote side.
pub fn get_amount_out_sell_quote(pool: &DodoPool, amount_in: u128) -> Option<u128> {
    let i = pool.i.parse::<f64>().ok()? / ONE;
    let k = pool.k.parse::<f64>().ok()? / ONE;
    let q = pool.quote_reserve.parse::<f64>().ok()?;
    let q0 = pool.quote_target.parse::<f64>().ok()?;
    let fee_rate = pool.lp_fee_rate.parse::<f64>().ok()? / ONE;

    if q <= 0.0 || q0 <= 0.0 || i <= 0.0 || amount_in == 0 {
        return None;
    }

    let amount_in_f = amount_in as f64;
    let q_new = q + amount_in_f;

    // For sell quote: we're adding quote, Q increases, compute base output
    // ΔB = (1/i) * (1-k) * ΔQ + (1/i) * k * Q0² * (1/Q - 1/Q_new)
    let inv_i = 1.0 / i;
    let output = if k < 1e-12 {
        inv_i * amount_in_f
    } else {
        let term1 = inv_i * (1.0 - k) * amount_in_f;
        let term2 = inv_i * k * q0 * q0 * (1.0 / q - 1.0 / q_new);
        term1 + term2
    };

    if output <= 0.0 {
        return None;
    }

    let output_after_fee = output * (1.0 - fee_rate);
    if output_after_fee <= 0.0 {
        return None;
    }

    Some(output_after_fee as u128)
}

/// Unified swap interface: compute output amount for a DODO pool swap.
///
/// `sell_base`: true if selling base token for quote, false if selling quote for base.
pub fn get_amount_out(pool: &DodoPool, amount_in: u128, sell_base: bool) -> Option<u128> {
    if sell_base {
        get_amount_out_sell_base(pool, amount_in)
    } else {
        get_amount_out_sell_quote(pool, amount_in)
    }
}

/// Returns the spot price of base denominated in quote (i.e. how many quote per 1 base).
pub fn spot_price(pool: &DodoPool) -> Option<f64> {
    let i = pool.i.parse::<f64>().ok()? / ONE;
    let k = pool.k.parse::<f64>().ok()? / ONE;
    let b = pool.base_reserve.parse::<f64>().ok()?;
    let b0 = pool.base_target.parse::<f64>().ok()?;

    if b <= 0.0 || b0 <= 0.0 {
        return None;
    }

    // PMM mid price: i * (1 - k + k * (B0/B)^2)
    let ratio = b0 / b;
    Some(i * (1.0 - k + k * ratio * ratio))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::DodoPool;

    fn make_dodo_pool(
        base_reserve: u128,
        quote_reserve: u128,
        base_target: u128,
        quote_target: u128,
        i: u128,
        k: u128,
        fee_rate: u128,
    ) -> DodoPool {
        DodoPool {
            address: "0x0000".into(),
            base_token: "0xbase".into(),
            quote_token: "0xquote".into(),
            i: i.to_string(),
            k: k.to_string(),
            base_reserve: base_reserve.to_string(),
            quote_reserve: quote_reserve.to_string(),
            base_target: base_target.to_string(),
            quote_target: quote_target.to_string(),
            base_decimals: 6,
            quote_decimals: 6,
            lp_fee_rate: fee_rate.to_string(),
            pool_type: "dsp".into(),
        }
    }

    #[test]
    fn constant_price_pool_k_zero() {
        // k=0 means constant price: output = i * input (no slippage)
        // i = 1e18 (1:1 price), fee = 0
        let pool = make_dodo_pool(
            1_000_000_000, // 1000 USDC base
            1_000_000_000, // 1000 USDT quote
            1_000_000_000,
            1_000_000_000,
            1_000_000_000_000_000_000, // i = 1.0 (1e18)
            0,                          // k = 0
            0,                          // no fee
        );
        let out = get_amount_out(&pool, 1_000_000, true).unwrap();
        // Should be ~1:1
        assert!(out > 990_000, "Output {out} should be close to 1M");
        assert!(out <= 1_000_000, "Output {out} should not exceed input");
    }

    #[test]
    fn constant_product_k_one() {
        // k=1e18 means full constant product behavior
        let pool = make_dodo_pool(
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000_000_000_000, // i = 1.0
            1_000_000_000_000_000_000, // k = 1.0
            3_000_000_000_000_000,     // 0.3% fee
        );
        let out = get_amount_out(&pool, 10_000_000, true).unwrap();
        assert!(out > 0);
        assert!(out < 10_000_000, "Should have slippage + fee");
    }

    #[test]
    fn sell_base_and_sell_quote_symmetry() {
        // With 1:1 price and equal reserves, sell base and sell quote should give similar outputs
        let pool = make_dodo_pool(
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000_000_000_000,
            500_000_000_000_000_000, // k = 0.5
            0,
        );
        let out_base = get_amount_out(&pool, 1_000_000, true).unwrap();
        let out_quote = get_amount_out(&pool, 1_000_000, false).unwrap();
        // Should be approximately equal for symmetric pool
        let diff = if out_base > out_quote {
            out_base - out_quote
        } else {
            out_quote - out_base
        };
        assert!(
            diff < 100,
            "Symmetric pool outputs should be close: base={out_base}, quote={out_quote}"
        );
    }

    #[test]
    fn spot_price_at_target() {
        // When B = B0, spot price should equal i
        let pool = make_dodo_pool(
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            2_000_000_000_000_000_000, // i = 2.0
            500_000_000_000_000_000,   // k = 0.5
            0,
        );
        let price = spot_price(&pool).unwrap();
        assert!(
            (price - 2.0).abs() < 1e-9,
            "Spot price {price} should be 2.0 when B=B0"
        );
    }

    #[test]
    fn zero_reserve_returns_none() {
        let pool = make_dodo_pool(0, 1_000_000, 0, 1_000_000, 1_000_000_000_000_000_000, 0, 0);
        assert!(get_amount_out(&pool, 1000, true).is_none());
    }

    #[test]
    fn fee_reduces_output() {
        let pool_no_fee = make_dodo_pool(
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000_000_000_000,
            500_000_000_000_000_000,
            0,
        );
        let pool_with_fee = make_dodo_pool(
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000_000_000_000,
            500_000_000_000_000_000,
            3_000_000_000_000_000, // 0.3%
        );
        let out_no_fee = get_amount_out(&pool_no_fee, 1_000_000, true).unwrap();
        let out_with_fee = get_amount_out(&pool_with_fee, 1_000_000, true).unwrap();
        assert!(out_no_fee > out_with_fee, "Fee should reduce output");
    }

    // ── ABI decode tests ──────────────────────────────────────────────────────

    #[test]
    fn decode_address_ok() {
        let hex = "0x000000000000000000000000abcd000000000000000000000000000000001234";
        let addr = decode_address(hex).unwrap();
        assert_eq!(addr, "0xabcd000000000000000000000000000000001234");
    }

    #[test]
    fn decode_uint256_ok() {
        let hex = format!("0x{:064x}", 42u128);
        assert_eq!(decode_uint256(&hex), Some(42));
    }
}
