use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::token::TokenAddress;

/// Which DEX/protocol this pool belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolKind {
    UniswapV2,
    Sushiswap,
    UniswapV3,
    Balancer,
    BalancerWeighted,
    BalancerStable,
    Curve,
    CamelotV2,
    CamelotV3,
    GmxV2,
    Dodo,
    Wombat,
    TraderJoeV21,
}

/// Uniswap V2 / Sushiswap constant-product pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniswapV2Pool {
    pub address: String,
    pub kind: PoolKind,
    pub token0: TokenAddress,
    pub token1: TokenAddress,
    /// Reserve of token0 (raw integer string)
    pub reserve0: String,
    /// Reserve of token1 (raw integer string)
    pub reserve1: String,
    /// Fee in basis points (30 = 0.3%)
    pub fee_bps: u32,
}

/// An initialized tick in a Uniswap V3 pool with its net liquidity delta.
///
/// When the current tick crosses this tick moving left-to-right (price increasing),
/// `liquidity_net` is *added* to the active liquidity. Moving right-to-left
/// (price decreasing), it is *subtracted*.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3Tick {
    /// The tick index (must be a multiple of tickSpacing for the fee tier).
    pub index: i32,
    /// Signed net liquidity that becomes active/inactive at this tick boundary.
    pub liquidity_net: i128,
}

/// Uniswap V3 concentrated liquidity pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniswapV3Pool {
    pub address: String,
    pub token0: TokenAddress,
    pub token1: TokenAddress,
    /// sqrtPriceX96 (raw integer string)
    pub sqrt_price_x96: String,
    /// Current tick
    pub tick: i32,
    /// Active liquidity
    pub liquidity: String,
    /// Fee tier in hundredths of a bip (e.g. 3000 = 0.3%)
    pub fee: u32,
    /// Initialized ticks with their liquidityNet values, sorted ascending by index.
    /// When `None`, only the simplified single-tick approximation is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticks: Option<Vec<V3Tick>>,
}

/// Camelot V2 constant-product pool with directional fees.
///
/// Unlike Uniswap V2, Camelot V2 has per-pair fees that differ by swap direction
/// (token0→token1 vs token1→token0). Also supports stable pairs (x³y+xy³=k).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CamelotV2Pool {
    pub address: String,
    pub token0: TokenAddress,
    pub token1: TokenAddress,
    /// Reserve of token0 (raw integer string)
    pub reserve0: String,
    /// Reserve of token1 (raw integer string)
    pub reserve1: String,
    /// Fee in basis points for token0 → token1 swaps
    pub fee_token0_to_token1: u32,
    /// Fee in basis points for token1 → token0 swaps
    pub fee_token1_to_token0: u32,
    /// Whether this is a stable pair (uses x³y+xy³=k instead of xy=k)
    pub is_stable: bool,
}

/// Camelot V3 (Algebra) concentrated liquidity pool with dynamic fees.
///
/// This is an Algebra fork, NOT a Uniswap V3 fork. Key differences:
/// - Dynamic fees based on volatility (not fixed fee tiers)
/// - Single pool per pair (no fee tier selection)
/// - Uses `globalState()` instead of `slot0()` to get price + fee
/// - Adaptive tick spacing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CamelotV3Pool {
    pub address: String,
    pub token0: TokenAddress,
    pub token1: TokenAddress,
    /// sqrtPriceX96 (raw integer string)
    pub sqrt_price_x96: String,
    /// Current tick
    pub tick: i32,
    /// Active liquidity (raw integer string)
    pub liquidity: String,
    /// Current dynamic fee in hundredths of a bip (e.g. 3000 = 0.3%)
    /// This fee changes based on volatility, unlike Uniswap V3's fixed tiers.
    pub fee: u32,
}

