//! Pool Discovery — External satellite that feeds the solver
//!
//! Two modes:
//! 1. **File loading** (solver startup) — reads `data/discovered_pools.json` and seeds the pool indexer.
//!    Zero RPC calls, zero latency impact.
//! 2. **Live discovery** (standalone binary or dashboard trigger) — queries factory contracts,
//!    writes results to `data/discovered_pools.json`, then hot-reloads into the pool indexer.
//!
//! The solver never calls factory contracts during normal operation.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::pool_indexer;

// ── Discovered pool file format ──────────────────────────────────────────────

/// A discovered pool entry (stored in JSON file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPool {
    pub address: String,
    pub token0: String,
    pub token1: String,
    pub factory: String,
    pub pool_type: String, // "v2" or "v3"
}

/// The full discovery result file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResult {
    pub chain_id: u64,
    pub discovered_at: String, // ISO 8601
    pub pool_count: usize,
    pub pools: Vec<DiscoveredPool>,
}

// ── File path ────────────────────────────────────────────────────────────────

fn pools_file_path() -> PathBuf {
    std::env::var("DISCOVERED_POOLS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/discovered_pools.json"))
}

// ── Global discovery lock (prevents concurrent runs) ─────────────────────────

static DISCOVERY_RUNNING: AtomicBool = AtomicBool::new(false);

/// Check if a discovery is currently in progress.
pub fn is_running() -> bool {
    DISCOVERY_RUNNING.load(Ordering::Relaxed)
}

/// Get the last discovery timestamp and pool count from the file.
pub fn last_discovery_info() -> Option<(String, usize)> {
    let path = pools_file_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let result: DiscoveryResult = serde_json::from_str(&data).ok()?;
    Some((result.discovered_at, result.pool_count))
}

// ── Mode 1: Load from file (solver startup) ─────────────────────────────────

/// Load discovered pools from JSON file and seed the pool indexer.
/// Called once at solver startup. Zero RPC calls.
pub fn load_from_file() -> usize {
    let path = pools_file_path();

    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            info!(path = %path.display(), error = %e, "No discovered pools file found — using bootstrap pools only");
            return 0;
        }
    };

    let result: DiscoveryResult = match serde_json::from_str(&data) {
        Ok(r) => r,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "Failed to parse discovered pools file");
            return 0;
        }
    };

    let mut loaded = 0usize;
    for pool in &result.pools {
        if pool_indexer::get(&pool.address).is_none() {
            let pt = if pool.pool_type == "v3" {
                pool_indexer::PoolType::V3
            } else {
                pool_indexer::PoolType::V2
            };
            // Parse fee tier from factory name for V3 pools
            let fee_tier = if pt == pool_indexer::PoolType::V3 {
                // Default to 3000 bps (0.3%) — most common V3 fee tier
                Some(3000u32)
            } else {
                None
            };
            pool_indexer::upsert(pool_indexer::PoolSnapshot {
                address: pool.address.clone(),
                token0: pool.token0.clone(),
                token1: pool.token1.clone(),
                pool_type: pt,
                reserve0: 0,
                reserve1: 0,
                sqrt_price_x96: None,
                tick: None,
                v3_liquidity: None,
                fee_tier,
                block: 0,
                fetched_at: 0,
            });
            loaded += 1;
        }
    }

    info!(
        file_pools = result.pool_count,
        loaded = loaded,
        discovered_at = %result.discovered_at,
        "Loaded discovered pools from file"
    );
    loaded
}

// ── Mode 2: Live discovery (standalone or dashboard trigger) ─────────────────

// Arbitrum Factory Addresses — V2-style (allPairsLength + allPairs)
const V2_FACTORIES: &[(&str, &str)] = &[
    ("0xc35dadb65012ec5796536bd9864ed8773abc74c4", "SushiSwap"),
    ("0xf1d7cc64fb4452f05c498126312ebe29f30cffc1", "UniswapV2"),
    ("0x6eccab422d763ac031210895c81787e87b43a652", "CamelotV2"),
    ("0x8e42f2f4101563bf679975178e880fd87d3efd4e", "TraderJoeV2.1"), // LBFactory — also supports allPairs enumeration
];

