use std::collections::HashMap;

use shared::rpc::EthClient;
use tracing::{debug, info, warn};

use crate::chain_config::ChainConfig;
use crate::liquidity::{balancer_v2, camelot_v2, camelot_v3, curve, dodo, gmx_v2, trader_joe, uniswap_v2, uniswap_v3, wombat};
use crate::models::liquidity::{LiquiditySource, PoolKind, UniswapV2Pool, UniswapV3Pool};

// ── Chain-specific constants ──────────────────────────────────────────────────

/// CoW Protocol Settlement contract — same address on all supported chains.
pub const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

// ── Mainnet token addresses ───────────────────────────────────────────────────

pub const MAINNET_WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
pub const MAINNET_USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
pub const MAINNET_USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
pub const MAINNET_DAI: &str = "0x6B175474E89094C44Da98b954EedeAC495271d0F";
pub const MAINNET_WBTC: &str = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599";

/// Mainnet default discovery pairs.
pub const MAINNET_DEFAULT_PAIRS: &[(&str, &str)] = &[
    (MAINNET_WETH, MAINNET_USDC),
    (MAINNET_WETH, MAINNET_USDT),
    (MAINNET_WETH, MAINNET_DAI),
    (MAINNET_WETH, MAINNET_WBTC),
    (MAINNET_USDC, MAINNET_USDT),
    (MAINNET_USDC, MAINNET_DAI),
];

// ── Arbitrum One (42161) token addresses ─────────────────────────────────────

/// WETH on Arbitrum (same wrapped Ether, different address)
pub const ARB_WETH: &str = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1";
/// USDC (native, not bridged) on Arbitrum
pub const ARB_USDC: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
/// USDC.e (bridged from mainnet) on Arbitrum
pub const ARB_USDC_E: &str = "0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8";
/// USDT on Arbitrum
pub const ARB_USDT: &str = "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9";
/// DAI on Arbitrum
pub const ARB_DAI: &str = "0xDA10009cBd5D07dd0CeCc66161FC93D7c9000da1";
/// WBTC on Arbitrum
pub const ARB_WBTC: &str = "0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f";
/// ARB governance token
pub const ARB_ARB: &str = "0x912CE59144191C1204E64559FE8253a0e49E6548";

/// Arbitrum default discovery pairs.
pub const ARB_DEFAULT_PAIRS: &[(&str, &str)] = &[
    (ARB_WETH, ARB_USDC),
    (ARB_WETH, ARB_USDC_E),
    (ARB_WETH, ARB_USDT),
    (ARB_WETH, ARB_DAI),
    (ARB_WETH, ARB_WBTC),
    (ARB_USDC, ARB_USDT),
    (ARB_USDC, ARB_DAI),
    (ARB_WETH, ARB_ARB),
    (ARB_ARB, ARB_USDC),
];

// ── DEX factory addresses ─────────────────────────────────────────────────────

/// Uniswap V2 factory on Arbitrum (same as mainnet — Uniswap V2 canonical deploy)
pub const ARB_UNISWAP_V2_FACTORY: &str = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f";
/// Sushiswap V2 factory on Arbitrum
pub const ARB_SUSHISWAP_FACTORY: &str = "0xc35DADB65012eC5796536bD9864eD8773aBc74C4";
/// Uniswap V3 factory on Arbitrum (same canonical address as mainnet)
pub const ARB_UNISWAP_V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

// ── Backwards-compat aliases (mainnet defaults) ───────────────────────────────
// Kept for code that doesn't yet pass chain_id.

pub const WETH: &str = MAINNET_WETH;
pub const USDC: &str = MAINNET_USDC;
pub const USDT: &str = MAINNET_USDT;
pub const DAI: &str = MAINNET_DAI;
pub const WBTC: &str = MAINNET_WBTC;

/// Default token pairs to discover on startup (mainnet).
pub const DEFAULT_PAIRS: &[(&str, &str)] = MAINNET_DEFAULT_PAIRS;

