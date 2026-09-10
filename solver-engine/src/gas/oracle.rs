//! Dynamic gas price oracle for accurate cost estimation.
//!
//! ## Arbitrum
//! Fetches L1 data posting costs from the `ArbGasInfo` precompile at
//! `0x000000000000000000000000000000000000006C`. Caches the result for
//! the duration of a single solve() call (one auction).
//!
//! ## Mainnet
//! Fetches `eth_gasPrice` as a sanity check against the auction's
//! `effectiveGasPrice`.
//!
//! ## Fallback
//! If RPC calls fail, falls back to the static estimates in the parent module.

use shared::rpc::EthClient;
use tracing::{debug, warn};

use super::calldata::{self, InteractionKind};
use super::{
    CHAIN_ARBITRUM, CHAIN_MAINNET,
    estimate_cost_arbitrum_wei, estimate_cost_mainnet_wei,
};

// ── ArbGasInfo precompile ────────────────────────────────────────────────────

/// Arbitrum ArbGasInfo precompile address (same on Arbitrum One and Nova).
const ARB_GAS_INFO_ADDRESS: &str = "0x000000000000000000000000000000000000006C";

/// `getL1BaseFeeEstimate()` selector — returns uint256 (L1 base fee in wei).
/// keccak256("getL1BaseFeeEstimate()") = 0xf5d6ded7...
const SELECTOR_GET_L1_BASE_FEE: &str = "0xf5d6ded7";

/// `getPricesInWei()` selector — returns (uint256,uint256,uint256,uint256,uint256,uint256).
/// The 6 returned values are:
///   [0] per-L2-tx wei cost
///   [1] per-L1-calldata-unit wei cost
///   [2] per-storage-allocation wei cost
///   [3] per-ArbGas-base wei cost
///   [4] per-ArbGas-congestion wei cost
///   [5] per-ArbGas-total wei cost
/// keccak256("getPricesInWei()") = 0x02199f34...
const SELECTOR_GET_PRICES_IN_WEI: &str = "0x02199f34";

// ── Cached oracle data ──────────────────────────────────────────────────────

/// Cached gas pricing data, fetched once per auction solve cycle.
#[derive(Debug, Clone)]
pub struct GasPrices {
    /// Current L1 base fee in wei (Arbitrum only). None on mainnet.
    pub l1_base_fee_wei: Option<u128>,
    /// Cost per L1 calldata byte in wei (Arbitrum only). None on mainnet.
    pub l1_calldata_byte_price_wei: Option<u128>,
    /// Per-L2-transaction fixed cost in wei (Arbitrum only).
    pub l2_per_tx_wei: Option<u128>,
    /// Current network gas price in wei (from eth_gasPrice).
    pub network_gas_price_wei: Option<u128>,
    /// Whether this data was fetched from RPC (true) or is using fallback (false).
    pub is_live: bool,
}

impl GasPrices {
    /// Create a fallback instance with no live data.
    pub fn fallback() -> Self {
        Self {
            l1_base_fee_wei: None,
            l1_calldata_byte_price_wei: None,
            l2_per_tx_wei: None,
            network_gas_price_wei: None,
            is_live: false,
        }
    }
}

// ── GasOracle ────────────────────────────────────────────────────────────────

/// Gas price oracle that fetches live pricing once per solve cycle.
///
/// Created at the start of each `solve()` call. Caches RPC results so that
/// all interactions within the same auction use consistent gas pricing.
///
/// Falls back to static estimates (from the parent `gas` module) if RPC
/// calls fail.
#[derive(Debug)]
pub struct GasOracle {
    chain_id: u64,
    /// Cached gas prices, populated by `refresh()`.
    prices: GasPrices,
    /// The auction's effective gas price (L2 gas price on Arbitrum).
    auction_gas_price_wei: u128,
}

impl GasOracle {
    /// Create a new oracle for the given chain. Does NOT fetch prices yet;
    /// call `refresh()` to populate live data.
    pub fn new(chain_id: u64, auction_gas_price_wei: u128) -> Self {
        Self {
            chain_id,
            prices: GasPrices::fallback(),
            auction_gas_price_wei,
        }
    }

