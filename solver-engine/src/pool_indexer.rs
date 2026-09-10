//! Pool Indexer (B.1)
//!
//! Real-time pool reserve cache backed by a DashMap.
//! Tracks the latest on-chain reserves for every pool we care about so the
//! solver always reads from an in-memory snapshot rather than firing RPC calls
//! on the critical solve path.
//!
//! ## Architecture
//!
//! ```text
//! Alchemy WSS ──newHeads──▶ IndexerTask
//!                               │
//!                         for each watched pool
//!                               │
//!                         JSON-RPC eth_call (getReserves / slot0)
//!                               │
//!                         POOL_CACHE.insert(address, PoolSnapshot)
//!                               │
//!                         CURRENT_BLOCK.store(head)
//! ```
//!
//! ## Kill Switch
//! Set `POOL_INDEXER_ENABLED=false` to disable the background refresh.
//! The cache will remain empty; the solver falls back to auction-provided data.
//!
//! ## Usage
//! ```rust,ignore
//! // Startup (once):
//! tokio::spawn(pool_indexer::run(rpc_url, chain_id));
//!
//! // In solve path:
//! let block = pool_indexer::current_block();
//! let snap  = pool_indexer::get("0x..pool_address..");
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::{OnceLock, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use tracing::{debug, info, warn};

// ── Types ────────────────────────────────────────────────────────────────────

/// Pool type — determines which refresh method and liquidity export to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolType {
    /// Uniswap V2 / Sushiswap / Camelot V2 — uses getReserves()
    V2,
    /// Uniswap V3 / Camelot V3 — uses slot0() for sqrtPrice/tick/liquidity
    V3,
}

/// In-memory snapshot of a pool's state at a particular block.
#[derive(Debug, Clone)]
pub struct PoolSnapshot {
    /// Pool contract address (lowercase hex)
    pub address: String,
    /// First token address
    pub token0: String,
    /// Second token address
    pub token1: String,
    /// Pool type (V2 or V3)
    pub pool_type: PoolType,
    /// Reserve of token0 (V2 pools only)
    pub reserve0: u128,
    /// Reserve of token1 (V2 pools only)
    pub reserve1: u128,
    /// V3: sqrtPriceX96 from slot0()
    pub sqrt_price_x96: Option<u128>,
    /// V3: current tick from slot0()
    pub tick: Option<i32>,
    /// V3: active liquidity from slot0()
    pub v3_liquidity: Option<u128>,
    /// V3: fee tier in basis points (100, 500, 3000, 10000)
    pub fee_tier: Option<u32>,
    /// Block this snapshot was taken at
    pub block: u64,
    /// Unix timestamp (seconds) this snapshot was taken
    pub fetched_at: u64,
}

impl PoolSnapshot {
    /// Age of this snapshot in seconds.
    pub fn age_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(self.fetched_at)
    }

    /// Whether this snapshot is considered stale. With event-based polling,
    /// actively traded pools update every 10s. Quiet pools keep their startup
    /// data which is valid until reserves actually change on-chain.
    /// 10 min threshold: generous enough for low-activity pools.
    pub fn is_stale(&self) -> bool {
        self.age_secs() > 600
    }
}

// ── Global state ─────────────────────────────────────────────────────────────

/// Current chain head block number (updated on every newHeads event).
static CURRENT_BLOCK: AtomicU64 = AtomicU64::new(0);

/// Hot pool reserve cache: address → PoolSnapshot
static POOL_CACHE: OnceLock<DashMap<String, PoolSnapshot>> = OnceLock::new();

pub fn pool_cache() -> &'static DashMap<String, PoolSnapshot> {
    POOL_CACHE.get_or_init(DashMap::new)
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Get the most recent block number seen by the indexer (0 if not started).
pub fn current_block() -> u64 {
    CURRENT_BLOCK.load(Ordering::Relaxed)
}

/// Get a pool snapshot by address (lowercase hex with 0x prefix).
/// Returns `None` if the pool has not been indexed yet.
pub fn get(address: &str) -> Option<PoolSnapshot> {
    pool_cache().get(address).map(|r| r.clone())
}

/// Insert or update a pool snapshot (called by the indexer and by tests).
pub fn upsert(snap: PoolSnapshot) {
    pool_cache().insert(snap.address.clone(), snap);
}

/// Advance the known block number (called by the indexer and by tests).
pub fn set_block(block: u64) {
    // Only advance — never go backwards
    let _ = CURRENT_BLOCK.fetch_max(block, Ordering::Relaxed);
}

/// Return all currently cached pool addresses.
pub fn cached_addresses() -> Vec<String> {
    pool_cache().iter().map(|r| r.key().clone()).collect()
}

/// Number of pools currently in cache.
pub fn pool_count() -> usize {
    pool_cache().len()
}

// ── Hot pool tracking ────────────────────────────────────────────────────

/// Pools referenced by recent auctions get priority refresh.
static HOT_POOLS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn hot_pool_set() -> &'static Mutex<HashSet<String>> {
    HOT_POOLS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mark a pool as "hot" — it appeared in a recent auction and should be
