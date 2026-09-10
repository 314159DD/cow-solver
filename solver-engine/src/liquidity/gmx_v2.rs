use shared::rpc::EthClient;
use tracing::debug;

use crate::models::liquidity::GmxV2Pool;

// ── GMX V2 contract addresses (Arbitrum) ────────────────────────────────────

/// GMX V2 Reader contract on Arbitrum — used to fetch market/pool info.
pub const GMX_V2_READER: &str = "0xf60becbba223EEA9495Da3f606753867eC10d139";

/// GMX V2 DataStore contract on Arbitrum — stores all protocol state.
pub const GMX_V2_DATA_STORE: &str = "0xFD70de6b91282D8017aA4E741e9Ae325CAb992d8";

/// GMX V2 Exchange Router on Arbitrum — used for executing swaps.
pub const GMX_V2_EXCHANGE_ROUTER: &str = "0x7C68C7866A64FA2160F78EEaE12217FFbf871fa8";

// ── Key market addresses on Arbitrum ────────────────────────────────────────

/// ETH/USD market
pub const GMX_V2_ETH_USD_MARKET: &str = "0x70d95587d40A2cda56C5e14bBbF65707D79e44e6";
/// BTC/USD market
pub const GMX_V2_BTC_USD_MARKET: &str = "0x47c031236e19d024b42f8AE6DA7A02043E7889Cb";
/// ARB/USD market
pub const GMX_V2_ARB_USD_MARKET: &str = "0xC25cEf6061Cf5dE5eb761b50E4743c1F5D7E5407";

// ── ABI selectors ───────────────────────────────────────────────────────────

/// `getMarket(address dataStore, address key)` → Reader selector
/// Returns: (address marketToken, address indexToken, address longToken, address shortToken)
const GET_MARKET_SELECTOR: &str = "7dc0d1d0";

/// `getPoolAmount(address dataStore, address market, address token)` → Reader selector
/// Returns: uint256
const GET_POOL_AMOUNT_SELECTOR: &str = "5bd6e168";

// ── ABI encoding helpers ────────────────────────────────────────────────────

/// Pad an Ethereum address to 32 bytes (strip 0x, left-pad with zeros).
fn pad_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

/// Decode a 32-byte ABI-encoded address.
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

/// Decode a uint256 from a 32-byte hex word.
fn decode_uint256(hex: &str) -> Option<u128> {
    let clean = hex.trim_start_matches("0x").trim_start_matches('0');
    if clean.is_empty() {
        return Some(0);
    }
    u128::from_str_radix(clean, 16).ok()
}

/// Encode `getMarket(dataStore, marketAddress)`.
fn encode_get_market(data_store: &str, market: &str) -> String {
    format!(
        "0x{}{}{}",
        GET_MARKET_SELECTOR,
        pad_address(data_store),
        pad_address(market),
    )
}

/// Encode `getPoolAmount(dataStore, market, token)`.
fn encode_get_pool_amount(data_store: &str, market: &str, token: &str) -> String {
    format!(
        "0x{}{}{}{}",
        GET_POOL_AMOUNT_SELECTOR,
        pad_address(data_store),
        pad_address(market),
        pad_address(token),
    )
}

// ── Pool discovery ──────────────────────────────────────────────────────────