/// Curve StableSwap pool stored in the pool registry.
///
/// Contains the full pool state needed for offline quote computation
/// via the StableSwap invariant (see `liquidity::curve::get_dy`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurveStablePool {
    pub address: String,
    /// Ordered token addresses (index matches Curve's `coins[i]`).
    pub tokens: Vec<TokenAddress>,
    /// Current balances in raw token units (decimal string, like "1000000").
    pub balances: Vec<String>,
    /// Amplification coefficient (A).
    pub amp: u128,
    /// Swap fee in basis points (4 = 0.04%).
    pub fee_bps: u32,
    /// Decimal normalization rates: `10^(18 - decimals_i)`.
    pub rates: Vec<u128>,
    /// Pool type identifier ("stable" or "crypto").
    pub pool_type: String,
}

/// Balancer V2 weighted pool stored in the pool registry.
///
/// Uses the generalised constant-product invariant: product of (balance_i ^ weight_i) = constant.
/// Swaps go through the Vault contract at `0xBA12222222228d8Ba445958a75a0704d566BF2C8`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalancerV2WeightedPool {
    /// Pool contract address
    pub address: String,
    /// Balancer pool ID (32-byte hex, identifies pool in Vault calls)
    pub pool_id: String,
    /// Ordered list of token addresses
    pub tokens: Vec<TokenAddress>,
    /// Balances per token (same order as `tokens`), in raw token units (decimal string)
    pub balances: Vec<String>,
    /// Weights per token as fractions summing to 1.0 (e.g. [0.8, 0.2] for 80/20)
    pub weights: Vec<f64>,
    /// Swap fee as a decimal (e.g. 0.003 = 0.3%)
    pub fee: f64,
}

/// Balancer V2 stable pool stored in the pool registry.
///
/// Uses the StableSwap invariant (same as Curve) for pegged assets.
/// Swaps go through the Vault contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalancerV2StablePool {
    /// Pool contract address
    pub address: String,
    /// Balancer pool ID (32-byte hex)
    pub pool_id: String,
    /// Ordered list of token addresses
    pub tokens: Vec<TokenAddress>,
    /// Balances per token in raw token units (decimal string)
    pub balances: Vec<String>,
    /// Amplification parameter A (typical range: 100-10000)
    pub amp: u128,
    /// Swap fee as a decimal (e.g. 0.0004 = 0.04%)
    pub fee: f64,
}

/// GMX V2 market pool on Arbitrum.
///
/// GMX V2 uses oracle-based pricing with pool impact factors.
/// Swaps are priced as: output = input * price_ratio * (1 - fee) * (1 - price_impact)
/// where price_ratio comes from oracle prices and price_impact depends on pool imbalance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmxV2Pool {
    /// Market address (e.g. ETH/USD market)
    pub address: String,
    /// Long token (e.g. WETH)
    pub long_token: TokenAddress,
    /// Short token (e.g. USDC)
    pub short_token: TokenAddress,
    /// Index token (the token whose price the market tracks, e.g. WETH for ETH/USD)
    pub index_token: TokenAddress,
    /// Long token pool amount (raw integer string)
    pub long_token_amount: String,
    /// Short token pool amount (raw integer string)
    pub short_token_amount: String,
    /// Swap fee factor (basis points, e.g. 5 = 0.05%)
    pub swap_fee_bps: u32,
    /// Swap impact factor for positive price impact (scaled, stored as f64 string)
    pub swap_impact_factor_positive: String,
    /// Swap impact factor for negative price impact (scaled, stored as f64 string)
    pub swap_impact_factor_negative: String,
    /// Oracle price of long token in USD (f64 string, e.g. "3500.50")
    pub long_token_price_usd: String,
    /// Oracle price of short token in USD (f64 string, e.g. "1.0")
    pub short_token_price_usd: String,
}