/// refreshed on the fast cycle. Called from the solve handler.
pub fn mark_hot(address: &str) {
    if let Ok(mut set) = hot_pool_set().lock() {
        set.insert(address.to_lowercase());
    }
}

/// Get the current hot pool addresses (capped at MAX_HOT_POOLS).
fn take_hot_addresses() -> Vec<String> {
    let set = hot_pool_set().lock().unwrap_or_else(|e| e.into_inner());
    set.iter().take(MAX_HOT_POOLS).cloned().collect()
}

// ── Background Indexer ───────────────────────────────────────────────────────

/// Kill switch — check if pool indexer is enabled.
pub fn is_enabled() -> bool {
    std::env::var("POOL_INDEXER_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

/// Event-based refresh interval. Uses eth_getLogs to catch V2 Sync events for ALL
/// With v2 aggregator-powered routing, pool reserves are only needed for
/// graph topology and CoW matching (not for AMM math). Reduced from 10s to 120s
/// to cut RPC costs ~90%. 120s: 75 CU × 720/day × 30 = 1.6M CU/month.
const EVENT_POLL_SECS: u64 = 120;

/// Maximum hot pools to refresh per cycle (V2 + V3 combined).
/// With 90s cycle: 40 pools × 26 CU × 960/day × 30 = 29.9M CU/month (just fits free tier).
const MAX_HOT_POOLS: usize = 40;

/// Run the pool indexer background task.
///
/// Hot-only refresh: ONLY refreshes pools that were used in recent solutions.
/// No slow all-pool sweep — unused pools stay at startup reserves (fine since
/// we don't route through them). Fits Alchemy free tier (30M CU/month).
pub async fn run(rpc_url: String, _chain_id: u64) {
    if !is_enabled() {
        info!("Pool indexer disabled via kill switch");
        return;
    }

    // Bootstrap: seed well-known Arbitrum pools so triage passes on first auction.
    if _chain_id == 42161 {
        bootstrap_arbitrum_pools();
    }

    info!(
        pools = pool_count(),
        event_poll_secs = EVENT_POLL_SECS,
        "Pool indexer started (event-based V2 refresh + slot0 V3 refresh)"
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let mut last_block = 0u64;

    // Startup: refresh top pools quickly, then do a full refresh in background.
    // This lets the first auction be served fast (~100ms) while remaining pools
    // load in the background over the next 30-60 seconds.
    match fetch_block_number(&client, &rpc_url).await {
        Ok(block) => {
            last_block = block;
            set_block(block);
            // Quick startup: only bootstrap V2 pools (26 hardcoded) + top 20 V3
            let bootstrap_v2: Vec<String> = pool_cache().iter()
                .filter(|e| e.value().pool_type == PoolType::V2 && !e.value().token0.is_empty())
                .take(30)
                .map(|e| e.key().clone())
                .collect();
            if !bootstrap_v2.is_empty() {
                info!(pools = bootstrap_v2.len(), "Quick startup: V2 bootstrap");
                refresh_pools(&client, &rpc_url, block, &bootstrap_v2).await;
            }
            let bootstrap_v3: Vec<String> = pool_cache().iter()
                .filter(|e| e.value().pool_type == PoolType::V3 && !e.value().token0.is_empty())
                .take(20)
                .map(|e| e.key().clone())
                .collect();
            if !bootstrap_v3.is_empty() {
                info!(pools = bootstrap_v3.len(), "Quick startup: V3 bootstrap (slot0)");
                refresh_v3_pools(&client, &rpc_url, block, &bootstrap_v3).await;
            }
        }
        Err(e) => warn!(error = %e, "Initial block number fetch failed"),
    }

    // Background: refresh remaining pools in batches (non-blocking)
    {
        let client = client.clone();
        let rpc_url = rpc_url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await; // let first auctions flow
            let block = current_block();
            // Refresh remaining V2 pools in chunks
            let remaining_v2: Vec<String> = pool_cache().iter()
                .filter(|e| e.value().pool_type == PoolType::V2 && e.value().reserve0 == 0 && !e.value().token0.is_empty())
                .map(|e| e.key().clone())
                .collect();
            if !remaining_v2.is_empty() {
                info!(pools = remaining_v2.len(), "Background: refreshing remaining V2 pools");
                refresh_pools(&client, &rpc_url, block, &remaining_v2).await;
            }
            let remaining_v3: Vec<String> = pool_cache().iter()
                .filter(|e| e.value().pool_type == PoolType::V3 && e.value().sqrt_price_x96.is_none() && !e.value().token0.is_empty())
                .map(|e| e.key().clone())
                .collect();
            if !remaining_v3.is_empty() {
                info!(pools = remaining_v3.len(), "Background: refreshing remaining V3 pools (slot0)");
                refresh_v3_pools(&client, &rpc_url, block, &remaining_v3).await;
            }
            info!("Background pool refresh complete");
        });
    }

    // Track last polled block for eth_getLogs range queries
    let mut last_polled_block = last_block;

    loop {
        tokio::time::sleep(Duration::from_secs(EVENT_POLL_SECS)).await;

        // Fetch current block number
        let block = match fetch_block_number(&client, &rpc_url).await {
            Ok(b) => {
                if b > last_block {
                    last_block = b;
                    set_block(b);
                }
                b
            }
            Err(e) => {
                debug!(error = %e, "Pool indexer block fetch failed, will retry");
                continue;
            }
        };

        // ── Event-based refresh: V2 Sync + V3 Swap in one call ──────────
        // One eth_getLogs call (75 CU) fetches ALL state changes for ALL
        // V2 and V3 pools since last poll. Updates reserves (V2) and
        // sqrtPrice/tick/liquidity (V3) from on-chain events.
        // Freshness: 10 seconds for both V2 AND V3.
        if block > last_polled_block {
            // Cap block range to avoid massive eth_getLogs responses on Arbitrum
            // (Arbitrum produces ~4 blocks/sec, 10s = ~40 blocks, each with many events)
            let max_range = 50; // ~12 seconds of Arbitrum blocks
            let from_block = if block - last_polled_block > max_range {
                block - max_range // skip old blocks, just get recent
            } else {
                last_polled_block + 1
            };
            let updated = fetch_sync_events(&client, &rpc_url, from_block, block).await;
            if updated > 0 {
                debug!(from_block, to_block = block, updated, "Pool events processed (V2 Sync + V3 Swap)");
            }
            last_polled_block = block;
        }

        crate::monitoring::set_pool_count(pool_cache().len() as u64);
    }
}

/// Seed the indexer with pools from an auction's liquidity list.
///
/// Called from the solve handler after parsing an auction. Seeds the cache
/// with pool addresses so the background task knows what to watch.
pub fn seed_from_auction(liquidity: &[crate::models::liquidity::Liquidity]) {
    use crate::models::liquidity::Liquidity;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let current = current_block();

    for pool in liquidity {
        match pool {
            Liquidity::ConstantProduct(p) => {
                // Only seed if not already cached (avoid overwriting fresher data)
                if !pool_cache().contains_key(&p.address) {
                    let tokens: Vec<&str> = p.tokens.keys().map(|s| s.as_str()).collect();
                    if tokens.len() >= 2 {
                        upsert(PoolSnapshot {
                            address: p.address.clone(),
                            token0: tokens[0].to_string(),
                            token1: tokens[1].to_string(),
                            pool_type: PoolType::V2,
                            reserve0: 0,
                            reserve1: 0,
                            sqrt_price_x96: None,
                            tick: None,
                            v3_liquidity: None,
                            fee_tier: None,
                            block: current,
                            fetched_at: now,
                        });
                    }
                }
            }
            _ => {} // CL pools (V3) handled differently — skip for seeding
        }
    }
}

// ── Bootstrap ────────────────────────────────────────────────────────────────

/// Seed the cache with well-known Arbitrum pool addresses so triage sees
/// non-zero liquidity from the very first auction.  Reserves start at 0 and
/// get filled by the background polling loop within a few seconds.
fn bootstrap_arbitrum_pools() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Well-known Arbitrum tokens (lowercase with 0x prefix)
    const WETH:   &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
    const USDC:   &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
    const USDT:   &str = "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9";
    const WBTC:   &str = "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f";
    const ARB:    &str = "0x912ce59144191c1204e64559fe8253a0e49e6548";
    const WSTETH: &str = "0x5979d7b546e38e9ab8049524bc56bbc6aad10f00";
    const DAI:    &str = "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1";
    const USDCE:  &str = "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8"; // Bridged USDC.e

    // (address, token0, token1) — major Arbitrum liquidity venues
    // All V2-style pools that support getReserves() — these get real reserves via RPC.
    // SushiSwap addresses confirmed via factory.getPair()
    let pools: &[(&str, &str, &str)] = &[
        // ── SushiSwap V2 (confirmed via factory.getPair) ─────────────────────
        ("0x57b85fef094e10b5eecdf350af688299e9553378", WETH, USDC),     // WETH/USDC (native)
        ("0x905dfcd5649217c42684f23958568e533c711aa3", WETH, USDCE),    // WETH/USDC.e
        ("0xcb0e5bfa72bbb4d16ab5aa0c60601c438f04b4ad", WETH, USDT),    // WETH/USDT
        ("0xbf6cbb1f40a542af50839cad01b0dc1747f11e18", WETH, ARB),     // WETH/ARB
        ("0x515e252b2b5c22b4b2b6df66c2ebeea871aa4d69", WETH, WBTC),    // WETH/WBTC
        ("0x8cebfb915f7aa8474abcf0fff11d869a256eb887", USDC, USDT),    // USDC/USDT

        // ── Camelot V2 ──────────────────────────────────────────────────────
        ("0x84652bb2539513baf36e225c930fdd8eaa63ce27", WETH, USDCE),    // WETH/USDC.e
        ("0xa6c5c7d189fa4eb5af8ba34e63dcdd3a635d433f", WETH, USDC),     // WETH/USDC (native)
        ("0xe80b4f755417fb4baf4dbd23c029db3f62786523", ARB, USDCE),     // ARB/USDC.e
        ("0xfa0724d8569a1f775e285d5e3f38bd570dd63e8a", WETH, ARB),      // WETH/ARB
        ("0x7ddbfc2bbb1881f4d5311cbf7274f5c525fb9a62", WETH, WBTC),     // WETH/WBTC
        ("0xcc7e20c937f46b4db51a2a1d3f64bd42d155fecc", USDC, USDT),     // USDC/USDT
        ("0x0e4831e73fbfa2a73cc04de62033590e64019a22", USDC, DAI),      // USDC/DAI

        // ── Curve (V2-style getReserves not available — keep for triage but reserves may = 0)
        ("0x7f90122bf0700f9e7e1f688fe926940e8839f353", USDC, USDT),     // 2pool USDC/USDT
        ("0x960ea3e3c7fb317332d990873d354e18d7645590", USDT, WBTC),     // tricrypto

        // ── Balancer (V2 vault style — getReserves may not work, keep for triage)
        ("0x36bf227d6bac96e2ab1ebb5492ecec69c691943f", WSTETH, WETH),   // wstETH/WETH
        ("0x1533a3278f3f9141d5f820a184ea4b017fce2382", USDC, USDT),     // USDC/USDT/DAI
        ("0xcc65a812ce382ab909a11e434dbf75b34f1cc59d", WETH, ARB),      // WETH/ARB

        // ── DODO
        ("0xe4b2dfc82977dd2dce7e8d37895a6a8f50cbb4fb", USDC, USDT),    // USDC/USDT
        // ── Wombat
        ("0xc6bc781e20f9323012f6e422bdf552ff06ba6cd1", USDC, USDT),     // Main Pool
    ];

    for (addr, t0, t1) in pools {
        upsert(PoolSnapshot {
            address: addr.to_string(),
            token0: t0.to_string(),
            token1: t1.to_string(),
            pool_type: PoolType::V2,
            reserve0: 0,
            reserve1: 0,
            sqrt_price_x96: None,
            tick: None,
            v3_liquidity: None,
            fee_tier: None,
            block: 0,
            fetched_at: now,
        });
    }

    info!(pools = pools.len(), "Bootstrapped Arbitrum pool cache");
}