/// Fetch a GMX V2 market pool from the Reader contract.
///
/// Returns `None` if the market doesn't exist or tokens can't be decoded.
///
/// Note: Oracle prices are initialized to "0.0" and must be updated via
/// `sync_pool` with actual oracle data before swap math is accurate.
pub async fn fetch_pool(
    rpc: &EthClient,
    market_address: &str,
) -> anyhow::Result<Option<GmxV2Pool>> {
    // 1. Get market info (indexToken, longToken, shortToken)
    let market_call = encode_get_market(GMX_V2_DATA_STORE, market_address);
    let market_hex = rpc.call(GMX_V2_READER, &market_call).await?;
    let clean = market_hex.trim_start_matches("0x");

    // Response is 4 words: marketToken, indexToken, longToken, shortToken
    if clean.len() < 256 {
        debug!(
            market = market_address,
            "GMX V2 getMarket response too short"
        );
        return Ok(None);
    }

    let _market_token = decode_address(&format!("0x{}", &clean[0..64]));
    let index_token = match decode_address(&format!("0x{}", &clean[64..128])) {
        Some(addr) => addr,
        None => return Ok(None),
    };
    let long_token = match decode_address(&format!("0x{}", &clean[128..192])) {
        Some(addr) => addr,
        None => return Ok(None),
    };
    let short_token = match decode_address(&format!("0x{}", &clean[192..256])) {
        Some(addr) => addr,
        None => return Ok(None),
    };

    debug!(
        market = market_address,
        index_token = %index_token,
        long_token = %long_token,
        short_token = %short_token,
        "Found GMX V2 market"
    );

    // 2. Fetch pool amounts for long and short tokens
    let long_amount_call =
        encode_get_pool_amount(GMX_V2_DATA_STORE, market_address, &long_token);
    let short_amount_call =
        encode_get_pool_amount(GMX_V2_DATA_STORE, market_address, &short_token);

    let (long_hex, short_hex) = tokio::try_join!(
        rpc.call(GMX_V2_READER, &long_amount_call),
        rpc.call(GMX_V2_READER, &short_amount_call),
    )?;

    let long_amount = decode_uint256(&long_hex).unwrap_or(0);
    let short_amount = decode_uint256(&short_hex).unwrap_or(0);

    debug!(
        market = market_address,
        long_amount = long_amount,
        short_amount = short_amount,
        "GMX V2 pool amounts"
    );

    Ok(Some(GmxV2Pool {
        address: market_address.to_string(),
        long_token,
        short_token,
        index_token,
        long_token_amount: long_amount.to_string(),
        short_token_amount: short_amount.to_string(),
        swap_fee_bps: 7, // GMX V2 default swap fee ~0.07%
        swap_impact_factor_positive: "0.0".to_string(),
        swap_impact_factor_negative: "0.0".to_string(),
        long_token_price_usd: "0.0".to_string(),
        short_token_price_usd: "0.0".to_string(),
    }))
}

/// Refresh pool amounts for an existing GMX V2 pool in-place.
pub async fn sync_pool(rpc: &EthClient, pool: &mut GmxV2Pool) -> anyhow::Result<()> {
    let long_amount_call =
        encode_get_pool_amount(GMX_V2_DATA_STORE, &pool.address, &pool.long_token);
    let short_amount_call =
        encode_get_pool_amount(GMX_V2_DATA_STORE, &pool.address, &pool.short_token);

    let (long_hex, short_hex) = tokio::try_join!(
        rpc.call(GMX_V2_READER, &long_amount_call),
        rpc.call(GMX_V2_READER, &short_amount_call),
    )?;

    if let Some(amt) = decode_uint256(&long_hex) {
        pool.long_token_amount = amt.to_string();
    }
    if let Some(amt) = decode_uint256(&short_hex) {
        pool.short_token_amount = amt.to_string();
    }

    debug!(
        market = %pool.address,
        long_amount = %pool.long_token_amount,
        short_amount = %pool.short_token_amount,
        "Synced GMX V2 pool amounts"
    );
    Ok(())
}

// ── Spot price ──────────────────────────────────────────────────────────────

/// Returns the spot price of long token denominated in short token.
///
/// Uses oracle prices stored in the pool. Returns `None` if prices are not set.
pub fn spot_price(pool: &GmxV2Pool) -> Option<f64> {
    let long_price = pool.long_token_price_usd.parse::<f64>().ok()?;
    let short_price = pool.short_token_price_usd.parse::<f64>().ok()?;
    if short_price == 0.0 || long_price == 0.0 {
        return None;
    }
    Some(long_price / short_price)
}

// ── Swap math ───────────────────────────────────────────────────────────────