// V3-style factories that use getPool(tokenA, tokenB, fee)
const V3_FACTORIES: &[(&str, &str, &[u32])] = &[
    // Uniswap V3: 4 fee tiers
    ("0x1f98431c8ad98523631ae4a59f267346ea31f984", "UniswapV3", &[100, 500, 3000, 10000]),
    // Camelot V3 (Algebra): uses poolByPair(tokenA, tokenB) — no fee tier param.
    // We'll query getPool with fee=0 as a workaround (Algebra ignores the fee param).
    ("0x1a3c9b1d2f0529d97f2afc5136cc23e58f1fd35d", "CamelotV3", &[0]),
];

// Top tokens on Arbitrum for V3 pair discovery — 23 tokens
const DISCOVERY_TOKENS: &[&str] = &[
    // ── Tier 1: Core DeFi (highest volume) ──────────────────────────────
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1", // WETH
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831", // USDC (native)
    "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8", // USDC.e (bridged)
    "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9", // USDT
    "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f", // WBTC
    "0x912ce59144191c1204e64559fe8253a0e49e6548", // ARB
    "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1", // DAI
    // ── Tier 2: Major DeFi tokens ───────────────────────────────────────
    "0xfc5a1a6eb076a2c7ad06ed22c90d7e710e35ad0a", // GMX
    "0x5979d7b546e38e414f7e9822514be443a4800529", // wstETH
    "0xf97f4df75117a78c1a5a0dbb814af92458539fb4", // LINK
    "0x17fc002b466eec40dae837fc4be5c67993ddbd6f", // FRAX
    "0xfea7a6a0b346362bf88cf9a27b7abad8d69c6c0",  // MIM
    "0x3d9907f9a368ad0a51be60f7da3b97cf940982d8", // GRAIL
    // ── Tier 3: Popular Arbitrum tokens ─────────────────────────────────
    "0xfa7f8980b0f1e64a2062791cc3b0871572f1f7f0", // UNI
    "0x0c880f6761f1af8d9aa9c466984b80dab9a8c9e8", // PENDLE
    "0x3082cc23568ea640225c2467653db90e9250aaa0", // RDNT (Radiant)
    "0x539bde0d7dbd336b79148aa742883198bbf60342", // MAGIC
    "0xec70dcb4a1efa46b8f2d97c310c9c4790ba5ffa8", // rETH
    "0x6694340fc020c5e6b96567843da2df01b2ce1eb6", // STG (Stargate)
    "0x4e352cf164e64adcbad318c3a1e222e9eba4ce29", // MCB (MCDEX)
    "0x3f56e0c36d275367b8c502090edf38289b3dea0d", // MAI (miMATIC)
    "0x11cdb42b0eb46d95f990bedd4695a6e3fa034978", // CRV
    "0x6c2c06790b3e3e3c38e12ee22f8183b37a13ee55", // DPX (Dopex)
];

// ABI selectors
const ALL_PAIRS_LENGTH_SELECTOR: &str = "0x574f2ba3";
const ALL_PAIRS_SELECTOR: &str = "0x1e3dd18b";
const GET_POOL_SELECTOR: &str = "0x1698ee82";