// ── Liquidity conversion ────────────────────────────────────────────────────

/// Convert all cached pool snapshots with non-zero reserves into `Liquidity::ConstantProduct`
/// entries that can be used by the routing strategies.
///
/// This is the bridge between the pool indexer (background RPC) and the solver
/// (which expects `Vec<Liquidity>`). Call this before solving to enrich the
/// auction's liquidity with our own cached on-chain data.
pub fn cached_as_liquidity() -> Vec<crate::models::liquidity::Liquidity> {
    use crate::models::liquidity::{
        ConstantProductPool, Liquidity, LiquidityTokenBalance, LiquidityTokenMap,
    };

    let mut result = Vec::new();

    for entry in pool_cache().iter() {
        let snap = entry.value();

        // Skip stale pools
        if snap.is_stale() {
            continue;
        }

        match snap.pool_type {
            PoolType::V2 => {
                if snap.reserve0 == 0 || snap.reserve1 == 0 {
                    continue;
                }
                let mut tokens = LiquidityTokenMap::new();
                tokens.insert(snap.token0.clone(), LiquidityTokenBalance { balance: snap.reserve0.to_string() });
                tokens.insert(snap.token1.clone(), LiquidityTokenBalance { balance: snap.reserve1.to_string() });
                result.push(Liquidity::ConstantProduct(ConstantProductPool {
                    id: format!("indexer_{}", snap.address),
                    address: snap.address.clone(),
                    tokens,
                    fee: "0.003".to_string(),
                    router: None,
                    gas_estimate: String::new(),
                }));
            }
            PoolType::V3 => {
                let sqrt_price = match snap.sqrt_price_x96 {
                    Some(p) if p > 0 => p,
                    _ => continue, // No V3 state yet
                };
                let tokens = vec![snap.token0.clone(), snap.token1.clone()];

                let fee_str = match snap.fee_tier {
                    Some(100) => "0.0001",
                    Some(500) => "0.0005",
                    Some(3000) => "0.003",
                    Some(10000) => "0.01",
                    _ => "0.003",
                };

                result.push(Liquidity::ConcentratedLiquidity(
                    crate::models::liquidity::ConcentratedLiquidityPool {
                        id: format!("indexer_v3_{}", snap.address),
                        address: snap.address.clone(),
                        tokens,
                        fee: fee_str.to_string(),
                        router: None,
                        sqrt_price: sqrt_price.to_string(),
                        liquidity: snap.v3_liquidity.unwrap_or(0).to_string(),
                        tick: snap.tick.unwrap_or(0),
                        gas_estimate: String::new(),
                        // Include tick data from subgraph if available
                        liquidity_net: crate::subgraph::get_tick_data(&snap.address)
                            .map(|ticks| {
                                ticks.into_iter()
                                    .map(|(idx, net)| (idx.to_string(), net.to_string()))
                                    .collect()
                            }),
                    },
                ));
            }
        }
    }

    result
}