/// Returns the default token pairs for the given chain ID.
pub fn default_pairs_for_chain(chain_id: u64) -> &'static [(&'static str, &'static str)] {
    match chain_id {
        42161 => ARB_DEFAULT_PAIRS,
        _ => MAINNET_DEFAULT_PAIRS,
    }
}

// ── Pool registry ─────────────────────────────────────────────────────────────

/// Pool registry: discovers and caches liquidity sources across all DEXes.
pub struct PoolRegistry {
    pools: Vec<LiquiditySource>,
    /// Index: canonical (token0, token1) lowercase pair → pool indices.
    index: HashMap<(String, String), Vec<usize>>,
}

impl PoolRegistry {
    pub fn new() -> Self {
        Self {
            pools: vec![],
            index: HashMap::new(),
        }
    }

    /// Return all pools that include both tokens (in either order).
    pub fn pools_for_pair(&self, token_a: &str, token_b: &str) -> Vec<&LiquiditySource> {
        let key_ab = (token_a.to_lowercase(), token_b.to_lowercase());
        let key_ba = (token_b.to_lowercase(), token_a.to_lowercase());

        let mut result = Vec::new();
        for key in [&key_ab, &key_ba] {
            if let Some(indices) = self.index.get(key) {
                for &idx in indices {
                    result.push(&self.pools[idx]);
                }
            }
        }
        result
    }

    pub fn all_pools(&self) -> &[LiquiditySource] {
        &self.pools
    }

    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Add a pool and update the index.
    ///
    /// For two-token pools, indexes a single (token0, token1) pair.
    /// For multi-token pools (Curve), indexes all unique token pairs so that
    /// `pools_for_pair` works for any combination of tokens in the pool.
    pub fn add_pool(&mut self, pool: LiquiditySource) {
        let idx = self.pools.len();
        match &pool {
            LiquiditySource::UniswapV2(p) => {
                let key = (p.token0.to_lowercase(), p.token1.to_lowercase());
                self.index.entry(key).or_default().push(idx);
            }
            LiquiditySource::UniswapV3(p) => {
                let key = (p.token0.to_lowercase(), p.token1.to_lowercase());
                self.index.entry(key).or_default().push(idx);
            }
            LiquiditySource::CamelotV2(p) => {
                let key = (p.token0.to_lowercase(), p.token1.to_lowercase());
                self.index.entry(key).or_default().push(idx);
            }
            LiquiditySource::CamelotV3(p) => {
                let key = (p.token0.to_lowercase(), p.token1.to_lowercase());
                self.index.entry(key).or_default().push(idx);
            }
            LiquiditySource::CurveStable(p) => {
                // Index every unique token pair in the pool.
                for i in 0..p.tokens.len() {
                    for j in (i + 1)..p.tokens.len() {
                        let key = (p.tokens[i].to_lowercase(), p.tokens[j].to_lowercase());
                        self.index.entry(key).or_default().push(idx);
                    }
                }
            }
            LiquiditySource::BalancerWeighted(p) => {
                // Balancer weighted pools can have 2-8 tokens; index all pairs.
                for i in 0..p.tokens.len() {
                    for j in (i + 1)..p.tokens.len() {
                        let key = (p.tokens[i].to_lowercase(), p.tokens[j].to_lowercase());
                        self.index.entry(key).or_default().push(idx);
                    }
                }
            }
            LiquiditySource::BalancerStable(p) => {
                // Balancer stable pools can have 2-5 tokens; index all pairs.
                for i in 0..p.tokens.len() {
                    for j in (i + 1)..p.tokens.len() {
                        let key = (p.tokens[i].to_lowercase(), p.tokens[j].to_lowercase());
                        self.index.entry(key).or_default().push(idx);
                    }
                }
            }
            LiquiditySource::GmxV2(p) => {
                let key = (p.long_token.to_lowercase(), p.short_token.to_lowercase());
                self.index.entry(key).or_default().push(idx);
            }
            LiquiditySource::TraderJoeV21(p) => {
                let key = (p.token_x.to_lowercase(), p.token_y.to_lowercase());
                self.index.entry(key).or_default().push(idx);
            }
            LiquiditySource::Dodo(p) => {
                let key = (p.base_token.to_lowercase(), p.quote_token.to_lowercase());
                self.index.entry(key).or_default().push(idx);
            }
            LiquiditySource::Wombat(p) => {
                for i in 0..p.tokens.len() {
                    for j in (i + 1)..p.tokens.len() {
                        let key = (p.tokens[i].to_lowercase(), p.tokens[j].to_lowercase());
                        self.index.entry(key).or_default().push(idx);
                    }
                }
            }
        }
        self.pools.push(pool);
    }