/// Compute the output amount for a GMX V2 swap.
///
/// GMX V2 swap formula:
///   output = input * price_ratio * (1 - swap_fee) * (1 - price_impact)
///
/// Price impact depends on pool imbalance:
///   - If the swap improves balance (reduces USD diff), impact is positive (bonus)
///   - If the swap worsens balance (increases USD diff), impact is negative (penalty)
///
/// `long_to_short`: true = selling long token for short token (e.g. WETH → USDC)
pub fn get_amount_out(pool: &GmxV2Pool, amount_in: u128, long_to_short: bool) -> Option<u128> {
    if amount_in == 0 {
        return None;
    }

    let long_price = pool.long_token_price_usd.parse::<f64>().ok()?;
    let short_price = pool.short_token_price_usd.parse::<f64>().ok()?;

    if long_price <= 0.0 || short_price <= 0.0 {
        return None;
    }

    // Check pool has sufficient liquidity
    let long_amount = pool.long_token_amount.parse::<f64>().ok()?;
    let short_amount = pool.short_token_amount.parse::<f64>().ok()?;

    if long_to_short && short_amount <= 0.0 {
        return None;
    }
    if !long_to_short && long_amount <= 0.0 {
        return None;
    }

    // Price ratio: how many output tokens per 1 input token
    let price_ratio = if long_to_short {
        long_price / short_price
    } else {
        short_price / long_price
    };

    // Swap fee
    let fee_factor = 1.0 - (pool.swap_fee_bps as f64 / 10_000.0);

    // Price impact: based on how much the swap changes pool balance (as a ratio).
    //
    // GMX V2 uses pool imbalance to compute impact:
    //   impact = (next_diff - initial_diff) / total_pool_value * impact_exponent_factor
    //
    // We normalize by total pool value to get a percentage impact.
    let long_usd = long_amount * long_price;
    let short_usd = short_amount * short_price;
    let total_pool_usd = long_usd + short_usd;
    let initial_diff = (long_usd - short_usd).abs();

    let input_f64 = amount_in as f64;
    let output_estimate = input_f64 * price_ratio;

    let (new_long_usd, new_short_usd) = if long_to_short {
        (
            long_usd + input_f64 * long_price,
            short_usd - output_estimate * short_price,
        )
    } else {
        (
            long_usd - output_estimate * long_price,
            short_usd + input_f64 * short_price,
        )
    };
    let next_diff = (new_long_usd - new_short_usd).abs();

    // Impact factor: normalized by pool size
    let impact_factor_pos = pool
        .swap_impact_factor_positive
        .parse::<f64>()
        .unwrap_or(0.0);
    let impact_factor_neg = pool
        .swap_impact_factor_negative
        .parse::<f64>()
        .unwrap_or(0.0);

    let price_impact = if total_pool_usd <= 0.0 {
        0.0
    } else if next_diff < initial_diff {
        // Swap improves balance → positive impact (slight bonus, capped at 0.5%)
        let improvement_ratio = (initial_diff - next_diff) / total_pool_usd;
        let bonus = improvement_ratio * impact_factor_pos;
        -(bonus.min(0.005))
    } else {
        // Swap worsens balance → negative impact (penalty, capped at 5%)
        let deterioration_ratio = (next_diff - initial_diff) / total_pool_usd;
        (deterioration_ratio * impact_factor_neg).min(0.05)
    };

    let impact_multiplier = (1.0 - price_impact).max(0.0);

    // Final output
    let output = input_f64 * price_ratio * fee_factor * impact_multiplier;
    if output <= 0.0 {
        return None;
    }

    Some(output as u128)
}