/// Just-in-time refresh: fetch fresh reserves for specific pools before routing.
///
/// This is the critical fix for stale reserves. Instead of using cached data
/// that may be minutes old, we fetch current reserves right before computing
/// swap outputs. Takes ~50-200ms for 5-50 pools.
///
/// Returns the number of pools successfully refreshed.
pub async fn jit_refresh(addresses: &[String]) -> usize {
    if addresses.is_empty() {
        return 0;
    }

    let rpc_url = match std::env::var("RPC_URL") {
        Ok(url) => url,
        Err(_) => return 0,
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let block = current_block();
    let before_count = addresses.len();

    // Use the existing refresh_pools function which handles batching
    refresh_pools(&client, &rpc_url, block, addresses).await;

    // Count how many were actually updated (non-zero reserves, recent timestamp)
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let refreshed = addresses.iter()
        .filter(|addr| {
            pool_cache()
                .get(addr.as_str())
                .map(|snap| snap.fetched_at >= now.saturating_sub(5) && snap.reserve0 > 0)
                .unwrap_or(false)
        })
        .count();

    tracing::info!(
        requested = before_count,
        refreshed = refreshed,
        block = block,
        "JIT pool refresh complete"
    );
    refreshed
}

// ── RPC helpers ──────────────────────────────────────────────────────────────

async fn fetch_block_number(client: &reqwest::Client, rpc_url: &str) -> Result<u64, String> {
    let t0 = std::time::Instant::now();
    let resp = client
        .post(rpc_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_blockNumber",
            "params": [],
            "id": 1
        }))
        .send()
        .await
        .map_err(|e| {
            crate::monitoring::record_rpc(t0.elapsed().as_millis() as u64, true);
            e.to_string()
        })?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| {
            crate::monitoring::record_rpc(t0.elapsed().as_millis() as u64, true);
            e.to_string()
        })?;

    let latency_ms = t0.elapsed().as_millis() as u64;

    let hex = resp["result"]
        .as_str()
        .ok_or_else(|| {
            crate::monitoring::record_rpc(latency_ms, true);
            "missing result".to_string()
        })?;

    crate::monitoring::record_rpc(latency_ms, false);
    u64::from_str_radix(hex.trim_start_matches("0x"), 16)
        .map_err(|e| e.to_string())
}