    // ── Discovery ─────────────────────────────────────────────────────────────

    /// Discover V2/V3 pools for a list of token pairs from all DEXes.
    ///
    /// Fetches Uniswap V2, Sushiswap, and all Uniswap V3 fee tiers.
    /// Pools that don't exist (zero address from factory) are skipped silently.
    pub async fn discover_pools(
        &mut self,
        rpc: &EthClient,
        pairs: &[(&str, &str)],
    ) -> anyhow::Result<()> {
        let mut discovered = 0usize;

        for &(token_a, token_b) in pairs {
            // Uniswap V2
            match uniswap_v2::fetch_pool(
                rpc,
                uniswap_v2::UNISWAP_V2_FACTORY,
                PoolKind::UniswapV2,
                token_a,
                token_b,
            )
            .await
            {
                Ok(Some(pool)) => {
                    debug!(address = %pool.address, "Discovered Uniswap V2 pool");
                    self.add_pool(LiquiditySource::UniswapV2(pool));
                    discovered += 1;
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, token_a, token_b, "Uniswap V2 discovery error"),
            }

            // Sushiswap
            match uniswap_v2::fetch_pool(
                rpc,
                uniswap_v2::SUSHISWAP_FACTORY,
                PoolKind::Sushiswap,
                token_a,
                token_b,
            )
            .await
            {
                Ok(Some(pool)) => {
                    debug!(address = %pool.address, "Discovered Sushiswap pool");
                    self.add_pool(LiquiditySource::UniswapV2(pool));
                    discovered += 1;
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, token_a, token_b, "Sushiswap discovery error"),
            }

            // Uniswap V3 — all fee tiers
            match uniswap_v3::fetch_all_fee_tiers(rpc, token_a, token_b).await {
                Ok(v3_pools) => {
                    for pool in v3_pools {
                        debug!(address = %pool.address, fee = pool.fee, "Discovered Uniswap V3 pool");
                        self.add_pool(LiquiditySource::UniswapV3(pool));
                        discovered += 1;
                    }
                }
                Err(e) => warn!(error = %e, token_a, token_b, "Uniswap V3 discovery error"),
            }

            // Camelot V2 (Arbitrum native DEX — directional fees)
            match camelot_v2::fetch_pool(rpc, token_a, token_b).await {
                Ok(Some(pool)) => {
                    debug!(address = %pool.address, "Discovered Camelot V2 pool");
                    self.add_pool(LiquiditySource::CamelotV2(pool));
                    discovered += 1;
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, token_a, token_b, "Camelot V2 discovery error"),
            }

            // Camelot V3 (Algebra — single pool per pair, dynamic fee)
            match camelot_v3::fetch_pool(rpc, token_a, token_b).await {
                Ok(Some(pool)) => {
                    debug!(address = %pool.address, fee = pool.fee, "Discovered Camelot V3 pool");
                    self.add_pool(LiquiditySource::CamelotV3(pool));
                    discovered += 1;
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, token_a, token_b, "Camelot V3 discovery error"),
            }

            // Trader Joe V2.1 (Liquidity Book — all common bin steps)
            match trader_joe::fetch_all_bin_steps(
                rpc,
                trader_joe::TRADER_JOE_V21_FACTORY,
                token_a,
                token_b,
            )
            .await
            {
                Ok(tj_pools) => {
                    for pool in tj_pools {
                        debug!(
                            address = %pool.address,
                            bin_step = pool.bin_step,
                            "Discovered Trader Joe V2.1 pool"
                        );
                        self.add_pool(LiquiditySource::TraderJoeV21(pool));
                        discovered += 1;
                    }
                }
                Err(e) => warn!(error = %e, token_a, token_b, "Trader Joe V2.1 discovery error"),
            }
        }

        info!(pool_count = discovered, "Pool discovery complete");
        Ok(())
    }