/// Run live discovery: query factories, save to file, hot-reload into pool indexer.
/// Returns the number of pools discovered, or error message.
pub async fn run_discovery(rpc_url: &str, chain_id: u64) -> Result<usize, String> {
    if chain_id != 42161 {
        return Err("Pool discovery only supported on Arbitrum (42161)".to_string());
    }

    if DISCOVERY_RUNNING.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err("Discovery already in progress".to_string());
    }

    let result = run_discovery_inner(rpc_url, chain_id).await;

    DISCOVERY_RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn run_discovery_inner(rpc_url: &str, chain_id: u64) -> Result<usize, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let mut all_pools: Vec<DiscoveredPool> = Vec::new();

    // Discover V2-style pools
    for (factory_addr, factory_name) in V2_FACTORIES {
        match discover_v2_pools(&client, rpc_url, factory_addr, factory_name).await {
            Ok(pools) => {
                info!(factory = factory_name, pools = pools.len(), "V2 factory discovery complete");
                all_pools.extend(pools);
            }
            Err(e) => warn!(factory = factory_name, error = %e, "V2 factory discovery failed"),
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }

    // Discover V3-style pools from all V3 factories
    for (factory_addr, factory_name, fee_tiers) in V3_FACTORIES {
        match discover_v3_pools(&client, rpc_url, factory_addr, factory_name, fee_tiers).await {
            Ok(pools) => {
                info!(factory = *factory_name, pools = pools.len(), "V3 factory discovery complete");
                all_pools.extend(pools);
            }
            Err(e) => warn!(factory = *factory_name, error = %e, "V3 factory discovery failed"),
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }

    // Deduplicate by address
    all_pools.sort_by(|a, b| a.address.cmp(&b.address));
    all_pools.dedup_by(|a, b| a.address == b.address);

    let count = all_pools.len();

    // Save to file
    let result = DiscoveryResult {
        chain_id,
        discovered_at: chrono_now_iso(),
        pool_count: count,
        pools: all_pools,
    };

    let path = pools_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(&result)
        .map_err(|e| format!("JSON serialize error: {e}"))?;
    std::fs::write(&path, &json)
        .map_err(|e| format!("File write error: {e}"))?;

    info!(pools = count, path = %path.display(), "Discovery results saved to file");

    // Hot-reload into pool indexer
    let loaded = load_from_file();
    info!(loaded = loaded, "Hot-reloaded discovered pools into indexer");

    Ok(count)
}

fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple ISO-ish format without chrono dependency
    format!("{}Z", secs)
}

// ── V2 Factory Discovery ─────────────────────────────────────────────────────