/// Fetch reserves for a batch of Uniswap V2-style pools.
/// `getReserves()` selector: 0x0902f1ac
///
/// Sends requests in chunks of CHUNK_SIZE to avoid exceeding Alchemy's
/// max batch size. A small delay between chunks prevents rate limiting.
const REFRESH_CHUNK_SIZE: usize = 50;

async fn refresh_pools(
    client: &reqwest::Client,
    rpc_url: &str,
    block: u64,
    addresses: &[String],
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut total_updated = 0usize;
    let mut total_errors = 0usize;

    for (chunk_idx, chunk) in addresses.chunks(REFRESH_CHUNK_SIZE).enumerate() {
        // Small delay between chunks to avoid rate limiting (skip first chunk)
        if chunk_idx > 0 {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let calls: Vec<serde_json::Value> = chunk
            .iter()
            .enumerate()
            .map(|(i, addr)| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "eth_call",
                    "params": [{
                        "to": addr,
                        "data": "0x0902f1ac" // getReserves()
                    }, "latest"],
                    "id": i
                })
            })
            .collect();

        let t0 = std::time::Instant::now();
        let resp = match client
            .post(rpc_url)
            .json(&calls)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let latency_ms = t0.elapsed().as_millis() as u64;
                crate::monitoring::record_rpc(latency_ms, true);
                total_errors += chunk.len();
                warn!(error = %e, chunk = chunk_idx, pools = chunk.len(),
                    "Pool refresh chunk failed — RPC send error");
                continue;
            }
        };

        let results: Vec<serde_json::Value> = match resp.json().await {
            Ok(r) => r,
            Err(e) => {
                let latency_ms = t0.elapsed().as_millis() as u64;
                crate::monitoring::record_rpc(latency_ms, true);
                total_errors += chunk.len();
                warn!(error = %e, chunk = chunk_idx, pools = chunk.len(),
                    "Pool refresh chunk failed — parse error");
                continue;
            }
        };

        let latency_ms = t0.elapsed().as_millis() as u64;
        crate::monitoring::record_rpc(latency_ms, false);

        // Build id → address map for this chunk
        let id_to_addr: HashMap<usize, &str> = chunk
            .iter()
            .enumerate()
            .map(|(i, addr)| (i, addr.as_str()))
            .collect();

        for result in &results {
            let id = result["id"].as_u64().unwrap_or(9999) as usize;
            let addr = match id_to_addr.get(&id) {
                Some(a) => *a,
                None => continue,
            };

            let hex = match result["result"].as_str() {
                Some(h) if h.len() >= 2 => h,
                _ => continue,
            };

            let data = hex.trim_start_matches("0x");
            if data.len() < 128 {
                continue;
            }
            let r0 = u128::from_str_radix(&data[0..64], 16).unwrap_or(0);
            let r1 = u128::from_str_radix(&data[64..128], 16).unwrap_or(0);

            if let Some(mut snap) = pool_cache().get_mut(addr) {
                let had_zero = snap.reserve0 == 0 && snap.reserve1 == 0;
                snap.reserve0 = r0;
                snap.reserve1 = r1;
                snap.block = block;
                snap.fetched_at = now;
                if r0 > 0 && r1 > 0 {
                    total_updated += 1;
                    if had_zero {
                        debug!(pool = addr, reserve0 = r0, reserve1 = r1, block,
                            "Pool reserves loaded from RPC");
                    }
                }
            }
        }
    }

    // Summary log so we can verify refresh is working
    let total_pools = addresses.len();
    if total_errors > 0 {
        warn!(total_pools, updated = total_updated, errors = total_errors, block,
            "Pool refresh completed with errors");
    } else {
        info!(total_pools, updated = total_updated, block,
            "Pool refresh completed");
    }
}