    /// Refresh reserves/prices for all cached pools.
    pub async fn sync_all(&mut self, rpc: &EthClient) -> anyhow::Result<()> {
        let mut errors = 0usize;
        for pool in &mut self.pools {
            let result = match pool {
                LiquiditySource::UniswapV2(p) => uniswap_v2::sync_reserves(rpc, p).await,
                LiquiditySource::UniswapV3(p) => uniswap_v3::sync_pool(rpc, p).await,
                LiquiditySource::CurveStable(p) => curve::sync_curve_pool(rpc, p).await,
                LiquiditySource::CamelotV2(p) => camelot_v2::sync_reserves(rpc, p).await,
                LiquiditySource::CamelotV3(p) => camelot_v3::sync_pool(rpc, p).await,
                // Balancer pools are synced via subgraph or Vault queries;
                // for now, skip on-chain sync (pools are loaded from config/subgraph).
                LiquiditySource::BalancerWeighted(_) => Ok(()),
                LiquiditySource::BalancerStable(_) => Ok(()),
                // GMX, DODO, Wombat — sync not yet implemented
                LiquiditySource::GmxV2(p) => gmx_v2::sync_pool(rpc, p).await,
                LiquiditySource::TraderJoeV21(p) => trader_joe::sync_pool(rpc, p).await,
                LiquiditySource::Dodo(p) => dodo::sync_pool(rpc, p).await,
                LiquiditySource::Wombat(p) => wombat::sync_pool(rpc, p).await,
            };
            if let Err(e) = result {
                errors += 1;
                warn!(error = %e, "Pool sync error");
            }
        }

        if errors > 0 {
            warn!(
                synced = self.pools.len() - errors,
                errors = errors,
                "sync_all completed with errors"
            );
        } else {
            debug!(pool_count = self.pools.len(), "sync_all complete");
        }
        Ok(())
    }

    /// Load Curve pools from a resolved chain configuration.
    ///
    /// Converts each `ResolvedCurvePool` in the chain config into a
    /// `CurveStablePool` liquidity source and adds it to the registry.
    pub fn load_curve_pools_from_config(&mut self, chain_config: &ChainConfig) {
        for resolved in &chain_config.curve_pools {
            let pool = curve::resolved_to_liquidity_source(resolved);
            debug!(
                address = %pool.address(),
                name = %resolved.name,
                tokens = resolved.tokens.len(),
                "Loaded Curve pool from config"
            );
            self.add_pool(pool);
        }
        if !chain_config.curve_pools.is_empty() {
            info!(
                count = chain_config.curve_pools.len(),
                "Curve pools loaded from chain config"
            );
        }
    }

    /// Load Balancer V2 pools from a resolved chain configuration.
    ///
    /// Converts each `ResolvedBalancerPool` in the chain config into a
    /// `BalancerWeighted` or `BalancerStable` liquidity source.
    pub fn load_balancer_pools_from_config(&mut self, chain_config: &ChainConfig) {
        for resolved in &chain_config.balancer_pools {
            let pool = balancer_v2::resolved_to_liquidity_source(resolved);
            debug!(
                address = %pool.address(),
                name = %resolved.name,
                pool_type = %resolved.pool_type,
                tokens = resolved.tokens.len(),
                "Loaded Balancer V2 pool from config"
            );
            self.add_pool(pool);
        }
        if !chain_config.balancer_pools.is_empty() {
            info!(
                count = chain_config.balancer_pools.len(),
                "Balancer V2 pools loaded from chain config"
            );
        }
    }

