use shared::rpc::EthClient;
use tracing::debug;

use crate::models::liquidity::WombatPool;

// ── Pool addresses (Arbitrum) ────────────────────────────────────────────────

/// Wombat Exchange main pool on Arbitrum
pub const WOMBAT_MAIN_POOL: &str = "0xc6bc781E20f9323012F6e422bdf552Ff06bA6CD1";

// ── ABI selectors ─────────────────────────────────────────────────────────────

/// `ampFactor()` → 4-byte selector
const AMP_FACTOR_SELECTOR: &str = "1f8118e5";

/// `haircutRate()` → 4-byte selector
const HAIRCUT_RATE_SELECTOR: &str = "af679861";

/// `addressOfAsset(address)` → 4-byte selector — returns the Asset contract for a token
const ADDRESS_OF_ASSET_SELECTOR: &str = "e4393a05";

/// `cash()` on Asset contract → 4-byte selector
const CASH_SELECTOR: &str = "961be391";

/// `liability()` on Asset contract → 4-byte selector
const LIABILITY_SELECTOR: &str = "f37f3b20";

/// `underlyingTokenDecimals()` on Asset contract → 4-byte selector
const UNDERLYING_DECIMALS_SELECTOR: &str = "503ccb6e";

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

/// Fetch a Wombat pool's state from on-chain data.
///
/// `pool_address`: The Wombat pool contract address.
/// `token_addresses`: Known token addresses in the pool (from config).
///
/// For each token, fetches the Asset contract (which tracks cash/liability).
pub async fn fetch_pool(
    rpc: &EthClient,
    pool_address: &str,
    token_addresses: &[&str],
) -> anyhow::Result<Option<WombatPool>> {
    if token_addresses.is_empty() {
        return Ok(None);
    }

    // Fetch pool-level params
    let amp_call = format!("0x{AMP_FACTOR_SELECTOR}");
    let haircut_call = format!("0x{HAIRCUT_RATE_SELECTOR}");
    let (amp_hex, haircut_hex) = tokio::try_join!(
        rpc.call(pool_address, &amp_call),
        rpc.call(pool_address, &haircut_call),
    )?;

    let amp_factor = decode_uint256(&amp_hex).unwrap_or(0);
    let haircut_rate = decode_uint256(&haircut_hex).unwrap_or(0);

    let mut tokens = Vec::new();
    let mut cash = Vec::new();
    let mut liability = Vec::new();
    let mut decimals = Vec::new();

    for &token_addr in token_addresses {
        // Get Asset contract for this token
        let asset_call = format!(
            "0x{ADDRESS_OF_ASSET_SELECTOR}{}",
            pad_address(token_addr)
        );
        let asset_hex = rpc.call(pool_address, &asset_call).await?;
        let asset_address = match decode_address(&asset_hex) {
            Some(addr) => addr,
            None => {
                debug!(
                    pool = pool_address,
                    token = token_addr,
                    "No asset found for token in Wombat pool, skipping"
                );
                continue;
            }
        };

        // Fetch cash, liability, and decimals from Asset contract
        let cash_call = format!("0x{CASH_SELECTOR}");
        let liab_call = format!("0x{LIABILITY_SELECTOR}");
        let dec_call = format!("0x{DECIMALS_SELECTOR}");
        let (cash_hex, liab_hex, dec_hex) = tokio::try_join!(
            rpc.call(&asset_address, &cash_call),
            rpc.call(&asset_address, &liab_call),
            rpc.call(token_addr, &dec_call),
        )?;

        let cash_val = decode_uint256(&cash_hex).unwrap_or(0);
        let liab_val = decode_uint256(&liab_hex).unwrap_or(0);
        let dec_val = decode_uint8(&dec_hex).unwrap_or(18);

        tokens.push(token_addr.to_string());
        cash.push(cash_val.to_string());
        liability.push(liab_val.to_string());
        decimals.push(dec_val);
    }

    if tokens.is_empty() {
        return Ok(None);
    }

    debug!(
        address = pool_address,
        num_tokens = tokens.len(),
        amp = amp_factor,
        haircut = haircut_rate,
        "Fetched Wombat pool"
    );

    // Fee bps: derive from haircut rate. Haircut is scaled to 1e18.
    // haircut_rate / 1e18 * 10000 = fee_bps
    let fee_bps = (haircut_rate as f64 / 1e18 * 10_000.0).round() as u32;

    Ok(Some(WombatPool {
        address: pool_address.to_string(),
        tokens,
        cash,
        liability,
        decimals,
        amp_factor: amp_factor.to_string(),
        haircut_rate: haircut_rate.to_string(),
        fee_bps,
    }))
}