/// Fetch Uniswap V2 Sync events via eth_getLogs for a block range.
///
/// The Sync event is emitted on every V2 swap/mint/burn:
///   event Sync(uint112 reserve0, uint112 reserve1)
///   topic0: 0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1
///
/// One eth_getLogs call (75 CU) returns ALL Sync events across ALL pools in the
/// block range. We filter to pools in our cache and update reserves.
///
/// Returns the number of pools updated.
/// Fetch V2 Sync + V3 Swap events via a single eth_getLogs call.
///
/// V2 Sync: topic 0x1c411e..., data = (reserve0, reserve1)
/// V3 Swap: topic 0xc42079..., data = (amount0, amount1, sqrtPriceX96, liquidity, tick)
///
/// One RPC call (75 CU) catches ALL pool state changes in the block range.
/// Returns number of pools updated.
async fn fetch_sync_events(
    client: &reqwest::Client,
    rpc_url: &str,
    from_block: u64,
    to_block: u64,
) -> usize {
    const SYNC_TOPIC: &str = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";
    const SWAP_V3_TOPIC: &str = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";
    // V3 Mint/Burn events update liquidityNet at tick boundaries
    const MINT_TOPIC: &str = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde";
    const BURN_TOPIC: &str = "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c";

    // Build lookup sets for both pool types
    let v2_addrs: std::collections::HashSet<String> = pool_cache().iter()
        .filter(|e| e.value().pool_type == PoolType::V2)
        .map(|e| e.key().to_lowercase())
        .collect();
    let v3_addrs: std::collections::HashSet<String> = pool_cache().iter()
        .filter(|e| e.value().pool_type == PoolType::V3)
        .map(|e| e.key().to_lowercase())
        .collect();

    if v2_addrs.is_empty() && v3_addrs.is_empty() {
        return 0;
    }

    let t0 = std::time::Instant::now();
    // Query both event types in one call using OR on topic0
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getLogs",
        "params": [{
            "fromBlock": format!("0x{:x}", from_block),
            "toBlock": format!("0x{:x}", to_block),
            "topics": [[SYNC_TOPIC, SWAP_V3_TOPIC, MINT_TOPIC, BURN_TOPIC]]
        }],
        "id": 1
    });

    let resp = match client.post(rpc_url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            crate::monitoring::record_rpc(t0.elapsed().as_millis() as u64, true);
            debug!(error = %e, "eth_getLogs failed");
            return 0;
        }
    };

    let result: serde_json::Value = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            crate::monitoring::record_rpc(t0.elapsed().as_millis() as u64, true);
            debug!(error = %e, "eth_getLogs parse failed");
            return 0;
        }
    };
    crate::monitoring::record_rpc(t0.elapsed().as_millis() as u64, false);

    let logs = match result["result"].as_array() {
        Some(l) => l,
        None => return 0,
    };

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut updated = 0usize;

    for log in logs {
        let addr = match log["address"].as_str() {
            Some(a) => a.to_lowercase(),
            None => continue,
        };

        let topic0 = log["topics"].as_array()
            .and_then(|t| t.first())
            .and_then(|t| t.as_str())
            .unwrap_or("");

        let data = match log["data"].as_str() {
            Some(d) => d.trim_start_matches("0x"),
            None => continue,
        };

        if topic0 == SYNC_TOPIC && v2_addrs.contains(&addr) {
            // V2 Sync: data = (uint112 reserve0, uint112 reserve1)
            if data.len() < 128 { continue; }
            let r0 = u128::from_str_radix(&data[0..64], 16).unwrap_or(0);
            let r1 = u128::from_str_radix(&data[64..128], 16).unwrap_or(0);
            if let Some(mut snap) = pool_cache().get_mut(&addr) {
                snap.reserve0 = r0;
                snap.reserve1 = r1;
                snap.block = to_block;
                snap.fetched_at = now;
                updated += 1;
            }
        } else if topic0 == SWAP_V3_TOPIC && v3_addrs.contains(&addr) {
            // V3 Swap: data = (int256 amount0, int256 amount1, uint160 sqrtPriceX96,
            //                  uint128 liquidity, int24 tick)
            if data.len() < 320 { continue; }
            let sqrt_price = u128::from_str_radix(&data[128..192], 16).unwrap_or(0);
            let liquidity = u128::from_str_radix(&data[192..256], 16).unwrap_or(0);
            // tick is int24 sign-extended in a 32-byte word
            let tick_raw = i64::from_str_radix(&data[256..320], 16).unwrap_or(0);
            let tick = if tick_raw > 0x7FFFFF { tick_raw as i32 - 0x1000000i32 } else { tick_raw as i32 };

            if sqrt_price > 0 {
                if let Some(mut snap) = pool_cache().get_mut(&addr) {
                    snap.sqrt_price_x96 = Some(sqrt_price);
                    snap.tick = Some(tick);
                    snap.v3_liquidity = Some(liquidity);
                    snap.block = to_block;
                    snap.fetched_at = now;
                    updated += 1;
                }
            }
        } else if (topic0 == MINT_TOPIC || topic0 == BURN_TOPIC) && v3_addrs.contains(&addr) {
            // V3 Mint/Burn: update liquidityNet at the tick boundaries
            // Mint topics: [topic0, sender_indexed], data = (owner, tickLower, tickUpper, amount, amount0, amount1)
            // Burn topics: [topic0, owner_indexed], data = (tickLower, tickUpper, amount, amount0, amount1)
            // For both: tickLower and tickUpper define the range affected
            // We update the subgraph tick store with the liquidity delta
            let topics = log["topics"].as_array();
            if data.len() >= 192 {
                // Parse tick range from data (first two int24 words)
                let tick_lower_raw = i64::from_str_radix(&data[0..64], 16).unwrap_or(0);
                let tick_upper_raw = i64::from_str_radix(&data[64..128], 16).unwrap_or(0);
                let tick_lower = if tick_lower_raw > 0x7FFFFF { tick_lower_raw as i32 - 0x1000000 } else { tick_lower_raw as i32 };
                let tick_upper = if tick_upper_raw > 0x7FFFFF { tick_upper_raw as i32 - 0x1000000 } else { tick_upper_raw as i32 };
                let amount = u128::from_str_radix(&data[128..192], 16).unwrap_or(0);

                if amount > 0 && tick_lower != tick_upper {
                    // Update tick data in subgraph store
                    let delta = amount as i128;
                    let sign = if topic0 == MINT_TOPIC { 1i128 } else { -1i128 };
                    if let Some(mut ticks) = crate::subgraph::get_tick_data(&addr) {
                        *ticks.entry(tick_lower).or_insert(0) += delta * sign;
                        *ticks.entry(tick_upper).or_insert(0) -= delta * sign;
                        crate::subgraph::store_tick_data_pub(&addr, &ticks);
                        updated += 1;
                    }
                }
            }
            // Also mark pool as freshly updated
            if let Some(mut snap) = pool_cache().get_mut(&addr) {
                snap.block = to_block;
                snap.fetched_at = now;
            }
        }
    }

    updated
}