    /// Load DODO pools from a resolved chain configuration.
/// Discover GMX V2 markets from a list of known market addresses.    ///    /// GMX V2 markets are not pair-based like AMMs — each market has a fixed    /// address. We fetch pool state from the Reader contract for each market.    pub async fn discover_gmx_v2_markets(        &mut self,        rpc: &EthClient,        market_addresses: &[&str],    ) -> anyhow::Result<()> {        let mut discovered = 0usize;        for &market in market_addresses {            match gmx_v2::fetch_pool(rpc, market).await {                Ok(Some(pool)) => {                    debug!(                        address = %pool.address,                        long_token = %pool.long_token,                        short_token = %pool.short_token,                        "Discovered GMX V2 market"                    );                    self.add_pool(LiquiditySource::GmxV2(pool));                    discovered += 1;                }                Ok(None) => {                    debug!(market = market, "GMX V2 market not found");                }                Err(e) => warn!(error = %e, market = market, "GMX V2 discovery error"),            }        }        if discovered > 0 {            info!(count = discovered, "GMX V2 markets discovered");        }        Ok(())    }    /// Load GMX V2 markets from a resolved chain configuration.    pub fn load_gmx_v2_markets_from_config(&mut self, chain_config: &ChainConfig) {        for market_cfg in &chain_config.gmx_v2_markets {            let pool = crate::models::liquidity::GmxV2Pool {                address: market_cfg.address.clone(),                long_token: market_cfg.long_token.clone(),                short_token: market_cfg.short_token.clone(),                index_token: market_cfg.index_token.clone(),                long_token_amount: "0".to_string(),                short_token_amount: "0".to_string(),                swap_fee_bps: market_cfg.swap_fee_bps,                swap_impact_factor_positive: "0.00001".to_string(),                swap_impact_factor_negative: "0.00002".to_string(),                long_token_price_usd: "0.0".to_string(),                short_token_price_usd: "0.0".to_string(),            };            debug!(                address = %pool.address,                name = %market_cfg.name,                "Loaded GMX V2 market from config"            );            self.add_pool(LiquiditySource::GmxV2(pool));        }        if !chain_config.gmx_v2_markets.is_empty() {            info!(                count = chain_config.gmx_v2_markets.len(),                "GMX V2 markets loaded from chain config"            );        }    }
    ///
    /// DODO pools are configured with explicit addresses (no factory discovery).
    /// Fetches on-chain state for each configured pool.
    pub async fn load_dodo_pools_from_config(
        &mut self,
        rpc: &EthClient,
        chain_config: &ChainConfig,
    ) {
        for pool_cfg in &chain_config.dodo_pools {
            match dodo::fetch_pool(rpc, &pool_cfg.address, &pool_cfg.pool_type).await {
                Ok(Some(pool)) => {
                    debug!(
                        address = %pool.address,
                        base = %pool.base_token,
                        quote = %pool.quote_token,
                        "Loaded DODO pool from config"
                    );
                    self.add_pool(LiquiditySource::Dodo(pool));
                }
                Ok(None) => {
                    warn!(address = %pool_cfg.address, "DODO pool not found on-chain");
                }
                Err(e) => {
                    warn!(address = %pool_cfg.address, error = %e, "DODO pool fetch error");
                }
            }
        }
        if !chain_config.dodo_pools.is_empty() {
            info!(
                count = chain_config.dodo_pools.len(),
                "DODO pools loaded from chain config"
            );
        }
    }