async fn discover_v2_pools(
    client: &reqwest::Client,
    rpc_url: &str,
    factory_addr: &str,
    factory_name: &str,
) -> Result<Vec<DiscoveredPool>, String> {
    let length = call_uint256(client, rpc_url, factory_addr, ALL_PAIRS_LENGTH_SELECTOR, "").await?;
    if length == 0 {
        return Ok(vec![]);
    }

    let max_pairs = length.min(500) as usize;
    info!(factory = factory_name, total_pairs = length, fetching = max_pairs, "Enumerating V2 pairs");

    let mut pools = Vec::new();
    let batch_size = 50;

    for batch_start in (0..max_pairs).step_by(batch_size) {
        let batch_end = (batch_start + batch_size).min(max_pairs);
        let calls: Vec<serde_json::Value> = (batch_start..batch_end)
            .map(|i| {
                let data = format!("{}{:064x}", ALL_PAIRS_SELECTOR, i);
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "eth_call",
                    "params": [{"to": factory_addr, "data": data}, "latest"],
                    "id": i
                })
            })
            .collect();

        let resp = client.post(rpc_url).json(&calls).send().await
            .map_err(|e| format!("RPC error: {e}"))?;
        let results: Vec<serde_json::Value> = resp.json().await
            .map_err(|e| format!("JSON parse error: {e}"))?;

        for result in &results {
            let hex = result["result"].as_str().unwrap_or("");
            if let Some(addr) = parse_address_from_hex(hex) {
                if addr != "0x0000000000000000000000000000000000000000" {
                    pools.push(DiscoveredPool {
                        address: addr,
                        token0: String::new(), // filled by getReserves later
                        token1: String::new(),
                        factory: factory_name.to_string(),
                        pool_type: "v2".to_string(),
                    });
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Ok(pools)
}

// ── V3 Pool Discovery ────────────────────────────────────────────────────────

async fn discover_v3_pools(
    client: &reqwest::Client,
    rpc_url: &str,
    factory_addr: &str,
    factory_name: &str,
    fee_tiers: &[u32],
) -> Result<Vec<DiscoveredPool>, String> {
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for (i, &token_a) in DISCOVERY_TOKENS.iter().enumerate() {
        for &token_b in DISCOVERY_TOKENS.iter().skip(i + 1) {
            pairs.push((token_a, token_b));
        }
    }

    info!(factory = factory_name, pairs = pairs.len(), fee_tiers = fee_tiers.len(), "Discovering V3 pools");

    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut call_meta: Vec<(&str, &str, u32)> = Vec::new();

    for (token_a, token_b) in &pairs {
        for &fee in fee_tiers {
            let data = format!(
                "{}{}{}{}",
                GET_POOL_SELECTOR,
                encode_address(token_a),
                encode_address(token_b),
                format!("{:064x}", fee),
            );
            calls.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_call",
                "params": [{"to": factory_addr, "data": data}, "latest"],
                "id": calls.len()
            }));
            call_meta.push((token_a, token_b, fee));
        }
    }

    let mut pools = Vec::new();
    let batch_size = 100;

    for batch_start in (0..calls.len()).step_by(batch_size) {
        let batch_end = (batch_start + batch_size).min(calls.len());
        let batch: Vec<serde_json::Value> = calls[batch_start..batch_end].to_vec();

        let resp = client.post(rpc_url).json(&batch).send().await
            .map_err(|e| format!("RPC error: {e}"))?;
        let results: Vec<serde_json::Value> = resp.json().await
            .map_err(|e| format!("JSON parse error: {e}"))?;

        for result in &results {
            let id = result["id"].as_u64().unwrap_or(0) as usize;
            let hex = result["result"].as_str().unwrap_or("");

            if let Some(addr) = parse_address_from_hex(hex) {
                if addr != "0x0000000000000000000000000000000000000000" {
                    if let Some((token_a, token_b, _fee)) = call_meta.get(id) {
                        pools.push(DiscoveredPool {
                            address: addr,
                            token0: token_a.to_string(),
                            token1: token_b.to_string(),
                            factory: factory_name.to_string(),
                            pool_type: "v3".to_string(),
                        });
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Ok(pools)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn call_uint256(
    client: &reqwest::Client,
    rpc_url: &str,
    contract: &str,
    selector: &str,
    extra_data: &str,
) -> Result<u64, String> {
    let data = format!("{}{}", selector, extra_data);
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{"to": contract, "data": data}, "latest"],
        "id": 1
    });

    let resp = client.post(rpc_url).json(&call).send().await
        .map_err(|e| format!("RPC error: {e}"))?;
    let result: serde_json::Value = resp.json().await
        .map_err(|e| format!("JSON parse error: {e}"))?;

    let hex = result["result"].as_str().unwrap_or("0x0");
    let trimmed = hex.trim_start_matches("0x");

    if trimmed.len() > 16 {
        u64::from_str_radix(&trimmed[trimmed.len() - 16..], 16)
            .map_err(|e| format!("Hex parse error: {e}"))
    } else {
        u64::from_str_radix(trimmed, 16)
            .map_err(|e| format!("Hex parse error: {e}"))
    }
}

fn parse_address_from_hex(hex: &str) -> Option<String> {
    let trimmed = hex.trim_start_matches("0x");
    if trimmed.len() < 40 {
        return None;
    }
    let addr_hex = &trimmed[trimmed.len().saturating_sub(40)..];
    Some(format!("0x{}", addr_hex.to_lowercase()))
}

fn encode_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_address_from_abi_encoded() {
        let hex = "0x000000000000000000000000abcdef1234567890abcdef1234567890abcdef12";
        assert_eq!(
            parse_address_from_hex(hex),
            Some("0xabcdef1234567890abcdef1234567890abcdef12".to_string())
        );
    }

    #[test]
    fn parse_zero_address() {
        let hex = "0x0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(
            parse_address_from_hex(hex),
            Some("0x0000000000000000000000000000000000000000".to_string())
        );
    }

    #[test]
    fn encode_address_pads_correctly() {
        let encoded = encode_address("0x82af49447d8a07e3bd95bd0d56f35241523fbab1");
        assert_eq!(encoded.len(), 64);
        assert!(encoded.ends_with("82af49447d8a07e3bd95bd0d56f35241523fbab1"));
    }

    #[test]
    fn discovery_token_pairs_count() {
        // 23 tokens → 23*22/2 = 253 unique pairs
        let n = DISCOVERY_TOKENS.len();
        let pairs = n * (n - 1) / 2;
        assert_eq!(pairs, 253);
    }

    #[test]
    fn load_from_file_returns_zero_when_missing() {
        // No file exists in test env — should return 0 gracefully
        assert_eq!(load_from_file(), 0);
    }
}