/// Fetch slot0() for V3 pools to get sqrtPriceX96, tick, and liquidity.
/// slot0() selector: 0x3850c7bd
/// Returns: (sqrtPriceX96, tick, observationIndex, observationCardinality, ...)
async fn refresh_v3_pools(
    client: &reqwest::Client,
    rpc_url: &str,
    block: u64,
    addresses: &[String],
) {
    if addresses.is_empty() {
        return;
    }

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

    // Also need liquidity() call: selector 0x1a686502
    // Batch both slot0 and liquidity for each pool
    let mut calls: Vec<serde_json::Value> = Vec::new();
    for (i, addr) in addresses.iter().enumerate() {
        // slot0()
        calls.push(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{ "to": addr, "data": "0x3850c7bd" }, "latest"],
            "id": i * 2
        }));
        // liquidity()
        calls.push(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{ "to": addr, "data": "0x1a686502" }, "latest"],
            "id": i * 2 + 1
        }));
    }

    let t0 = std::time::Instant::now();
    let resp = match client.post(rpc_url).json(&calls).send().await {
        Ok(r) => r,
        Err(e) => {
            crate::monitoring::record_rpc(t0.elapsed().as_millis() as u64, true);
            warn!(error = %e, "V3 pool refresh failed");
            return;
        }
    };

    let results: Vec<serde_json::Value> = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            crate::monitoring::record_rpc(t0.elapsed().as_millis() as u64, true);
            warn!(error = %e, "V3 pool refresh parse failed");
            return;
        }
    };
    crate::monitoring::record_rpc(t0.elapsed().as_millis() as u64, false);

    // Parse results in pairs (slot0, liquidity) for each address
    for (i, addr) in addresses.iter().enumerate() {
        let slot0_result = results.iter().find(|r| r["id"].as_u64() == Some((i * 2) as u64));
        let liq_result = results.iter().find(|r| r["id"].as_u64() == Some((i * 2 + 1) as u64));

        let slot0_hex = slot0_result
            .and_then(|r| r["result"].as_str())
            .unwrap_or("");
        let liq_hex = liq_result
            .and_then(|r| r["result"].as_str())
            .unwrap_or("");

        // Parse slot0: first 32 bytes = sqrtPriceX96, next 32 bytes = tick (int24, sign-extended)
        let slot0_data = slot0_hex.trim_start_matches("0x");
        if slot0_data.len() < 128 {
            continue;
        }
        let sqrt_price = u128::from_str_radix(&slot0_data[0..64], 16).unwrap_or(0);
        // tick is int24 packed in a 32-byte word — parse as i32
        let tick_raw = i64::from_str_radix(&slot0_data[64..128], 16).unwrap_or(0);
        let tick = if tick_raw > 0x7FFFFF { tick_raw as i32 - 0x1000000i32 } else { tick_raw as i32 };

        // Parse liquidity: single uint128
        let liq_data = liq_hex.trim_start_matches("0x");
        let liquidity = if liq_data.len() >= 64 {
            u128::from_str_radix(&liq_data[0..64], 16).unwrap_or(0)
        } else {
            0
        };

        if let Some(mut snap) = pool_cache().get_mut(addr.as_str()) {
            snap.sqrt_price_x96 = Some(sqrt_price);
            snap.tick = Some(tick);
            snap.v3_liquidity = Some(liquidity);
            snap.block = block;
            snap.fetched_at = now;
        }
    }
}