/// DODO Proactive Market Maker (PMM) pool.
///
/// DODO uses a unique PMM algorithm that provides better liquidity near the oracle
/// price. The formula: price = i * (1 - k + k * B0²/B²) where:
/// - i = oracle price (guide price)
/// - k = slippage factor (0 = constant price, 1 = constant product)
/// - B0 = target base token amount
/// - B = current base token amount
///
/// Pool types on Arbitrum: DSP (DODO Stable Pool), DPP (DODO Private Pool).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DodoPool {
    pub address: String,
    /// Base token (the token whose price is quoted)
    pub base_token: TokenAddress,
    /// Quote token (the token used to quote the base)
    pub quote_token: TokenAddress,
    /// Oracle / guide price i (scaled to 18 decimals, raw integer string)
    pub i: String,
    /// Slippage factor k (scaled to 18 decimals, raw integer string).
    /// k=0 means constant price, k=1e18 means constant product (xy=k).
    pub k: String,
    /// Current base token reserve (raw integer string)
    pub base_reserve: String,
    /// Current quote token reserve (raw integer string)
    pub quote_reserve: String,
    /// Target base token amount B0 (raw integer string)
    pub base_target: String,
    /// Target quote token amount Q0 (raw integer string)
    pub quote_target: String,
    /// Base token decimals
    pub base_decimals: u8,
    /// Quote token decimals
    pub quote_decimals: u8,
    /// LP fee rate (scaled to 18 decimals, raw integer string, e.g. "3000000000000000" = 0.3%)
    pub lp_fee_rate: String,
    /// Pool type: "dsp" (stable) or "dpp" (private)
    pub pool_type: String,
}

/// Wombat Exchange single-sided stableswap pool.
///
/// Wombat uses a coverage-ratio model where each asset has:
/// - cash: actual tokens in the pool
/// - liability: LP deposit obligations
/// - coverage ratio r = cash / liability
///
/// Swap pricing depends on how the swap affects each asset's coverage ratio.
/// Better ratios (closer to 1.0) get better prices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WombatPool {
    pub address: String,
    /// Asset addresses in the pool (e.g. USDC, USDT, DAI)
    pub tokens: Vec<TokenAddress>,
    /// Cash amounts per token (raw integer strings, same order as `tokens`)
    pub cash: Vec<String>,
    /// Liability amounts per token (raw integer strings, same order as `tokens`)
    pub liability: Vec<String>,
    /// Token decimals (same order as `tokens`)
    pub decimals: Vec<u8>,
    /// Amplification factor A (scaled to 18 decimals, raw integer string)
    pub amp_factor: String,
    /// Haircut rate (scaled to 18 decimals, raw integer string, e.g. "200000000000000" = 0.02%)
    pub haircut_rate: String,
    /// Swap fee (basis points, e.g. 1 = 0.01%)
    pub fee_bps: u32,
}

/// Trader Joe V2.1 (Liquidity Book) bin-based AMM pool.
///
/// Trader Joe V2.1 uses "Liquidity Book" — discrete price bins instead of
/// continuous ticks. Each bin has a fixed price; liquidity is concentrated
/// in specific bins. The active bin contains the current price.
///
/// Key differences from Uniswap V3:
/// - Discrete bins instead of continuous ticks
/// - Bin step = price increment between bins (in basis points)
/// - Swap traverses bins: consume liquidity in active bin, move to next
/// - `getSwapOut(pair, amountIn, swapForY)` returns (amountInLeft, amountOut, fee)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderJoeV21Pool {
    /// LBPair contract address
    pub address: String,
    pub token_x: TokenAddress,
    pub token_y: TokenAddress,
    /// Bin step in basis points (e.g. 15 = 0.15% price increment per bin)
    pub bin_step: u32,
    /// Active bin ID (the bin containing the current price)
    pub active_bin_id: u32,
    /// Reserve of token X in the active bin (raw integer string)
    pub reserve_x: String,
    /// Reserve of token Y in the active bin (raw integer string)
    pub reserve_y: String,
    /// Total fee in basis points (base fee + variable fee)
    pub total_fee_bps: u32,
    /// Base fee in basis points
    pub base_fee_bps: u32,
    /// Variable fee in basis points (depends on volatility)
    pub variable_fee_bps: u32,
}

/// A generic liquidity source the solver can use
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiquiditySource {
    UniswapV2(UniswapV2Pool),
    UniswapV3(UniswapV3Pool),
    CamelotV2(CamelotV2Pool),
    CamelotV3(CamelotV3Pool),
    CurveStable(CurveStablePool),
    BalancerWeighted(BalancerV2WeightedPool),
    BalancerStable(BalancerV2StablePool),
    GmxV2(GmxV2Pool),
    TraderJoeV21(TraderJoeV21Pool),
    Dodo(DodoPool),
    Wombat(WombatPool),
}