    /// Fetch live gas prices from the RPC node.
    ///
    /// On Arbitrum: queries the ArbGasInfo precompile for L1 data costs.
    /// On Mainnet: queries `eth_gasPrice` for the current gas price.
    ///
    /// If any RPC call fails, logs a warning and falls back to static estimates.
    /// This method is safe to call and will never return an error.
    pub async fn refresh(&mut self, rpc: &EthClient) {
        match self.chain_id {
            CHAIN_ARBITRUM => self.refresh_arbitrum(rpc).await,
            CHAIN_MAINNET => self.refresh_mainnet(rpc).await,
            _ => {
                // Unknown chain — use static estimates
                debug!(chain_id = self.chain_id, "Unknown chain; using static gas estimates");
            }
        }
    }

    /// Refresh Arbitrum-specific gas prices from ArbGasInfo precompile.
    async fn refresh_arbitrum(&mut self, rpc: &EthClient) {
        // Fetch getPricesInWei() — returns 6 uint256 values
        match rpc.call(ARB_GAS_INFO_ADDRESS, SELECTOR_GET_PRICES_IN_WEI).await {
            Ok(hex_data) => {
                if let Some(prices) = parse_arb_prices_in_wei(&hex_data) {
                    self.prices.l2_per_tx_wei = Some(prices.0);
                    self.prices.l1_calldata_byte_price_wei = Some(prices.1);
                    self.prices.is_live = true;
                    debug!(
                        l2_per_tx = prices.0,
                        l1_per_byte = prices.1,
                        "Fetched live Arbitrum gas prices"
                    );
                } else {
                    warn!("Failed to parse ArbGasInfo.getPricesInWei() response");
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to fetch ArbGasInfo.getPricesInWei(); using static estimates");
            }
        }

        // Also fetch L1 base fee for logging/debugging
        match rpc.call(ARB_GAS_INFO_ADDRESS, SELECTOR_GET_L1_BASE_FEE).await {
            Ok(hex_data) => {
                if let Some(fee) = parse_uint256(&hex_data) {
                    self.prices.l1_base_fee_wei = Some(fee);
                    debug!(l1_base_fee_wei = fee, "Fetched L1 base fee");
                }
            }
            Err(e) => {
                debug!(error = %e, "Failed to fetch L1 base fee (non-critical)");
            }
        }
    }

    /// Refresh mainnet gas price from eth_gasPrice.
    async fn refresh_mainnet(&mut self, rpc: &EthClient) {
        // Use eth_call to get current gas price as sanity check
        match rpc_gas_price(rpc).await {
            Ok(price) => {
                self.prices.network_gas_price_wei = Some(price);
                self.prices.is_live = true;
                debug!(
                    network_gas_price = price,
                    auction_gas_price = self.auction_gas_price_wei,
                    "Fetched mainnet gas price"
                );
            }
            Err(e) => {
                warn!(error = %e, "Failed to fetch eth_gasPrice; using auction gas price");
            }
        }
    }

    /// Whether live data was successfully fetched.
    pub fn is_live(&self) -> bool {
        self.prices.is_live
    }

    /// Get the cached gas prices.
    pub fn prices(&self) -> &GasPrices {
        &self.prices
    }

    /// Estimate total gas cost in wei for a set of interactions.
    ///
    /// Uses live L1 data costs on Arbitrum when available, otherwise falls
    /// back to the static model.
    pub fn estimate_cost_wei(
        &self,
        interaction_count: usize,
        is_v3: bool,
    ) -> u128 {
        match self.chain_id {
            CHAIN_ARBITRUM => self.estimate_arbitrum(interaction_count, is_v3),
            _ => self.estimate_mainnet(interaction_count, is_v3),
        }
    }

    /// Estimate cost for a specific set of interaction kinds (more precise).
    pub fn estimate_cost_wei_detailed(
        &self,
        interactions: &[InteractionKind],
        order_count: usize,
        needs_approval: bool,
    ) -> u128 {
        match self.chain_id {
            CHAIN_ARBITRUM => self.estimate_arbitrum_detailed(interactions, order_count, needs_approval),
            _ => {
                // On mainnet, interaction kind doesn't affect L1 data cost
                let gas_price = self.effective_gas_price();
                let gas_units = super::estimate_gas_detailed(
                    order_count,
                    interactions.len(),
                    // Use V3 if any interaction is V3
                    interactions.iter().any(|k| matches!(k, InteractionKind::UniswapV3ExactInputSingle)),
                    needs_approval,
                ) as u128;
                gas_units * gas_price
            }
        }
    }

    /// Estimate Arbitrum cost using live L1 data or static fallback.
    fn estimate_arbitrum(&self, interaction_count: usize, is_v3: bool) -> u128 {
        if let Some(l1_byte_price) = self.prices.l1_calldata_byte_price_wei {
            // Dynamic model: L1 cost based on actual calldata size
            let kind = calldata::interaction_kind_from_v3_flag(is_v3);
            let interactions = vec![kind; interaction_count];
            let total_bytes = calldata::estimate_total_calldata(&interactions);
            let l1_cost = (total_bytes as u128) * l1_byte_price;

            // L2 execution cost
            let l2_gas_units = super::estimate_gas(interaction_count, is_v3) as u128;
            let l2_cost = l2_gas_units * self.auction_gas_price_wei;

            debug!(
                l1_bytes = total_bytes,
                l1_cost_wei = l1_cost,
                l2_gas = l2_gas_units,
                l2_cost_wei = l2_cost,
                total_wei = l1_cost + l2_cost,
                "Dynamic Arbitrum gas estimate"
            );

            l1_cost + l2_cost
        } else {
            // Fallback to static estimates
            estimate_cost_arbitrum_wei(
                interaction_count,
                is_v3,
                self.auction_gas_price_wei,
            )
        }
    }

    /// Detailed Arbitrum cost with specific interaction kinds.
    fn estimate_arbitrum_detailed(
        &self,
        interactions: &[InteractionKind],
        order_count: usize,
        needs_approval: bool,
    ) -> u128 {
        if let Some(l1_byte_price) = self.prices.l1_calldata_byte_price_wei {
            // Dynamic model
            let mut all_kinds: Vec<InteractionKind> = interactions.to_vec();
            if needs_approval {
                all_kinds.push(InteractionKind::Erc20Approve);
            }
            let total_bytes = calldata::estimate_total_calldata(&all_kinds);
            let l1_cost = (total_bytes as u128) * l1_byte_price;

            // L2 execution cost
            let is_v3 = interactions.iter().any(|k| matches!(k, InteractionKind::UniswapV3ExactInputSingle));
            let l2_gas_units = super::estimate_gas_detailed(
                order_count,
                interactions.len(),
                is_v3,
                needs_approval,
            ) as u128;
            let l2_cost = l2_gas_units * self.auction_gas_price_wei;

            l1_cost + l2_cost
        } else {
            // Fallback
            let is_v3 = interactions.iter().any(|k| matches!(k, InteractionKind::UniswapV3ExactInputSingle));
            estimate_cost_arbitrum_wei(
                interactions.len(),
                is_v3,
                self.auction_gas_price_wei,
            )
        }
    }

    /// Estimate mainnet cost, using network gas price if available as a sanity bound.
    fn estimate_mainnet(&self, interaction_count: usize, is_v3: bool) -> u128 {
        let gas_price = self.effective_gas_price();
        estimate_cost_mainnet_wei(interaction_count, is_v3, gas_price)
    }

    /// Return the effective gas price to use for L2 execution cost.
    ///
    /// On mainnet, uses the higher of auction price and network price (conservative).
    /// On Arbitrum, uses the auction's L2 gas price directly.
    fn effective_gas_price(&self) -> u128 {
        match self.chain_id {
            CHAIN_MAINNET => {
                // Use the higher of auction and network gas price (conservative estimate)
                match self.prices.network_gas_price_wei {
                    Some(network) => self.auction_gas_price_wei.max(network),
                    None => self.auction_gas_price_wei,
                }
            }
            _ => self.auction_gas_price_wei,
        }
    }

    // ── Per-DEX estimation (new API) ────────────────────────────────────────

    /// Estimate gas cost in wei for a solution with per-DEX swap types.
    ///
    /// Uses live L1 data prices on Arbitrum when available, otherwise falls
    /// back to a conservative static estimate.
    pub fn estimate_solution_cost(
        &self,
        swaps: &[crate::models::liquidity::PoolKind],
        order_count: usize,
        approval_count: usize,
    ) -> u128 {
        let gas_units = super::estimate_solution_gas(swaps, order_count, approval_count) as u128;
        let gas_price = self.effective_gas_price();

        match self.chain_id {
            CHAIN_ARBITRUM => {
                let l2_cost = gas_units * self.auction_gas_price_wei;

                // L1 data cost
                let l1_bytes: usize = calldata::estimate_calldata_bytes(
                    calldata::InteractionKind::SettlementOverhead,
                ) + swaps.iter().map(|k| {
                    calldata::estimate_calldata_bytes(calldata::interaction_kind_from_pool_kind(*k))
                }).sum::<usize>()
                + approval_count * calldata::estimate_calldata_bytes(calldata::InteractionKind::Erc20Approve);

                let l1_cost = if let Some(l1_byte_price) = self.prices.l1_calldata_byte_price_wei {
                    // Live L1 data cost
                    (l1_bytes as u128) * l1_byte_price
                } else {
                    // Conservative static estimate: 16 gas/byte * 30 gwei
                    (l1_bytes as u128) * 16 * 30_000_000_000u128
                };

                debug!(
                    l2_gas_units = gas_units,
                    l2_cost_wei = l2_cost,
                    l1_bytes = l1_bytes,
                    l1_cost_wei = l1_cost,
                    live_l1 = self.prices.l1_calldata_byte_price_wei.is_some(),
                    "Per-DEX Arbitrum gas estimate"
                );

                l2_cost + l1_cost
            }
            _ => {
                debug!(
                    gas_units = gas_units,
                    gas_price_wei = gas_price,
                    cost_wei = gas_units * gas_price,
                    "Per-DEX mainnet gas estimate"
                );
                gas_units * gas_price
            }
        }
    }

    /// Estimate gas cost for a multi-hop route through specific DEX pools.
    ///
    /// Each hop contributes its own swap gas plus routing overhead.
    pub fn estimate_route_cost(
        &self,
        hops: &[crate::models::liquidity::PoolKind],
    ) -> u128 {
        let gas_units = super::estimate_route_gas(hops) as u128;
        let gas_price = self.effective_gas_price();

        match self.chain_id {
            CHAIN_ARBITRUM => {
                let l2_cost = gas_units * self.auction_gas_price_wei;

                let l1_bytes: usize = calldata::estimate_calldata_bytes(
                    calldata::InteractionKind::SettlementOverhead,
                ) + hops.iter().map(|k| {
                    calldata::estimate_calldata_bytes(calldata::interaction_kind_from_pool_kind(*k))
                }).sum::<usize>();

                let l1_cost = if let Some(l1_byte_price) = self.prices.l1_calldata_byte_price_wei {
                    (l1_bytes as u128) * l1_byte_price
                } else {
                    (l1_bytes as u128) * 16 * 30_000_000_000u128
                };

                l2_cost + l1_cost
            }
            _ => gas_units * gas_price,
        }
    }

    /// Convert a gas cost in wei to a reference token amount.
    ///
    /// `reference_price` is the CoW driver's reference price for the token,
    /// expressed as "amount of token per 1e18 wei of ETH".
    pub fn gas_cost_in_token(&self, gas_cost_wei: u128, reference_price: u128) -> u128 {
        super::gas_cost_in_reference_token(gas_cost_wei, reference_price)
    }

    /// Return the chain ID this oracle was created for.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Return the auction gas price.
    pub fn auction_gas_price(&self) -> u128 {
        self.auction_gas_price_wei
    }
}

// ── Global cached gas prices (B.3 — background refresh) ─────────────────────

use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

/// Global cached gas prices, refreshed in the background every N seconds.
/// Avoids spending solve-window time on gas RPC calls.
static CACHED_GAS_PRICES: OnceLock<Arc<RwLock<GasPrices>>> = OnceLock::new();

fn cached_prices() -> &'static Arc<RwLock<GasPrices>> {
    CACHED_GAS_PRICES.get_or_init(|| Arc::new(RwLock::new(GasPrices::fallback())))
}