/// Refresh cash and liability for all assets in a Wombat pool.
pub async fn sync_pool(rpc: &EthClient, pool: &mut WombatPool) -> anyhow::Result<()> {
    for i in 0..pool.tokens.len() {
        let asset_call = format!(
            "0x{ADDRESS_OF_ASSET_SELECTOR}{}",
            pad_address(&pool.tokens[i])
        );
        let asset_hex = rpc.call(&pool.address, &asset_call).await?;
        if let Some(asset_address) = decode_address(&asset_hex) {
            let cash_call = format!("0x{CASH_SELECTOR}");
            let liab_call = format!("0x{LIABILITY_SELECTOR}");
            let (cash_hex, liab_hex) = tokio::try_join!(
                rpc.call(&asset_address, &cash_call),
                rpc.call(&asset_address, &liab_call),
            )?;

            if let Some(v) = decode_uint256(&cash_hex) {
                pool.cash[i] = v.to_string();
            }
            if let Some(v) = decode_uint256(&liab_hex) {
                pool.liability[i] = v.to_string();
            }
        }
    }

    debug!(
        address = %pool.address,
        "Synced Wombat pool"
    );
    Ok(())
}

// ── Wombat Stableswap Math ───────────────────────────────────────────────────
//
// Wombat uses a coverage-ratio-based model:
//   r_i = cash_i / liability_i   (coverage ratio for asset i)
//
// The swap formula is based on maintaining pool health:
//   dy = D * A * (r_x - r_x') / (A * r_x' + 1)  (simplified)
//
// Where:
//   r_x = coverage ratio of from-asset before swap
//   r_x' = coverage ratio of from-asset after swap
//   A = amplification factor (higher A = more stable pricing)
//   D = liability of to-asset

/// 1e18 constant for fixed-point math.
const WAD: f64 = 1e18;

/// Compute the output amount for a Wombat pool swap.
///
/// `from_idx`: index of the token being sold (in pool.tokens)
/// `to_idx`: index of the token being bought (in pool.tokens)
/// `amount_in`: input amount in from-token's raw units
pub fn get_amount_out(
    pool: &WombatPool,
    from_idx: usize,
    to_idx: usize,
    amount_in: u128,
) -> Option<u128> {
    if from_idx >= pool.tokens.len() || to_idx >= pool.tokens.len() || from_idx == to_idx {
        return None;
    }
    if amount_in == 0 {
        return None;
    }

    let amp = pool.amp_factor.parse::<f64>().ok()? / WAD;
    let haircut = pool.haircut_rate.parse::<f64>().ok()? / WAD;

    // From-asset state
    let from_cash = pool.cash[from_idx].parse::<f64>().ok()?;
    let from_liability = pool.liability[from_idx].parse::<f64>().ok()?;
    // To-asset state
    let to_cash = pool.cash[to_idx].parse::<f64>().ok()?;
    let to_liability = pool.liability[to_idx].parse::<f64>().ok()?;

    if from_liability <= 0.0 || to_liability <= 0.0 || to_cash <= 0.0 {
        return None;
    }

    // Normalize to 18 decimals for uniform math
    let from_dec = pool.decimals[from_idx] as f64;
    let to_dec = pool.decimals[to_idx] as f64;
    let from_scale = 10f64.powf(18.0 - from_dec);
    let to_scale = 10f64.powf(18.0 - to_dec);

    let from_cash_18 = from_cash * from_scale;
    let from_liab_18 = from_liability * from_scale;
    let to_cash_18 = to_cash * to_scale;
    let to_liab_18 = to_liability * to_scale;
    let amount_in_18 = amount_in as f64 * from_scale;

    // Coverage ratios before swap
    let r_from = from_cash_18 / from_liab_18;
    let r_from_new = (from_cash_18 + amount_in_18) / from_liab_18;
    let r_to = to_cash_18 / to_liab_18;

    // Compute "from" side: how much value flows in
    // quotient_from = A * (r_from_new - r_from) / (1/(1-A) + r_from_new * A)
    // Simplified Wombat invariant contribution
    let from_value = wombat_quote(r_from, r_from_new, amp, from_liab_18)?;

    // Compute "to" side: find r_to_new such that to_value = from_value
    let r_to_new = wombat_inverse_quote(r_to, from_value, amp, to_liab_18)?;

    if r_to_new >= r_to {
        return None; // No output possible
    }

    // Output = change in to-asset cash
    let output_18 = (r_to - r_to_new) * to_liab_18;
    if output_18 <= 0.0 {
        return None;
    }

    // Apply haircut
    let output_after_haircut = output_18 * (1.0 - haircut);
    if output_after_haircut <= 0.0 {
        return None;
    }

    // Convert back from 18 decimals
    let output = output_after_haircut / to_scale;
    if output < 1.0 {
        return None;
    }

    Some(output as u128)
}