impl LiquiditySource {
    pub fn address(&self) -> &str {
        match self {
            LiquiditySource::UniswapV2(p) => &p.address,
            LiquiditySource::UniswapV3(p) => &p.address,
            LiquiditySource::CamelotV2(p) => &p.address,
            LiquiditySource::CamelotV3(p) => &p.address,
            LiquiditySource::CurveStable(p) => &p.address,
            LiquiditySource::BalancerWeighted(p) => &p.address,
            LiquiditySource::BalancerStable(p) => &p.address,
            LiquiditySource::GmxV2(p) => &p.address,
            LiquiditySource::TraderJoeV21(p) => &p.address,
            LiquiditySource::Dodo(p) => &p.address,
            LiquiditySource::Wombat(p) => &p.address,
        }
    }

    /// Return all token addresses this pool supports.
    pub fn tokens(&self) -> Vec<&str> {
        match self {
            LiquiditySource::UniswapV2(p) => vec![&p.token0, &p.token1],
            LiquiditySource::UniswapV3(p) => vec![&p.token0, &p.token1],
            LiquiditySource::CamelotV2(p) => vec![&p.token0, &p.token1],
            LiquiditySource::CamelotV3(p) => vec![&p.token0, &p.token1],
            LiquiditySource::CurveStable(p) => p.tokens.iter().map(|t| t.as_str()).collect(),
            LiquiditySource::BalancerWeighted(p) => p.tokens.iter().map(|t| t.as_str()).collect(),
            LiquiditySource::BalancerStable(p) => p.tokens.iter().map(|t| t.as_str()).collect(),
            LiquiditySource::GmxV2(p) => vec![&p.long_token, &p.short_token],
            LiquiditySource::TraderJoeV21(p) => vec![&p.token_x, &p.token_y],
            LiquiditySource::Dodo(p) => vec![&p.base_token, &p.quote_token],
            LiquiditySource::Wombat(p) => p.tokens.iter().map(|t| t.as_str()).collect(),
        }
    }

    /// Return the pool kind.
    pub fn kind(&self) -> PoolKind {
        match self {
            LiquiditySource::UniswapV2(p) => p.kind,
            LiquiditySource::UniswapV3(_) => PoolKind::UniswapV3,
            LiquiditySource::CamelotV2(_) => PoolKind::CamelotV2,
            LiquiditySource::CamelotV3(_) => PoolKind::CamelotV3,
            LiquiditySource::CurveStable(_) => PoolKind::Curve,
            LiquiditySource::BalancerWeighted(_) => PoolKind::BalancerWeighted,
            LiquiditySource::BalancerStable(_) => PoolKind::BalancerStable,
            LiquiditySource::GmxV2(_) => PoolKind::GmxV2,
            LiquiditySource::TraderJoeV21(_) => PoolKind::TraderJoeV21,
            LiquiditySource::Dodo(_) => PoolKind::Dodo,
            LiquiditySource::Wombat(_) => PoolKind::Wombat,
        }
    }
}

// ── Auction-format liquidity (what the CoW driver sends in POST /solve) ────────

/// Balance entry inside a liquidity pool's token map
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityTokenBalance {
    pub balance: String,
}

pub type LiquidityTokenMap = HashMap<TokenAddress, LiquidityTokenBalance>;

/// Constant-product pool (Uniswap V2 / Sushiswap style) as received from the driver
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstantProductPool {
    pub tokens: LiquidityTokenMap,
    /// Fee as a decimal string (e.g. "0.003" for 0.3%)
    pub fee: String,
    pub id: String,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
    #[serde(default)]
    pub gas_estimate: String,
}

/// Balancer weighted-product pool as received from the driver (Sprint 3: add per-token weights)
pub type WeightedProductPool = ConstantProductPool;

/// Stable pool (Curve / Balancer Stable) as received from the driver (Sprint 3: add amplification)
pub type StablePool = ConstantProductPool;