/// Read the latest cached gas prices (non-blocking).
///
/// Returns a clone of the cached prices. If the background task hasn't
/// populated them yet, returns fallback (static) prices.
pub async fn get_cached_prices() -> GasPrices {
    cached_prices().read().await.clone()
}

/// Create a `GasOracle` pre-populated with the latest cached prices.
///
/// This avoids the need to call `oracle.refresh(rpc)` during the solve window.
/// The oracle still falls back gracefully if cached prices are stale.
pub fn oracle_from_cache(chain_id: u64, auction_gas_price_wei: u128) -> GasOracle {
    // Try to read synchronously (non-blocking) — if locked, use fallback
    let prices = match cached_prices().try_read() {
        Ok(guard) => guard.clone(),
        Err(_) => GasPrices::fallback(),
    };
    let mut oracle = GasOracle::new(chain_id, auction_gas_price_wei);
    oracle.prices = prices;
    oracle
}

/// Background task that refreshes gas prices every `interval_secs` seconds.
///
/// Spawn once at startup:
/// ```rust,ignore
/// tokio::spawn(gas::oracle::run_gas_refresh(rpc.clone(), 42161, 15));
/// ```
pub async fn run_gas_refresh(rpc: shared::rpc::EthClient, chain_id: u64, interval_secs: u64) {
    use tokio::time::{Duration, sleep};

    tracing::info!(chain_id, interval_secs, "Gas price background refresh started");

    loop {
        let mut fresh = GasPrices::fallback();

        match chain_id {
            CHAIN_ARBITRUM => {
                // Fetch ArbGasInfo.getPricesInWei()
                match rpc.call(ARB_GAS_INFO_ADDRESS, SELECTOR_GET_PRICES_IN_WEI).await {
                    Ok(hex_data) => {
                        if let Some(prices) = parse_arb_prices_in_wei(&hex_data) {
                            fresh.l2_per_tx_wei = Some(prices.0);
                            fresh.l1_calldata_byte_price_wei = Some(prices.1);
                            fresh.is_live = true;
                            debug!(l2_per_tx = prices.0, l1_per_byte = prices.1, "Gas prices refreshed");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Gas refresh: failed to fetch ArbGasInfo");
                    }
                }
                // L1 base fee
                match rpc.call(ARB_GAS_INFO_ADDRESS, SELECTOR_GET_L1_BASE_FEE).await {
                    Ok(hex_data) => {
                        if let Some(fee) = parse_uint256(&hex_data) {
                            fresh.l1_base_fee_wei = Some(fee);
                        }
                    }
                    Err(_) => {} // non-critical
                }
            }
            CHAIN_MAINNET => {
                match rpc_gas_price(&rpc).await {
                    Ok(price) => {
                        fresh.network_gas_price_wei = Some(price);
                        fresh.is_live = true;
                        debug!(gas_price = price, "Mainnet gas price refreshed");
                    }
                    Err(e) => {
                        warn!(error = %e, "Gas refresh: failed to fetch eth_gasPrice");
                    }
                }
            }
            _ => {}
        }

        // Update cache
        {
            let mut guard = cached_prices().write().await;
            *guard = fresh;
        }

        sleep(Duration::from_secs(interval_secs)).await;
    }
}

// ── RPC helpers ─────────────────────────────────────────────────────────────

/// Parse the 6-tuple response from ArbGasInfo.getPricesInWei().
///
/// Returns (per_l2_tx, per_l1_calldata_byte) or None if parsing fails.
fn parse_arb_prices_in_wei(hex_data: &str) -> Option<(u128, u128)> {
    let data = hex_data.trim_start_matches("0x");
    // Each uint256 is 64 hex chars (32 bytes). We need at least 2 values.
    if data.len() < 128 {
        return None;
    }
    let per_l2_tx = parse_u128_from_hex_slot(data, 0)?;
    let per_l1_calldata_byte = parse_u128_from_hex_slot(data, 1)?;
    Some((per_l2_tx, per_l1_calldata_byte))
}

/// Parse a uint256 from a specific 32-byte slot in ABI-encoded data.
///
/// `slot` is 0-indexed. Each slot is 64 hex characters (32 bytes).
fn parse_u128_from_hex_slot(data: &str, slot: usize) -> Option<u128> {
    let start = slot * 64;
    let end = start + 64;
    if data.len() < end {
        return None;
    }
    let hex_slice = &data[start..end];
    // Take the last 32 hex chars (16 bytes = u128 max) to avoid overflow
    // For gas prices this is always sufficient
    let meaningful = &hex_slice[hex_slice.len().saturating_sub(32)..];
    u128::from_str_radix(meaningful, 16).ok()
}

/// Parse a single uint256 from ABI-encoded return data.
fn parse_uint256(hex_data: &str) -> Option<u128> {
    let data = hex_data.trim_start_matches("0x");
    if data.len() < 64 {
        return None;
    }
    parse_u128_from_hex_slot(data, 0)
}

/// Fetch eth_gasPrice from the RPC node. Returns gas price in wei.
async fn rpc_gas_price(rpc: &EthClient) -> anyhow::Result<u128> {
    // eth_gasPrice is not an eth_call; we need to use the RPC call method.
    // Since EthClient only exposes `call()` (eth_call), we use a workaround:
    // The EthClient has a private rpc_call method, but we can use the public
    // interface by making a raw call. For now, we'll use the block_number +
    // effective gas price approach, or we can extend EthClient.
    //
    // Simpler approach: use reqwest directly (EthClient stores the rpc_url).
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(&rpc.rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_gasPrice",
            "params": []
        }))
        .send()
        .await?
        .json()
        .await?;

    let hex = resp["result"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("eth_gasPrice returned non-string result"))?;
    let price = u128::from_str_radix(hex.trim_start_matches("0x"), 16)
        .map_err(|e| anyhow::anyhow!("Failed to parse gas price hex: {e}"))?;
    Ok(price)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parsing tests ────────────────────────────────────────────────────────

    #[test]
    fn parse_uint256_single_value() {
        // 30 gwei = 30_000_000_000 = 0x6FC23AC00
        let hex = "0x00000000000000000000000000000000000000000000000000000006fc23ac00";
        let val = parse_uint256(hex).expect("should parse");
        assert_eq!(val, 30_000_000_000u128);
    }

    #[test]
    fn parse_uint256_zero() {
        let hex = "0x0000000000000000000000000000000000000000000000000000000000000000";
        let val = parse_uint256(hex).expect("should parse");
        assert_eq!(val, 0);
    }

    #[test]
    fn parse_uint256_too_short() {
        assert!(parse_uint256("0x1234").is_none());
    }

    #[test]
    fn parse_arb_prices_two_slots() {
        // Slot 0: per_l2_tx = 100_000 (0x186A0)
        // Slot 1: per_l1_calldata_byte = 16_000_000 (0xF42400)
        let hex = concat!(
            "0x",
            "00000000000000000000000000000000000000000000000000000000000186a0", // slot 0
            "0000000000000000000000000000000000000000000000000000000000f42400", // slot 1
            "0000000000000000000000000000000000000000000000000000000000000000", // slot 2 (unused)
            "0000000000000000000000000000000000000000000000000000000000000000", // slot 3 (unused)
            "0000000000000000000000000000000000000000000000000000000000000000", // slot 4 (unused)
            "0000000000000000000000000000000000000000000000000000000000000000", // slot 5 (unused)
        );
        let (per_tx, per_byte) = parse_arb_prices_in_wei(hex).expect("should parse");
        assert_eq!(per_tx, 100_000);
        assert_eq!(per_byte, 16_000_000);
    }

    #[test]
    fn parse_arb_prices_too_short() {
        assert!(parse_arb_prices_in_wei("0x1234").is_none());
    }

    // ── GasOracle unit tests (no RPC) ────────────────────────────────────────

    #[test]
    fn oracle_fallback_uses_static_model() {
        let oracle = GasOracle::new(CHAIN_ARBITRUM, 100_000_000); // 0.1 gwei
        assert!(!oracle.is_live());
        let cost = oracle.estimate_cost_wei(1, false);
        // Should match the static model
        let static_cost = estimate_cost_arbitrum_wei(1, false, 100_000_000);
        assert_eq!(cost, static_cost, "Fallback should match static model");
    }

    #[test]
    fn oracle_mainnet_fallback() {
        let oracle = GasOracle::new(CHAIN_MAINNET, 30_000_000_000); // 30 gwei
        let cost = oracle.estimate_cost_wei(1, false);
        let static_cost = estimate_cost_mainnet_wei(1, false, 30_000_000_000);
        assert_eq!(cost, static_cost);
    }

    #[test]
    fn oracle_with_live_arb_prices() {
        let mut oracle = GasOracle::new(CHAIN_ARBITRUM, 100_000_000); // 0.1 gwei L2

        // Simulate live prices: 16M wei per L1 calldata byte (roughly 16 gas/byte * 1 gwei)
        oracle.prices.l1_calldata_byte_price_wei = Some(16_000_000);
        oracle.prices.l2_per_tx_wei = Some(100_000);
        oracle.prices.is_live = true;

        let cost = oracle.estimate_cost_wei(1, false);
        assert!(oracle.is_live());

        // Expected: L1 cost = (500 overhead + 196 V2) * 16M = 696 * 16M = 11,136,000,000
        // L2 cost = estimate_gas(1, false) * 0.1 gwei
        let l2_gas = super::super::estimate_gas(1, false) as u128;
        let expected_l2 = l2_gas * 100_000_000;
        let expected_l1 = 696u128 * 16_000_000;
        let expected_total = expected_l1 + expected_l2;

        assert_eq!(cost, expected_total, "Live cost should use dynamic L1 model");
    }

    #[test]
    fn oracle_live_vs_static_arbitrum_divergence() {
        // With low L1 gas prices, live should be cheaper than static
        let mut oracle_low = GasOracle::new(CHAIN_ARBITRUM, 100_000_000);
        oracle_low.prices.l1_calldata_byte_price_wei = Some(1_000_000); // ~1M wei/byte (very low L1)
        oracle_low.prices.is_live = true;

        let live_cost = oracle_low.estimate_cost_wei(1, false);
        let static_cost = estimate_cost_arbitrum_wei(1, false, 100_000_000);

        // At 1M wei/byte, L1 cost = 696 * 1M = ~696M wei = ~0.7 gwei equivalent
        // Static L1 surcharge = 144,000 gwei = 144,000,000,000,000 wei
        // Live should be MUCH cheaper
        assert!(
            live_cost < static_cost,
            "Low L1 price: live ({live_cost}) should be cheaper than static ({static_cost})"
        );
    }

    #[test]
    fn oracle_live_high_l1_more_expensive() {
        // With very high L1 gas prices, live should be more expensive than the conservative static estimate.
        // The static model uses a flat surcharge of ~144_000 gwei per V2 swap + 60_000 gwei overhead.
        // The live model uses (500+196 calldata bytes) * l1_byte_price.
        // At 500B wei/byte: live L1 = 696 * 500B = 348T wei
        // Static L1 = 144T + 60T = 204T wei
        let mut oracle = GasOracle::new(CHAIN_ARBITRUM, 100_000_000);
        oracle.prices.l1_calldata_byte_price_wei = Some(500_000_000_000); // 500B wei/byte
        oracle.prices.is_live = true;

        let live_cost = oracle.estimate_cost_wei(1, false);
        let static_cost = estimate_cost_arbitrum_wei(1, false, 100_000_000);

        assert!(
            live_cost > static_cost,
            "High L1 price: live ({live_cost}) should exceed static ({static_cost})"
        );
    }

    #[test]
    fn oracle_mainnet_uses_higher_gas_price() {
        let mut oracle = GasOracle::new(CHAIN_MAINNET, 20_000_000_000); // auction says 20 gwei
        oracle.prices.network_gas_price_wei = Some(25_000_000_000); // network says 25 gwei
        oracle.prices.is_live = true;

        let cost = oracle.estimate_cost_wei(1, false);
        // Should use 25 gwei (the higher of auction and network)
        let expected = estimate_cost_mainnet_wei(1, false, 25_000_000_000);
        assert_eq!(cost, expected, "Should use the higher gas price");
    }

    #[test]
    fn oracle_detailed_with_mixed_interactions() {
        let mut oracle = GasOracle::new(CHAIN_ARBITRUM, 100_000_000);
        oracle.prices.l1_calldata_byte_price_wei = Some(16_000_000);
        oracle.prices.is_live = true;

        let interactions = vec![
            InteractionKind::UniswapV2Swap,
            InteractionKind::UniswapV3ExactInputSingle,
        ];

        let cost = oracle.estimate_cost_wei_detailed(&interactions, 2, true);

        // L1: overhead(500) + V2(196) + V3(228) + approve(68) = 992 bytes
        // L1 cost = 992 * 16M = 15,872,000,000
        let expected_l1 = 992u128 * 16_000_000;
        assert!(cost > expected_l1, "Total cost should exceed L1 component");
    }

    // ── Per-DEX oracle tests ────────────────────────────────────────────────

    #[test]
    fn oracle_per_dex_mainnet() {
        use crate::models::liquidity::PoolKind;
        let oracle = GasOracle::new(CHAIN_MAINNET, 30_000_000_000); // 30 gwei

        let v2_cost = oracle.estimate_solution_cost(&[PoolKind::UniswapV2], 1, 0);
        let curve_cost = oracle.estimate_solution_cost(&[PoolKind::Curve], 1, 0);
        assert!(
            curve_cost > v2_cost,
            "Curve ({curve_cost}) should cost more gas than V2 ({v2_cost})"
        );
    }

    #[test]
    fn oracle_per_dex_arbitrum_live() {
        use crate::models::liquidity::PoolKind;
        let mut oracle = GasOracle::new(CHAIN_ARBITRUM, 100_000_000);
        oracle.prices.l1_calldata_byte_price_wei = Some(16_000_000);
        oracle.prices.is_live = true;

        let v2_cost = oracle.estimate_solution_cost(&[PoolKind::UniswapV2], 1, 0);
        let v3_cost = oracle.estimate_solution_cost(&[PoolKind::UniswapV3], 1, 0);
        // V3 should cost more (higher L2 gas + larger calldata = higher L1 cost)
        assert!(
            v3_cost > v2_cost,
            "V3 ({v3_cost}) should cost more than V2 ({v2_cost}) on Arbitrum"
        );
    }

    #[test]
    fn oracle_route_cost_increases_with_hops() {
        use crate::models::liquidity::PoolKind;
        let oracle = GasOracle::new(CHAIN_MAINNET, 30_000_000_000);

        let one_hop = oracle.estimate_route_cost(&[PoolKind::UniswapV2]);
        let two_hop = oracle.estimate_route_cost(&[PoolKind::UniswapV2, PoolKind::UniswapV2]);
        assert!(two_hop > one_hop, "Two hops should cost more than one");
    }

    #[test]
    fn oracle_gas_cost_in_token() {
        let oracle = GasOracle::new(CHAIN_MAINNET, 30_000_000_000);
        // WETH reference price = 1e18
        let eth_ref = 1_000_000_000_000_000_000u128;
        let gas_wei = 5_000_000_000_000_000u128; // 0.005 ETH
        let cost = oracle.gas_cost_in_token(gas_wei, eth_ref);
        assert_eq!(cost, gas_wei, "In ETH, cost should equal gas wei");
    }
}
