//! Chain-agnostic configuration system.
//!
//! Loads per-chain settings from TOML files in the `config/` directory.
//! Each chain has its own file (e.g. `config/mainnet.toml`, `config/arbitrum.toml`)
//! containing token addresses, DEX factory addresses, known pool addresses,
//! and gas parameters.
//!
//! The appropriate config is selected at startup based on the `CHAIN_ID` env var.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

// ── Error types ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ChainConfigError {
    #[error("No config file found for chain_id {0} (searched: {1})")]
    NotFound(u64, String),
    #[error("Failed to read config file {path}: {source}")]
    IoError {
        path: String,
        source: std::io::Error,
    },
    #[error("Failed to parse config file {path}: {source}")]
    ParseError {
        path: String,
        source: toml::de::Error,
    },
    #[error("Chain ID mismatch: file declares {declared}, but expected {expected}")]
    ChainIdMismatch { declared: u64, expected: u64 },
    #[error("Token symbol '{symbol}' referenced in pool '{pool}' not found in [tokens] table")]
    UnknownTokenSymbol { symbol: String, pool: String },
}

// ── TOML schema types ───────────────────────────────────────────────────────

/// Root of a chain config TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct ChainConfigFile {
    pub chain: ChainInfo,
    /// Token symbol -> address mapping.
    #[serde(default)]
    pub tokens: HashMap<String, String>,
    /// DEX configurations keyed by protocol name.
    #[serde(default)]
    pub dex: DexConfigs,
    /// Default discovery pairs (token symbol references).
    #[serde(default)]
    pub discovery_pairs: Vec<DiscoveryPair>,
    /// Gas estimation parameters.
    #[serde(default)]
    pub gas: GasConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChainInfo {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DexConfigs {
    pub uniswap_v2: Option<DexFactoryConfig>,
    pub sushiswap: Option<DexFactoryConfig>,
    pub uniswap_v3: Option<UniswapV3Config>,
    pub camelot_v2: Option<CamelotV2Config>,
    pub camelot_v3: Option<CamelotV3Config>,
    pub curve: Option<CurveConfig>,
    pub balancer_v2: Option<BalancerV2Config>,
    pub trader_joe_v21: Option<TraderJoeV21Config>,
    pub gmx_v2: Option<GmxV2Config>,
    pub dodo: Option<DodoConfig>,
    pub wombat: Option<WombatConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DexFactoryConfig {
    pub factory: String,
    #[serde(default)]
    pub router: Option<String>,
    pub fee_bps: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UniswapV3Config {
    pub factory: String,
    #[serde(default)]
    pub router: Option<String>,
    #[serde(default = "default_v3_fee_tiers")]
    pub fee_tiers: Vec<u32>,
}

fn default_v3_fee_tiers() -> Vec<u32> {
    vec![100, 500, 3000, 10000]
}

#[derive(Debug, Clone, Deserialize)]
pub struct CamelotV2Config {
    pub factory: String,
    #[serde(default)]
    pub router: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CamelotV3Config {
    pub factory: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TraderJoeV21Config {
    pub factory: String,
    #[serde(default)]
    pub router: Option<String>,
    /// Bin steps to scan for pools (defaults to common bin steps: 1, 5, 10, 15, 20, 25)
    #[serde(default = "default_tj_bin_steps")]
    pub bin_steps: Vec<u32>,
}

fn default_tj_bin_steps() -> Vec<u32> {
    vec![1, 5, 10, 15, 20, 25]
}

#[derive(Debug, Clone, Deserialize)]
pub struct GmxV2Config {
    pub reader: String,
    pub data_store: String,
    #[serde(default)]
    pub exchange_router: Option<String>,
    #[serde(default)]
    pub markets: Vec<GmxV2MarketConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GmxV2MarketConfig {
    pub name: String,
    pub address: String,
    pub long_token: String,
    pub short_token: String,
    pub index_token: String,
    #[serde(default = "default_gmx_v2_fee")]
    pub swap_fee_bps: u32,
}

fn default_gmx_v2_fee() -> u32 {
    7
}

#[derive(Debug, Clone, Deserialize)]
pub struct DodoConfig {
    /// DODO Proxy Router address
    pub router: String,
    /// Known DODO pools to load
    #[serde(default)]
    pub pools: Vec<DodoPoolConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DodoPoolConfig {
    pub name: String,
    pub address: String,
    /// Base token symbol (resolved against [tokens] table)
    pub base_token: String,
    /// Quote token symbol (resolved against [tokens] table)
    pub quote_token: String,
    /// Pool type: "dsp" (stable) or "dpp" (private)
    #[serde(rename = "type")]
    pub pool_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WombatConfig {
    /// Wombat Router address
    pub router: String,
    /// Known Wombat pool addresses
    #[serde(default)]
    pub pools: Vec<WombatPoolConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WombatPoolConfig {
    pub name: String,
    pub address: String,
    /// Token symbols in the pool (resolved against [tokens] table)
    pub tokens: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurveConfig {
    #[serde(default)]
    pub pools: Vec<CurvePoolConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurvePoolConfig {
    pub name: String,
    pub address: String,
    /// Token symbols (resolved against [tokens] table).
    pub tokens: Vec<String>,
    /// Pool type: "stable" or "crypto".
    #[serde(rename = "type")]
    pub pool_type: String,
    /// StableSwap amplification coefficient.
    /// Note: TOML doesn't support u128, so we deserialize as u64 and widen later.
    #[serde(default = "default_amp")]
    pub amp: u64,
    /// Swap fee in bps (typically 4 = 0.04%).
    #[serde(default = "default_curve_fee")]
    pub fee_bps: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BalancerV2Config {
    /// Vault address (same on all chains: 0xBA12222222228d8Ba445958a75a0704d566BF2C8)
    pub vault: String,
    #[serde(default)]
    pub pools: Vec<BalancerPoolConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BalancerPoolConfig {
    pub name: String,
    /// Pool contract address
    pub address: String,
    /// Balancer pool ID (32-byte hex)
    pub pool_id: String,
    /// Token symbols (resolved against [tokens] table)
    pub tokens: Vec<String>,
    /// Pool type: "weighted" or "stable"
    #[serde(rename = "type")]
    pub pool_type: String,
    /// Weights per token (required for weighted pools, ignored for stable)
    #[serde(default)]
    pub weights: Vec<f64>,
    /// Amplification coefficient A (required for stable pools)
    #[serde(default = "default_balancer_amp")]
    pub amp: u64,
    /// Swap fee as decimal (e.g. 0.003 = 0.3%)
    #[serde(default = "default_balancer_fee")]
    pub fee: f64,
}

fn default_balancer_amp() -> u64 {
    100
}

fn default_balancer_fee() -> f64 {
    0.003
}

fn default_amp() -> u64 {
    100
}

fn default_curve_fee() -> u32 {
    4
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryPair {
    pub token_a: String,
    pub token_b: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GasConfig {
    #[serde(default = "default_settlement_overhead")]
    pub settlement_overhead: u64,
    #[serde(default = "default_per_order")]
    pub per_order: u64,
    #[serde(default)]
    pub l1_surcharge_enabled: bool,
}

impl Default for GasConfig {
    fn default() -> Self {
        Self {
            settlement_overhead: default_settlement_overhead(),
            per_order: default_per_order(),
            l1_surcharge_enabled: false,
        }
    }
}

fn default_settlement_overhead() -> u64 {
    100_000
}

fn default_per_order() -> u64 {
    50_000
}

// ── Resolved config (all symbols expanded to addresses) ─────────────────────

/// A fully resolved chain configuration with token symbols expanded to addresses.
#[derive(Debug, Clone)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub chain_name: String,
    /// Token symbol -> address (checksummed).
    pub tokens: HashMap<String, String>,
    /// Reverse lookup: lowercase address -> symbol.
    pub address_to_symbol: HashMap<String, String>,
    pub dex: DexConfigs,
    /// Discovery pairs with addresses resolved.
    pub discovery_pairs: Vec<(String, String)>,
    pub gas: GasConfig,
    /// Curve pools with token symbols resolved to addresses.
    pub curve_pools: Vec<ResolvedCurvePool>,
    /// Balancer V2 pools with token symbols resolved to addresses.
    pub balancer_pools: Vec<ResolvedBalancerPool>,
    /// GMX V2 markets with token addresses resolved.
    pub gmx_v2_markets: Vec<ResolvedGmxV2Market>,
    /// DODO pools (addresses from config, state fetched on-chain).
    pub dodo_pools: Vec<DodoPoolConfig>,
    /// Wombat pools with token symbols resolved to addresses.
    pub wombat_pools: Vec<ResolvedWombatPool>,
}

/// A Curve pool with token addresses (not symbols).
#[derive(Debug, Clone)]
pub struct ResolvedCurvePool {
    pub name: String,
    pub address: String,
    pub tokens: Vec<String>,
    pub pool_type: String,
    pub amp: u128,
    pub fee_bps: u32,
}

/// A Balancer V2 pool with token addresses (not symbols).
#[derive(Debug, Clone)]
pub struct ResolvedBalancerPool {
    pub name: String,
    pub address: String,
    pub pool_id: String,
    pub tokens: Vec<String>,
    /// "weighted" or "stable"
    pub pool_type: String,
    /// Weights per token (only for weighted pools)
    pub weights: Vec<f64>,
    /// Amplification parameter (only for stable pools)
    pub amp: u128,
    /// Swap fee as a decimal
    pub fee: f64,
}

/// A GMX V2 market with token addresses resolved.
#[derive(Debug, Clone)]
pub struct ResolvedGmxV2Market {
    pub name: String,
    pub address: String,
    pub long_token: String,
    pub short_token: String,
    pub index_token: String,
    pub swap_fee_bps: u32,
}

/// A Wombat pool with token addresses resolved.
#[derive(Debug, Clone)]
pub struct ResolvedWombatPool {
    pub name: String,
    pub address: String,
    pub tokens: Vec<String>,
}

impl ChainConfig {
    /// Resolve a token symbol to its address.
    /// Returns the symbol itself if it looks like an address (starts with "0x").
    pub fn resolve_token<'a>(&'a self, symbol: &'a str) -> Option<&'a str> {
        // If it already looks like an address, return as-is.
        if symbol.starts_with("0x") || symbol.starts_with("0X") {
            return Some(symbol);
        }
        // Special case: "ETH" maps to the zero address sentinel.
        if symbol == "ETH" {
            return Some("0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE");
        }
        self.tokens.get(symbol).map(move |s| s.as_str())
    }

    /// Look up a token symbol by its address (case-insensitive).
    pub fn symbol_for_address(&self, addr: &str) -> Option<&str> {
        self.address_to_symbol
            .get(&addr.to_lowercase())
            .map(|s| s.as_str())
    }
}

// ── Loading logic ───────────────────────────────────────────────────────────

/// Map chain_id to the expected config filename.
fn config_filename(chain_id: u64) -> &'static str {
    match chain_id {
        1 => "mainnet.toml",
        42161 => "arbitrum.toml",
        5 => "goerli.toml",
        11155111 => "sepolia.toml",
        100 => "gnosis.toml",
        _ => "unknown.toml",
    }
}

/// Locate the `config/` directory.
///
/// Searches up from `start_dir` (typically the binary's working directory)
/// looking for a `config/` folder that contains TOML files.
fn find_config_dir(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    for _ in 0..5 {
        let candidate = dir.join("config");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Load and resolve a chain config from a TOML string.
pub fn parse_chain_config(toml_str: &str, expected_chain_id: u64) -> Result<ChainConfig, ChainConfigError> {
    let raw: ChainConfigFile = toml::from_str(toml_str).map_err(|e| ChainConfigError::ParseError {
        path: "<string>".into(),
        source: e,
    })?;

    if raw.chain.id != expected_chain_id {
        return Err(ChainConfigError::ChainIdMismatch {
            declared: raw.chain.id,
            expected: expected_chain_id,
        });
    }

    resolve_config(raw)
}

/// Load the chain config for the given chain_id from the filesystem.
///
/// Looks for `config/{filename}.toml` relative to the current working directory
/// or up to 5 parent directories.
pub fn load_chain_config(chain_id: u64) -> Result<ChainConfig, ChainConfigError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    load_chain_config_from(chain_id, &cwd)
}

/// Load chain config starting the directory search from `start_dir`.
pub fn load_chain_config_from(chain_id: u64, start_dir: &Path) -> Result<ChainConfig, ChainConfigError> {
    let filename = config_filename(chain_id);
    let config_dir = find_config_dir(start_dir).ok_or_else(|| {
        ChainConfigError::NotFound(chain_id, format!("no config/ directory found from {}", start_dir.display()))
    })?;

    let path = config_dir.join(filename);
    let toml_str = std::fs::read_to_string(&path).map_err(|e| ChainConfigError::IoError {
        path: path.display().to_string(),
        source: e,
    })?;

    let raw: ChainConfigFile = toml::from_str(&toml_str).map_err(|e| ChainConfigError::ParseError {
        path: path.display().to_string(),
        source: e,
    })?;

    if raw.chain.id != chain_id {
        return Err(ChainConfigError::ChainIdMismatch {
            declared: raw.chain.id,
            expected: chain_id,
        });
    }

    resolve_config(raw)
}

/// Resolve token symbols in pools and discovery pairs to actual addresses.
fn resolve_config(raw: ChainConfigFile) -> Result<ChainConfig, ChainConfigError> {
    let tokens = &raw.tokens;

    // Build reverse lookup: lowercase address -> symbol.
    let mut address_to_symbol = HashMap::new();
    for (sym, addr) in tokens {
        address_to_symbol.insert(addr.to_lowercase(), sym.clone());
    }

    // Resolve discovery pairs.
    let mut discovery_pairs = Vec::new();
    for pair in &raw.discovery_pairs {
        let addr_a = resolve_symbol(&pair.token_a, tokens, "discovery_pair")?;
        let addr_b = resolve_symbol(&pair.token_b, tokens, "discovery_pair")?;
        discovery_pairs.push((addr_a, addr_b));
    }

    // Resolve Curve pools.
    let mut curve_pools = Vec::new();
    if let Some(ref curve) = raw.dex.curve {
        for pool_cfg in &curve.pools {
            let mut resolved_tokens = Vec::new();
            for sym in &pool_cfg.tokens {
                let addr = resolve_symbol(sym, tokens, &pool_cfg.name)?;
                resolved_tokens.push(addr);
            }
            curve_pools.push(ResolvedCurvePool {
                name: pool_cfg.name.clone(),
                address: pool_cfg.address.clone(),
                tokens: resolved_tokens,
                pool_type: pool_cfg.pool_type.clone(),
                amp: pool_cfg.amp as u128,
                fee_bps: pool_cfg.fee_bps,
            });
        }
    }

    // Resolve Balancer V2 pools.
    let mut balancer_pools = Vec::new();
    if let Some(ref bal) = raw.dex.balancer_v2 {
        for pool_cfg in &bal.pools {
            let mut resolved_tokens = Vec::new();
            for sym in &pool_cfg.tokens {
                let addr = resolve_symbol(sym, tokens, &pool_cfg.name)?;
                resolved_tokens.push(addr);
            }
            balancer_pools.push(ResolvedBalancerPool {
                name: pool_cfg.name.clone(),
                address: pool_cfg.address.clone(),
                pool_id: pool_cfg.pool_id.clone(),
                tokens: resolved_tokens,
                pool_type: pool_cfg.pool_type.clone(),
                weights: pool_cfg.weights.clone(),
                amp: pool_cfg.amp as u128,
                fee: pool_cfg.fee,
            });
        }
    }

    // Resolve GMX V2 markets.
    let mut gmx_v2_markets = Vec::new();
    if let Some(ref gmx) = raw.dex.gmx_v2 {
        for mkt in &gmx.markets {
            let long_token = resolve_symbol(&mkt.long_token, tokens, &mkt.name)?;
            let short_token = resolve_symbol(&mkt.short_token, tokens, &mkt.name)?;
            let index_token = resolve_symbol(&mkt.index_token, tokens, &mkt.name)?;
            gmx_v2_markets.push(ResolvedGmxV2Market {
                name: mkt.name.clone(),
                address: mkt.address.clone(),
                long_token,
                short_token,
                index_token,
                swap_fee_bps: mkt.swap_fee_bps,
            });
        }
    }

    // Collect DODO pools (no symbol resolution needed — addresses are direct).
    let dodo_pools: Vec<DodoPoolConfig> = raw
        .dex
        .dodo
        .as_ref()
        .map(|d| d.pools.clone())
        .unwrap_or_default();

    // Resolve Wombat pools.
    let mut wombat_pools = Vec::new();
    if let Some(ref wombat) = raw.dex.wombat {
        for pool_cfg in &wombat.pools {
            let mut resolved_tokens = Vec::new();
            for sym in &pool_cfg.tokens {
                let addr = resolve_symbol(sym, tokens, &pool_cfg.name)?;
                resolved_tokens.push(addr);
            }
            wombat_pools.push(ResolvedWombatPool {
                name: pool_cfg.name.clone(),
                address: pool_cfg.address.clone(),
                tokens: resolved_tokens,
            });
        }
    }

    Ok(ChainConfig {
        chain_id: raw.chain.id,
        chain_name: raw.chain.name.clone(),
        tokens: raw.tokens,
        address_to_symbol,
        dex: raw.dex,
        gas: raw.gas,
        discovery_pairs,
        curve_pools,
        balancer_pools,
        gmx_v2_markets,
        dodo_pools,
        wombat_pools,
    })
}

/// Resolve a token symbol to an address using the tokens table.
/// Passes through raw addresses (0x-prefixed) unchanged.
/// "ETH" is resolved to the conventional sentinel address.
fn resolve_symbol(
    symbol: &str,
    tokens: &HashMap<String, String>,
    context: &str,
) -> Result<String, ChainConfigError> {
    if symbol.starts_with("0x") || symbol.starts_with("0X") {
        return Ok(symbol.to_string());
    }
    if symbol == "ETH" {
        return Ok("0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE".to_string());
    }
    tokens
        .get(symbol)
        .cloned()
        .ok_or_else(|| ChainConfigError::UnknownTokenSymbol {
            symbol: symbol.to_string(),
            pool: context.to_string(),
        })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const MAINNET_TOML: &str = r#"
[chain]
id = 1
name = "mainnet"

[tokens]
WETH  = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC  = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
USDT  = "0xdAC17F958D2ee523a2206206994597C13D831ec7"
DAI   = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
WBTC  = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
stETH = "0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84"
FRAX  = "0x853d955aCEf822Db058eb8505911ED77F175b99e"

[dex.uniswap_v2]
factory  = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
router   = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"
fee_bps  = 30

[dex.uniswap_v3]
factory  = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
fee_tiers = [100, 500, 3000, 10000]

[[dex.curve.pools]]
name    = "3pool"
address = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"
tokens  = ["DAI", "USDC", "USDT"]
type    = "stable"
amp     = 2000
fee_bps = 4

[[dex.curve.pools]]
name    = "stETH/ETH"
address = "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022"
tokens  = ["ETH", "stETH"]
type    = "stable"
amp     = 50
fee_bps = 4

[[discovery_pairs]]
token_a = "WETH"
token_b = "USDC"

[[discovery_pairs]]
token_a = "WETH"
token_b = "DAI"

[gas]
settlement_overhead  = 100000
per_order            = 50000
l1_surcharge_enabled = false
"#;

    const ARB_TOML: &str = r#"
[chain]
id = 42161
name = "arbitrum-one"

[tokens]
WETH   = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
USDC   = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
USDT   = "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9"
WBTC   = "0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f"
wstETH = "0x5979D7b546E38E9Ab8049524bC56bBC6aaD10F00"
FRAX   = "0x17FC002b466eEc40DaE837Fc4bE5c67993ddBd6F"

[dex.uniswap_v2]
factory  = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
fee_bps  = 30

[dex.uniswap_v3]
factory  = "0x1F98431c8aD98523631AE4a59f267346ea31F984"

[[dex.curve.pools]]
name    = "2pool"
address = "0x7f90122BF0700F9E7e1F688fe926940E8839F353"
tokens  = ["USDC", "USDT"]
type    = "stable"
amp     = 800
fee_bps = 4

[[dex.curve.pools]]
name    = "tricrypto"
address = "0x960ea3e3C7FB317332d990873d354E18d7645590"
tokens  = ["USDT", "WBTC", "WETH"]
type    = "crypto"
amp     = 1707629
fee_bps = 4

[[dex.curve.pools]]
name    = "wstETH/ETH"
address = "0x6eB2dc694eB516B16Dc9FBc678C60052BbdD7d80"
tokens  = ["wstETH", "WETH"]
type    = "stable"
amp     = 500
fee_bps = 4

[[discovery_pairs]]
token_a = "WETH"
token_b = "USDC"

[gas]
settlement_overhead  = 100000
per_order            = 50000
l1_surcharge_enabled = true
"#;

    #[test]
    fn parse_mainnet_config() {
        let cfg = parse_chain_config(MAINNET_TOML, 1).expect("should parse mainnet config");
        assert_eq!(cfg.chain_id, 1);
        assert_eq!(cfg.chain_name, "mainnet");
        assert!(cfg.tokens.contains_key("WETH"));
        assert!(cfg.tokens.contains_key("USDC"));
    }

    #[test]
    fn parse_arbitrum_config() {
        let cfg = parse_chain_config(ARB_TOML, 42161).expect("should parse arbitrum config");
        assert_eq!(cfg.chain_id, 42161);
        assert_eq!(cfg.chain_name, "arbitrum-one");
        assert_eq!(
            cfg.tokens.get("WETH").map(|s| s.as_str()),
            Some("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1")
        );
    }

    #[test]
    fn chain_id_mismatch_is_error() {
        let result = parse_chain_config(MAINNET_TOML, 42161);
        assert!(result.is_err());
        match result.unwrap_err() {
            ChainConfigError::ChainIdMismatch { declared, expected } => {
                assert_eq!(declared, 1);
                assert_eq!(expected, 42161);
            }
            other => panic!("Expected ChainIdMismatch, got: {other}"),
        }
    }

    #[test]
    fn curve_pools_resolved() {
        let cfg = parse_chain_config(MAINNET_TOML, 1).expect("should parse");
        assert_eq!(cfg.curve_pools.len(), 2);

        let pool_3 = &cfg.curve_pools[0];
        assert_eq!(pool_3.name, "3pool");
        assert_eq!(pool_3.tokens.len(), 3);
        // DAI should be resolved to its address
        assert_eq!(pool_3.tokens[0], "0x6B175474E89094C44Da98b954EedeAC495271d0F");

        let steth_pool = &cfg.curve_pools[1];
        assert_eq!(steth_pool.name, "stETH/ETH");
        // ETH should resolve to sentinel
        assert_eq!(steth_pool.tokens[0], "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE");
    }

    #[test]
    fn discovery_pairs_resolved() {
        let cfg = parse_chain_config(MAINNET_TOML, 1).expect("should parse");
        assert_eq!(cfg.discovery_pairs.len(), 2);
        assert_eq!(cfg.discovery_pairs[0].0, "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        assert_eq!(cfg.discovery_pairs[0].1, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    }

    #[test]
    fn gas_config_parsed() {
        let cfg = parse_chain_config(ARB_TOML, 42161).expect("should parse");
        assert!(cfg.gas.l1_surcharge_enabled);
        assert_eq!(cfg.gas.settlement_overhead, 100_000);
    }

    #[test]
    fn unknown_token_symbol_is_error() {
        let bad_toml = r#"
[chain]
id = 1
name = "test"

[tokens]
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

[[dex.curve.pools]]
name    = "bad-pool"
address = "0xDEAD"
tokens  = ["WETH", "NONEXISTENT"]
type    = "stable"

[gas]
"#;
        let result = parse_chain_config(bad_toml, 1);
        assert!(result.is_err());
        match result.unwrap_err() {
            ChainConfigError::UnknownTokenSymbol { symbol, pool } => {
                assert_eq!(symbol, "NONEXISTENT");
                assert_eq!(pool, "bad-pool");
            }
            other => panic!("Expected UnknownTokenSymbol, got: {other}"),
        }
    }

    #[test]
    fn resolve_token_passthrough_address() {
        let cfg = parse_chain_config(MAINNET_TOML, 1).expect("should parse");
        assert_eq!(
            cfg.resolve_token("0xDEADBEEF"),
            Some("0xDEADBEEF")
        );
    }

    #[test]
    fn resolve_token_eth_sentinel() {
        let cfg = parse_chain_config(MAINNET_TOML, 1).expect("should parse");
        assert_eq!(
            cfg.resolve_token("ETH"),
            Some("0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE")
        );
    }

    #[test]
    fn address_to_symbol_lookup() {
        let cfg = parse_chain_config(MAINNET_TOML, 1).expect("should parse");
        assert_eq!(
            cfg.symbol_for_address("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),
            Some("WETH")
        );
        // Non-existent address
        assert_eq!(cfg.symbol_for_address("0xDEAD"), None);
    }

    #[test]
    fn arbitrum_curve_pools_resolved() {
        let cfg = parse_chain_config(ARB_TOML, 42161).expect("should parse");
        assert_eq!(cfg.curve_pools.len(), 3);

        let pool_2 = &cfg.curve_pools[0];
        assert_eq!(pool_2.name, "2pool");
        assert_eq!(pool_2.tokens.len(), 2);
        assert_eq!(pool_2.tokens[0], "0xaf88d065e77c8cC2239327C5EDb3A432268e5831");
        assert_eq!(pool_2.tokens[1], "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9");

        let tricrypto = &cfg.curve_pools[1];
        assert_eq!(tricrypto.name, "tricrypto");
        assert_eq!(tricrypto.tokens.len(), 3);
    }

    #[test]
    fn default_gas_config() {
        let minimal_toml = r#"
[chain]
id = 1
name = "test"

[tokens]

[gas]
"#;
        let cfg = parse_chain_config(minimal_toml, 1).expect("should parse");
        assert_eq!(cfg.gas.settlement_overhead, 100_000);
        assert_eq!(cfg.gas.per_order, 50_000);
        assert!(!cfg.gas.l1_surcharge_enabled);
    }

    #[test]
    fn dex_config_v3_defaults() {
        let toml_str = r#"
[chain]
id = 1
name = "test"

[tokens]

[dex.uniswap_v3]
factory = "0x1234"

[gas]
"#;
        let cfg = parse_chain_config(toml_str, 1).expect("should parse");
        let v3 = cfg.dex.uniswap_v3.as_ref().expect("v3 should exist");
        assert_eq!(v3.fee_tiers, vec![100, 500, 3000, 10000]);
    }
}