// ── Metrics ──────────────────────────────────────────────────────────────────

pub struct IndexerMetrics {
    /// Total block head events processed
    pub blocks_processed: AtomicU64,
    /// Total pool reserve refreshes completed
    pub refreshes_completed: AtomicU64,
    /// Total refresh failures
    pub refresh_failures: AtomicU64,
}

impl IndexerMetrics {
    fn new() -> Self {
        Self {
            blocks_processed: AtomicU64::new(0),
            refreshes_completed: AtomicU64::new(0),
            refresh_failures: AtomicU64::new(0),
        }
    }
}

static INDEXER_METRICS: OnceLock<IndexerMetrics> = OnceLock::new();

pub fn indexer_metrics() -> &'static IndexerMetrics {
    INDEXER_METRICS.get_or_init(IndexerMetrics::new)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snap(addr: &str, block: u64, r0: u128, r1: u128) -> PoolSnapshot {
        PoolSnapshot {
            address: addr.to_string(),
            token0: "0xtoken_a".to_string(),
            token1: "0xtoken_b".to_string(),
            pool_type: PoolType::V2,
            reserve0: r0,
            reserve1: r1,
            sqrt_price_x96: None,
            tick: None,
            v3_liquidity: None,
            fee_tier: None,
            block,
            fetched_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    #[test]
    fn upsert_and_get_pool() {
        let addr = "0xpool_indexer_test_001";
        upsert(make_snap(addr, 100, 1_000_000, 2_000_000));
        let snap = get(addr).expect("pool should be in cache");
        assert_eq!(snap.reserve0, 1_000_000);
        assert_eq!(snap.reserve1, 2_000_000);
        assert_eq!(snap.block, 100);
    }

    #[test]
    fn upsert_updates_existing() {
        let addr = "0xpool_indexer_test_002";
        upsert(make_snap(addr, 100, 1_000_000, 2_000_000));
        upsert(make_snap(addr, 101, 1_500_000, 2_500_000));
        let snap = get(addr).expect("pool should be in cache");
        assert_eq!(snap.reserve0, 1_500_000);
        assert_eq!(snap.block, 101);
    }

    #[test]
    fn set_block_only_advances() {
        let before = current_block();
        set_block(before + 50);
        assert_eq!(current_block(), before + 50);

        // Going backwards should be ignored
        set_block(before + 10);
        assert_eq!(current_block(), before + 50);

        // Advance further
        set_block(before + 100);
        assert_eq!(current_block(), before + 100);
    }

    #[test]
    fn get_missing_pool_returns_none() {
        let snap = get("0xnonexistent_pool_xyz999");
        assert!(snap.is_none());
    }

    #[test]
    fn pool_count_reflects_cache_size() {
        let before = pool_count();
        upsert(make_snap("0xcount_test_pool_a", 100, 100, 200));
        upsert(make_snap("0xcount_test_pool_b", 100, 300, 400));
        assert!(pool_count() >= before + 2);
    }

    #[test]
    fn snapshot_age_calculation() {
        let snap = PoolSnapshot {
            address: "0xage_test".to_string(),
            token0: "0xa".to_string(),
            token1: "0xb".to_string(),
            pool_type: PoolType::V2,
            reserve0: 0,
            reserve1: 0,
            sqrt_price_x96: None, tick: None, v3_liquidity: None, fee_tier: None,
            block: 1,
            fetched_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .saturating_sub(10), // 10 seconds ago
        };
        assert!(snap.age_secs() >= 10);
        assert!(!snap.is_stale()); // 10s < 600s threshold

        let old_snap = PoolSnapshot {
            fetched_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .saturating_sub(700), // 700 seconds ago (> 600s threshold)
            ..snap
        };
        assert!(old_snap.is_stale());
    }

    #[test]
    fn indexer_disabled_via_env() {
        // Can't fully test without spawning, but verify the kill switch reads correctly
        // Default should be enabled
        // (We avoid setting env vars in tests to prevent interference)
        let _ = is_enabled(); // just verify it doesn't panic
    }

    #[test]
    fn cached_addresses_returns_inserted_keys() {
        let addr = "0xaddr_list_test_pool";
        upsert(make_snap(addr, 100, 1, 2));
        let addrs = cached_addresses();
        assert!(addrs.contains(&addr.to_string()));
    }
}