/// Concentrated-liquidity pool (Uniswap V3 style) as received from the driver
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcentratedLiquidityPool {
    /// V3 tokens as a simple array of addresses (NOT a map like V2).
    /// The CoW driver sends: "tokens": ["0xtoken0", "0xtoken1"]
    pub tokens: Vec<String>,
    /// Fee as a decimal string (e.g. "0.0005" for 0.05%)
    pub fee: String,
    pub id: String,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
    /// sqrtPriceX96 as a decimal string
    pub sqrt_price: String,
    /// Active liquidity as a decimal string
    pub liquidity: String,
    /// Current tick
    pub tick: i32,
    #[serde(default)]
    pub gas_estimate: String,
    /// Tick liquidity deltas — map from tick index to signed liquidity change.
    /// Sent by the CoW driver for concentrated liquidity pools.
    /// When present, enables full tick-traversal swap math instead of approximation.
    #[serde(default)]
    pub liquidity_net: Option<std::collections::HashMap<String, String>>,
}

impl ConcentratedLiquidityPool {
    /// Get token0 address (first in the sorted pair)
    pub fn token0(&self) -> &str {
        self.tokens.first().map(|s| s.as_str()).unwrap_or("")
    }
    /// Get token1 address (second in the sorted pair)
    pub fn token1(&self) -> &str {
        self.tokens.get(1).map(|s| s.as_str()).unwrap_or("")
    }
    /// Check if this pool contains both tokens of an order
    pub fn has_pair(&self, sell_token: &str, buy_token: &str) -> bool {
        let sell_lc = sell_token.to_lowercase();
        let buy_lc = buy_token.to_lowercase();
        let t0 = self.token0().to_lowercase();
        let t1 = self.token1().to_lowercase();
        (t0 == sell_lc && t1 == buy_lc) || (t0 == buy_lc && t1 == sell_lc)
    }
}

/// Liquidity source from the CoW driver auction payload (`liquidity` array).
///
/// The CoW driver may send liquidity types we don't handle (e.g. LimitOrder, CoWAmm).
/// We deserialize the `liquidity` array leniently via `deserialize_liquidity_vec`
/// so unknown types are silently skipped instead of crashing the entire auction parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Liquidity {
    ConstantProduct(ConstantProductPool),
    WeightedProduct(WeightedProductPool),
    Stable(StablePool),
    ConcentratedLiquidity(ConcentratedLiquidityPool),
}

/// Deserialize a Vec of Liquidity, silently skipping items with unknown `kind`.
/// This prevents one unknown pool type from crashing the entire auction parse.
pub fn deserialize_liquidity_vec<'de, D>(deserializer: D) -> Result<Vec<Liquidity>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::DeserializeSeed;
    let raw: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    let total = raw.len();
    let mut result = Vec::with_capacity(raw.len());
    let mut skipped_kinds: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for item in raw {
        let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
        match serde_json::from_value::<Liquidity>(item) {
            Ok(l) => result.push(l),
            Err(_) => {
                *skipped_kinds.entry(kind).or_insert(0) += 1;
            }
        }
    }
    if !skipped_kinds.is_empty() {
        tracing::warn!(
            total = total,
            parsed = result.len(),
            skipped = total - result.len(),
            kinds = ?skipped_kinds,
            "Liquidity deserialization: dropped unknown pool types"
        );
    }
    Ok(result)
}

impl Liquidity {
    pub fn id(&self) -> &str {
        match self {
            Liquidity::ConstantProduct(p) => &p.id,
            Liquidity::WeightedProduct(p) => &p.id,
            Liquidity::Stable(p) => &p.id,
            Liquidity::ConcentratedLiquidity(p) => &p.id,
        }
    }

    pub fn address(&self) -> &str {
        match self {
            Liquidity::ConstantProduct(p) => &p.address,
            Liquidity::WeightedProduct(p) => &p.address,
            Liquidity::Stable(p) => &p.address,
            Liquidity::ConcentratedLiquidity(p) => &p.address,
        }
    }
}