    /// Load Wombat pools from a resolved chain configuration.
    ///
    /// Wombat pools are multi-asset; tokens are resolved from config.
    pub async fn load_wombat_pools_from_config(
        &mut self,
        rpc: &EthClient,
        chain_config: &ChainConfig,
    ) {
        for pool_cfg in &chain_config.wombat_pools {
            let token_refs: Vec<&str> = pool_cfg.tokens.iter().map(|s| s.as_str()).collect();
            match wombat::fetch_pool(rpc, &pool_cfg.address, &token_refs).await {
                Ok(Some(pool)) => {
                    debug!(
                        address = %pool.address,
                        num_tokens = pool.tokens.len(),
                        "Loaded Wombat pool from config"
                    );
                    self.add_pool(LiquiditySource::Wombat(pool));
                }
                Ok(None) => {
                    warn!(address = %pool_cfg.address, "Wombat pool not found on-chain");
                }
                Err(e) => {
                    warn!(address = %pool_cfg.address, error = %e, "Wombat pool fetch error");
                }
            }
        }
        if !chain_config.wombat_pools.is_empty() {
            info!(
                count = chain_config.wombat_pools.len(),
                "Wombat pools loaded from chain config"
            );
        }
    }

    /// Load hardcoded Curve pools for a given chain_id (fallback when no TOML config).
    pub fn load_curve_pools_hardcoded(&mut self, chain_id: u64) {
        let pools = curve::pools_for_chain(chain_id);
        for pool in &pools {
            let source = curve::curve_pool_to_registry_source(pool);
            debug!(address = %pool.address, "Loaded hardcoded Curve pool");
            self.add_pool(source);
        }
        if !pools.is_empty() {
            info!(count = pools.len(), chain_id, "Hardcoded Curve pools loaded");
        }
    }
}

impl Default for PoolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Convenience: build registry from pre-fetched pools ───────────────────────

impl From<Vec<UniswapV2Pool>> for PoolRegistry {
    fn from(v2_pools: Vec<UniswapV2Pool>) -> Self {
        let mut registry = PoolRegistry::new();
        for pool in v2_pools {
            registry.add_pool(LiquiditySource::UniswapV2(pool));
        }
        registry
    }
}

impl From<Vec<UniswapV3Pool>> for PoolRegistry {
    fn from(v3_pools: Vec<UniswapV3Pool>) -> Self {
        let mut registry = PoolRegistry::new();
        for pool in v3_pools {
            registry.add_pool(LiquiditySource::UniswapV3(pool));
        }
        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::{PoolKind, UniswapV2Pool};

    fn make_v2_pool(addr: &str, t0: &str, t1: &str) -> LiquiditySource {
        LiquiditySource::UniswapV2(UniswapV2Pool {
            address: addr.to_string(),
            kind: PoolKind::UniswapV2,
            token0: t0.to_string(),
            token1: t1.to_string(),
            reserve0: "1000000000".to_string(),
            reserve1: "2000000000".to_string(),
            fee_bps: 30,
        })
    }

    #[test]
    fn add_and_lookup() {
        let mut registry = PoolRegistry::new();
        registry.add_pool(make_v2_pool("0xpool1", "0xweth", "0xusdc"));
        registry.add_pool(make_v2_pool("0xpool2", "0xweth", "0xdai"));

        let pairs = registry.pools_for_pair("0xweth", "0xusdc");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].address(), "0xpool1");

        // Reversed lookup
        let pairs = registry.pools_for_pair("0xusdc", "0xweth");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].address(), "0xpool1");
    }

    #[test]
    fn lookup_missing_returns_empty() {
        let registry = PoolRegistry::new();
        let pairs = registry.pools_for_pair("0xweth", "0xusdc");
        assert!(pairs.is_empty());
    }

    #[test]
    fn pool_count() {
        let mut registry = PoolRegistry::new();
        assert_eq!(registry.pool_count(), 0);
        registry.add_pool(make_v2_pool("0xp1", "0xa", "0xb"));
        registry.add_pool(make_v2_pool("0xp2", "0xc", "0xd"));
        assert_eq!(registry.pool_count(), 2);
    }
}