/// Compute the input needed to receive a specific output amount.
///
/// Inverse of `get_amount_out`. Uses the simplified formula:
///   input = output / (price_ratio * (1 - fee) * (1 - impact_estimate))
///
/// Note: price impact is estimated using a simplified model since the exact
/// inverse requires solving a quadratic.
pub fn get_amount_in(pool: &GmxV2Pool, amount_out: u128, long_to_short: bool) -> Option<u128> {
    if amount_out == 0 {
        return None;
    }

    let long_price = pool.long_token_price_usd.parse::<f64>().ok()?;
    let short_price = pool.short_token_price_usd.parse::<f64>().ok()?;

    if long_price <= 0.0 || short_price <= 0.0 {
        return None;
    }

    let price_ratio = if long_to_short {
        long_price / short_price
    } else {
        short_price / long_price
    };

    if price_ratio <= 0.0 {
        return None;
    }

    let fee_factor = 1.0 - (pool.swap_fee_bps as f64 / 10_000.0);
    // Conservative estimate: assume slight negative impact
    let impact_estimate = 0.999;

    let input = (amount_out as f64) / (price_ratio * fee_factor * impact_estimate);
    if input <= 0.0 {
        return None;
    }

    // Round up
    Some((input as u128).checked_add(1)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::GmxV2Pool;

    fn make_eth_usd_pool(
        long_amount: u128,
        short_amount: u128,
        long_price: f64,
        short_price: f64,
    ) -> GmxV2Pool {
        GmxV2Pool {
            address: GMX_V2_ETH_USD_MARKET.to_string(),
            long_token: "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".to_string(), // WETH
            short_token: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831".to_string(), // USDC
            index_token: "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".to_string(), // WETH
            long_token_amount: long_amount.to_string(),
            short_token_amount: short_amount.to_string(),
            swap_fee_bps: 7,
            swap_impact_factor_positive: "0.00001".to_string(),
            swap_impact_factor_negative: "0.00002".to_string(),
            long_token_price_usd: long_price.to_string(),
            short_token_price_usd: short_price.to_string(),
        }
    }

    #[test]
    fn basic_long_to_short_swap() {
        // Using uniform units: price ratio 3500:1
        // Pool: 1000 units of long token, 3_500_000 units of short token — balanced
        // Oracle prices are per-raw-unit (both tokens use same 18-dec scale in test)
        let pool = make_eth_usd_pool(
            1_000_000_000_000_000_000_000, // 1000 units (18 dec)
            3_500_000_000_000_000_000_000_000, // 3.5M units (18 dec)
            3500.0,
            1.0,
        );

        // Swap 1 unit of long token → short token
        let amount_in = 1_000_000_000_000_000_000u128; // 1e18 = 1 unit
        let out = get_amount_out(&pool, amount_in, true).unwrap();

        // Should get ~3500 units (minus ~0.07% fee)
        // 3500 * 0.9993 ≈ 3497.55 → in 18-dec: ~3_497_550_000_000_000_000_000
        let expected_min = 3_490_000_000_000_000_000_000u128;
        let expected_max = 3_500_000_000_000_000_000_000u128;
        assert!(out > expected_min, "Output {out} too low for swap");
        assert!(out < expected_max, "Output {out} too high (should deduct fee)");
    }

    #[test]
    fn basic_short_to_long_swap() {
        let pool = make_eth_usd_pool(
            1_000_000_000_000_000_000_000,     // 1000 units
            3_500_000_000_000_000_000_000_000,  // 3.5M units
            3500.0,
            1.0,
        );

        // Swap 3500 units of short token → long token
        let amount_in = 3_500_000_000_000_000_000_000u128; // 3500 units (18 dec)
        let out = get_amount_out(&pool, amount_in, false).unwrap();

        // Should get ~1 unit of long token (minus fee)
        // 3500 * (1/3500) * 0.9993 ≈ 0.9993 units → ~999_300_000_000_000_000
        let expected_min = 990_000_000_000_000_000u128;
        let expected_max = 1_000_000_000_000_000_000u128;
        assert!(out > expected_min, "Output {out} too low");
        assert!(out < expected_max, "Output {out} too high");
    }

    #[test]
    fn zero_amount_returns_none() {
        let pool = make_eth_usd_pool(1000, 3_500_000, 3500.0, 1.0);
        assert!(get_amount_out(&pool, 0, true).is_none());
    }

    #[test]
    fn zero_price_returns_none() {
        let pool = make_eth_usd_pool(1000, 3_500_000, 0.0, 1.0);
        assert!(get_amount_out(&pool, 100, true).is_none());
    }

    #[test]
    fn spot_price_eth_usd() {
        let pool = make_eth_usd_pool(1000, 3_500_000, 3500.0, 1.0);
        let price = spot_price(&pool).unwrap();
        assert!((price - 3500.0).abs() < 0.01);
    }

    #[test]
    fn spot_price_zero_returns_none() {
        let pool = make_eth_usd_pool(1000, 3_500_000, 0.0, 1.0);
        assert!(spot_price(&pool).is_none());
    }

    #[test]
    fn get_amount_in_round_trip() {
        let pool = make_eth_usd_pool(
            1_000_000_000_000_000_000_000,     // 1000 units
            3_500_000_000_000_000_000_000_000,  // 3.5M units
            3500.0,
            1.0,
        );

        let amount_in = 1_000_000_000_000_000_000u128; // 1 unit
        let out = get_amount_out(&pool, amount_in, true).unwrap();
        assert!(out > 0, "get_amount_out should produce output");

        let in_back = get_amount_in(&pool, out, true).unwrap();

        // get_amount_in should require roughly the same as original input
        // (within ~2% tolerance due to fee estimation and impact)
        assert!(
            in_back >= amount_in - amount_in / 50,
            "Round-trip: needed {in_back} to get {out}, original was {amount_in}"
        );
    }

    #[test]
    fn decode_address_ok() {
        let hex = "0x00000000000000000000000082af49447d8a07e3bd95bd0d56f35241523fbab1";
        let addr = decode_address(hex).unwrap();
        assert_eq!(addr, "0x82af49447d8a07e3bd95bd0d56f35241523fbab1");
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
        let hex = "0x0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(decode_uint256(hex), Some(0));
    }

    #[test]
    fn encode_get_market_length() {
        let data = encode_get_market(GMX_V2_DATA_STORE, GMX_V2_ETH_USD_MARKET);
        // 0x + 8 (selector) + 64 (addr1) + 64 (addr2) = 138 chars
        assert_eq!(data.len(), 2 + 8 + 64 + 64);
    }
}