/// Wombat invariant: compute the value flowing through when coverage ratio
/// changes from r to r_new (with amplification A and liability D).
///
/// Value = D * [ A * (r_new - r) + (1 / (1/r_new) - 1/(1/r)) ]
/// Simplified: value = D * (A * dr + 1/r - 1/r_new) where dr = r_new - r
fn wombat_quote(r: f64, r_new: f64, amp: f64, liability: f64) -> Option<f64> {
    if r <= 0.0 || r_new <= 0.0 || liability <= 0.0 {
        return None;
    }
    // Core formula: value = liability * (r_new^A - r^A) for the general case
    // For the simplified model: value = liability * (A*(r_new - r) + r^(-1) - r_new^(-1))
    // This ensures slippage increases as coverage moves away from 1.0
    let dr = r_new - r;
    let inv_r = 1.0 / r;
    let inv_r_new = 1.0 / r_new;
    let value = liability * (amp * dr + inv_r - inv_r_new);
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
}

/// Inverse of wombat_quote: given target value flowing out, compute r_new
/// such that the to-asset gives that much value.
///
/// We need: value = liability * (A*(r - r_new) + 1/r_new - 1/r)
/// This requires Newton's method to solve for r_new.
fn wombat_inverse_quote(r: f64, value: f64, amp: f64, liability: f64) -> Option<f64> {
    if liability <= 0.0 || value <= 0.0 || r <= 0.0 {
        return None;
    }

    // We want to find r_new < r such that:
    // liability * (A*(r - r_new) + 1/r_new - 1/r) = value
    // f(r_new) = liability * (A*(r - r_new) + 1/r_new - 1/r) - value = 0
    // f'(r_new) = liability * (-A - 1/r_new^2)

    let target = value / liability;
    let inv_r = 1.0 / r;

    // Initial guess: r_new ≈ r - value / (liability * A) (linear approximation)
    let mut r_new = r - target / (amp + inv_r * inv_r);
    if r_new <= 0.0 {
        r_new = r * 0.5; // Fallback
    }

    for _ in 0..128 {
        let inv_rn = 1.0 / r_new;
        let f_val = amp * (r - r_new) + inv_rn - inv_r - target;
        let f_deriv = -amp - inv_rn * inv_rn;
        if f_deriv.abs() < 1e-30 {
            break;
        }
        let step = f_val / f_deriv;
        let r_next = r_new - step;

        if (r_next - r_new).abs() < 1e-12 {
            return Some(r_next.max(1e-18));
        }
        r_new = r_next;
        if r_new <= 0.0 {
            r_new = 1e-18;
        }
    }

    Some(r_new.max(1e-18))
}

/// Convenience: compute output by token addresses instead of indices.
pub fn get_amount_out_by_tokens(
    pool: &WombatPool,
    from_token: &str,
    to_token: &str,
    amount_in: u128,
) -> Option<u128> {
    let from_idx = pool
        .tokens
        .iter()
        .position(|t| t.to_lowercase() == from_token.to_lowercase())?;
    let to_idx = pool
        .tokens
        .iter()
        .position(|t| t.to_lowercase() == to_token.to_lowercase())?;
    get_amount_out(pool, from_idx, to_idx, amount_in)
}

/// Spot price of token at from_idx denominated in token at to_idx.
pub fn spot_price(pool: &WombatPool, from_idx: usize, to_idx: usize) -> Option<f64> {
    if from_idx >= pool.tokens.len() || to_idx >= pool.tokens.len() || from_idx == to_idx {
        return None;
    }

    let amp = pool.amp_factor.parse::<f64>().ok()? / WAD;

    let from_cash = pool.cash[from_idx].parse::<f64>().ok()?;
    let from_liability = pool.liability[from_idx].parse::<f64>().ok()?;
    let to_cash = pool.cash[to_idx].parse::<f64>().ok()?;
    let to_liability = pool.liability[to_idx].parse::<f64>().ok()?;

    if from_liability <= 0.0 || to_liability <= 0.0 || from_cash <= 0.0 || to_cash <= 0.0 {
        return None;
    }

    let r_from = from_cash / from_liability;
    let r_to = to_cash / to_liability;

    // Marginal price at from-asset: dp/dr = A + 1/r^2
    let marginal_from = amp + 1.0 / (r_from * r_from);
    let marginal_to = amp + 1.0 / (r_to * r_to);

    // Spot price: marginal_from / marginal_to * (from_liability / to_liability)
    Some(marginal_from / marginal_to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::WombatPool;

    fn make_wombat_pool(
        cash: &[u128],
        liability: &[u128],
        decimals: &[u8],
        amp: u128,
        haircut: u128,
    ) -> WombatPool {
        let tokens: Vec<String> = (0..cash.len())
            .map(|i| format!("0xtoken{i}"))
            .collect();
        WombatPool {
            address: "0x0000".into(),
            tokens,
            cash: cash.iter().map(|c| c.to_string()).collect(),
            liability: liability.iter().map(|l| l.to_string()).collect(),
            decimals: decimals.to_vec(),
            amp_factor: amp.to_string(),
            haircut_rate: haircut.to_string(),
            fee_bps: (haircut as f64 / 1e18 * 10_000.0).round() as u32,
        }
    }

    #[test]
    fn balanced_pool_near_1_to_1() {
        // 3 stablecoins at equal cash/liability: should swap near 1:1
        let pool = make_wombat_pool(
            &[1_000_000_000, 1_000_000_000, 1_000_000_000], // 1000 each (6 decimals)
            &[1_000_000_000, 1_000_000_000, 1_000_000_000],
            &[6, 6, 6],
            25_000_000_000_000_000, // A = 0.025 (typical Wombat)
            200_000_000_000_000,    // haircut = 0.02% = 2 bps
        );
        let amount_in = 1_000_000u128; // 1.0 token
        let out = get_amount_out(&pool, 0, 1, amount_in).unwrap();
        // Should be close to 1.0 minus haircut
        assert!(
            out > 990_000,
            "Output {out} should be close to 1M (near 1:1)"
        );
        assert!(out < 1_000_000, "Output should be less than input due to fee");
    }

    #[test]
    fn unbalanced_pool_has_slippage() {
        // Token0 has excess cash → selling token0 should give worse rate
        let pool = make_wombat_pool(
            &[2_000_000_000, 500_000_000, 1_000_000_000],
            &[1_000_000_000, 1_000_000_000, 1_000_000_000],
            &[6, 6, 6],
            25_000_000_000_000_000,
            200_000_000_000_000,
        );
        let amount_in = 10_000_000u128;
        let out = get_amount_out(&pool, 0, 1, amount_in);
        // Should get some output but with more slippage
        assert!(out.is_some());
        let out_val = out.unwrap();
        assert!(out_val < amount_in, "Unbalanced: output should be less than input");
        assert!(out_val > 0);
    }

    #[test]
    fn same_token_returns_none() {
        let pool = make_wombat_pool(
            &[1_000_000_000, 1_000_000_000],
            &[1_000_000_000, 1_000_000_000],
            &[6, 6],
            25_000_000_000_000_000,
            200_000_000_000_000,
        );
        assert!(get_amount_out(&pool, 0, 0, 1_000_000).is_none());
    }

    #[test]
    fn out_of_bounds_returns_none() {
        let pool = make_wombat_pool(
            &[1_000_000_000, 1_000_000_000],
            &[1_000_000_000, 1_000_000_000],
            &[6, 6],
            25_000_000_000_000_000,
            200_000_000_000_000,
        );
        assert!(get_amount_out(&pool, 0, 5, 1_000_000).is_none());
    }

    #[test]
    fn zero_amount_returns_none() {
        let pool = make_wombat_pool(
            &[1_000_000_000, 1_000_000_000],
            &[1_000_000_000, 1_000_000_000],
            &[6, 6],
            25_000_000_000_000_000,
            200_000_000_000_000,
        );
        assert!(get_amount_out(&pool, 0, 1, 0).is_none());
    }

    #[test]
    fn spot_price_balanced_near_1() {
        let pool = make_wombat_pool(
            &[1_000_000_000, 1_000_000_000],
            &[1_000_000_000, 1_000_000_000],
            &[6, 6],
            25_000_000_000_000_000,
            0,
        );
        let price = spot_price(&pool, 0, 1).unwrap();
        assert!(
            (price - 1.0).abs() < 0.01,
            "Balanced pool spot price {price} should be ~1.0"
        );
    }

    #[test]
    fn by_tokens_matches_by_idx() {
        let mut pool = make_wombat_pool(
            &[1_000_000_000, 1_000_000_000],
            &[1_000_000_000, 1_000_000_000],
            &[6, 6],
            25_000_000_000_000_000,
            200_000_000_000_000,
        );
        pool.tokens = vec!["0xUSDC".to_string(), "0xUSDT".to_string()];
        let out_idx = get_amount_out(&pool, 0, 1, 1_000_000).unwrap();
        let out_tok = get_amount_out_by_tokens(&pool, "0xusdc", "0xusdt", 1_000_000).unwrap();
        assert_eq!(out_idx, out_tok);
    }

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
