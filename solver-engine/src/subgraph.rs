//! The Graph subgraph integration for Uniswap V3 tick data on Arbitrum.
//!
//! Fetches sqrtPrice, tick, liquidity, and liquidityNet per initialized tick
//! from the official Uniswap V3 Arbitrum subgraph. This is the same data source
//! the CoW Protocol reference implementation uses (graph_api.rs).
//!
//! Two-layer update strategy (from CoW reference):
//! 1. Full snapshot: fetch all tick data at startup via paginated GraphQL queries
//! 2. Incremental: on-chain Swap/Mint/Burn events update state between snapshots
//!
//! Subgraph: FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

// ── Config ──────────────────────────────────────────────────────────────────

const DEFAULT_SUBGRAPH_ID: &str = "FbCGRftH4a3yZugY7TnbYgPJVEv2LvMT6oF1fxPe9aJM";
const PAGE_SIZE: usize = 1000;
const MAX_PAGES: usize = 50; // safety limit

/// Build the subgraph query URL from env vars.
fn subgraph_url() -> Option<String> {
    let api_key = std::env::var("THEGRAPH_API_KEY")
        .or_else(|_| std::env::var("thegraph_api_key"))
        .ok()?;
    if api_key.is_empty() {
        return None;
    }
    let subgraph_id = std::env::var("THEGRAPH_SUBGRAPH_ID")
        .unwrap_or_else(|_| DEFAULT_SUBGRAPH_ID.to_string());
    Some(format!(
        "https://gateway.thegraph.com/api/{}/subgraphs/id/{}",
        api_key, subgraph_id
    ))
}

// ── GraphQL Response Types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GraphResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphError>>,
}

#[derive(Debug, Deserialize)]
struct GraphError {
    message: String,
}

// Pool state query
#[derive(Debug, Deserialize)]
struct PoolsData {
    pools: Vec<GraphPool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphPool {
    id: String,
    token0: GraphToken,
    token1: GraphToken,
    sqrt_price: String,
    tick: Option<String>,
    liquidity: String,
    fee_tier: String,
    total_value_locked_usd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphToken {
    id: String,
}

// Tick data query
#[derive(Debug, Deserialize)]
struct TicksData {
    ticks: Vec<GraphTick>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphTick {
    id: String,
    tick_idx: String,
    liquidity_net: String,
    pool_address: Option<String>,
    pool: Option<GraphTickPool>,
}

#[derive(Debug, Deserialize)]
struct GraphTickPool {
    id: String,
}

// ── Public Types ────────────────────────────────────────────────────────────

/// V3 pool state with tick data from the subgraph.
#[derive(Debug, Clone)]
pub struct V3PoolState {
    pub address: String,
    pub token0: String,
    pub token1: String,
    pub sqrt_price_x96: u128,
    pub tick: i32,
    pub liquidity: u128,
    pub fee_tier: u32,
    /// Map of tick index → liquidityNet (signed). Only initialized ticks.
    pub ticks: HashMap<i32, i128>,
}

// ── Fetcher ─────────────────────────────────────────────────────────────────

/// Fetch top V3 pools from the subgraph.
///
/// Returns pools sorted by TVL descending. `max_pools` limits the result.
pub async fn fetch_top_pools(max_pools: usize) -> Vec<V3PoolState> {
    let url = match subgraph_url() {
        Some(u) => u,
        None => {
            debug!("Subgraph: no API key configured (THEGRAPH_API_KEY)");
            return vec![];
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    // Step 1: Fetch top pools by TVL
    let pools_query = format!(
        r#"{{
            pools(
                first: {},
                orderBy: totalValueLockedUSD,
                orderDirection: desc,
                where: {{ totalValueLockedUSD_gt: "1000" }}
            ) {{
                id
                token0 {{ id }}
                token1 {{ id }}
                sqrtPrice
                tick
                liquidity
                feeTier
                totalValueLockedUSD
            }}
        }}"#,
        max_pools
    );

    let pools = match query_subgraph::<PoolsData>(&client, &url, &pools_query).await {
        Some(data) => data.pools,
        None => return vec![],
    };

    info!(pools = pools.len(), "Subgraph: fetched V3 pool states");

    // Step 2: For each pool, fetch all initialized ticks (paginated)
    let pool_ids: Vec<&str> = pools.iter().map(|p| p.id.as_str()).collect();
    let all_ticks = fetch_ticks_for_pools(&client, &url, &pool_ids).await;

    // Step 3: Combine into V3PoolState structs
    let mut result = Vec::with_capacity(pools.len());
    for pool in &pools {
        let sqrt_price = pool.sqrt_price.parse::<u128>().unwrap_or(0);
        let tick = pool.tick.as_ref()
            .and_then(|t| t.parse::<i32>().ok())
            .unwrap_or(0);
        let liquidity = pool.liquidity.parse::<u128>().unwrap_or(0);
        let fee_tier = pool.fee_tier.parse::<u32>().unwrap_or(3000);

        let ticks = all_ticks.get(pool.id.as_str())
            .cloned()
            .unwrap_or_default();

        if sqrt_price == 0 {
            continue;
        }

        result.push(V3PoolState {
            address: pool.id.clone(),
            token0: pool.token0.id.clone(),
            token1: pool.token1.id.clone(),
            sqrt_price_x96: sqrt_price,
            tick,
            liquidity,
            fee_tier,
            ticks,
        });
    }

    info!(
        pools_with_ticks = result.iter().filter(|p| !p.ticks.is_empty()).count(),
        total_pools = result.len(),
        "Subgraph: V3 pools with tick data ready"
    );

    result
}

/// Fetch initialized ticks for a set of pools via paginated queries.
async fn fetch_ticks_for_pools(
    client: &reqwest::Client,
    url: &str,
    pool_ids: &[&str],
) -> HashMap<String, HashMap<i32, i128>> {
    let mut result: HashMap<String, HashMap<i32, i128>> = HashMap::new();

    // Format pool IDs for GraphQL
    let ids_str = pool_ids.iter()
        .map(|id| format!("\"{}\"", id))
        .collect::<Vec<_>>()
        .join(", ");

    let mut last_id = String::new();
    let mut total_ticks = 0usize;

    for page in 0..MAX_PAGES {
        let query = format!(
            r#"{{
                ticks(
                    first: {},
                    where: {{
                        id_gt: "{}",
                        liquidityNet_not: "0",
                        pool_: {{ id_in: [{}] }}
                    }},
                    orderBy: id
                ) {{
                    id
                    tickIdx
                    liquidityNet
                    pool {{ id }}
                }}
            }}"#,
            PAGE_SIZE, last_id, ids_str
        );

        let ticks = match query_subgraph::<TicksData>(client, url, &query).await {
            Some(data) => data.ticks,
            None => break,
        };

        if ticks.is_empty() {
            break;
        }

        for tick in &ticks {
            let pool_addr = tick.pool.as_ref()
                .map(|p| p.id.as_str())
                .or(tick.pool_address.as_deref())
                .unwrap_or("");

            let tick_idx: i32 = tick.tick_idx.parse().unwrap_or(0);
            let liquidity_net: i128 = tick.liquidity_net.parse().unwrap_or(0);

            if !pool_addr.is_empty() && liquidity_net != 0 {
                result.entry(pool_addr.to_string())
                    .or_default()
                    .insert(tick_idx, liquidity_net);
            }
        }

        total_ticks += ticks.len();
        last_id = ticks.last().map(|t| t.id.clone()).unwrap_or_default();

        if ticks.len() < PAGE_SIZE {
            break; // Last page
        }

        debug!(page = page + 1, ticks = ticks.len(), "Subgraph: tick page fetched");
    }

    info!(total_ticks, pools_with_ticks = result.len(), "Subgraph: tick data fetch complete");
    result
}

/// Execute a GraphQL query against the subgraph.
async fn query_subgraph<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    query: &str,
) -> Option<T> {
    let body = serde_json::json!({ "query": query });

    let resp = match client.post(url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "Subgraph query failed");
            return None;
        }
    };

    let result: GraphResponse<T> = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "Subgraph response parse failed");
            return None;
        }
    };

    if let Some(errors) = &result.errors {
        for err in errors {
            warn!(error = %err.message, "Subgraph GraphQL error");
        }
        return None;
    }

    result.data
}

/// Background task: periodically refresh V3 tick data from the subgraph
/// and update the pool cache.
pub async fn run_subgraph_updater() {
    let url = match subgraph_url() {
        Some(_) => {
            info!("Subgraph updater started — will fetch V3 tick data every 30s");
        }
        None => {
            info!("Subgraph updater disabled — no THEGRAPH_API_KEY configured");
            return;
        }
    };

    // Initial fetch: get top 100 pools with all tick data
    let pools = fetch_top_pools(100).await;
    update_pool_cache(&pools);

    // Refresh loop: every 30 seconds, re-fetch pool states
    // (ticks change less frequently, full tick refresh every 5 minutes)
    let mut tick_refresh_counter = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        tick_refresh_counter += 1;

        // Quick refresh: just pool states (sqrtPrice, tick, liquidity) — no tick data
        // Full tick refresh every 10th cycle (5 minutes)
        let max_pools = if tick_refresh_counter >= 10 {
            tick_refresh_counter = 0;
            info!("Subgraph: full tick data refresh");
            100 // full refresh with ticks
        } else {
            50 // quick refresh, fewer pools
        };

        let pools = fetch_top_pools(max_pools).await;
        if !pools.is_empty() {
            update_pool_cache(&pools);
        }
    }
}

/// Update the pool indexer cache with subgraph data.
fn update_pool_cache(pools: &[V3PoolState]) {
    use crate::pool_indexer::{self, PoolSnapshot, PoolType};

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let block = pool_indexer::current_block();

    let mut updated = 0usize;
    let mut with_ticks = 0usize;

    for pool in pools {
        let addr = pool.address.to_lowercase();

        if let Some(mut snap) = pool_indexer::pool_cache().get_mut(&addr) {
            // Update existing pool
            snap.pool_type = PoolType::V3;
            snap.sqrt_price_x96 = Some(pool.sqrt_price_x96);
            snap.tick = Some(pool.tick);
            snap.v3_liquidity = Some(pool.liquidity);
            snap.fee_tier = Some(pool.fee_tier);
            snap.block = block;
            snap.fetched_at = now;
            updated += 1;
        } else {
            // New pool from subgraph — add to cache
            pool_indexer::upsert(PoolSnapshot {
                address: addr.clone(),
                token0: pool.token0.clone(),
                token1: pool.token1.clone(),
                pool_type: PoolType::V3,
                reserve0: 0,
                reserve1: 0,
                sqrt_price_x96: Some(pool.sqrt_price_x96),
                tick: Some(pool.tick),
                v3_liquidity: Some(pool.liquidity),
                fee_tier: Some(pool.fee_tier),
                block,
                fetched_at: now,
            });
            updated += 1;
        }

        // Store tick data for this pool (used by cached_as_liquidity)
        if !pool.ticks.is_empty() {
            store_tick_data(&addr, &pool.ticks);
            with_ticks += 1;
        }
    }

    if updated > 0 {
        info!(updated, with_ticks, "Subgraph: pool cache updated");
    }
}

// ── Tick Data Storage ───────────────────────────────────────────────────────

use std::sync::OnceLock;
use std::sync::Mutex;

/// Global tick data storage: pool_address → { tick_idx → liquidityNet }
static TICK_DATA: OnceLock<Mutex<HashMap<String, HashMap<i32, i128>>>> = OnceLock::new();

fn tick_store() -> &'static Mutex<HashMap<String, HashMap<i32, i128>>> {
    TICK_DATA.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Store tick data (called internally and from pool_indexer for Mint/Burn events).
pub fn store_tick_data_pub(pool_addr: &str, ticks: &HashMap<i32, i128>) {
    store_tick_data(pool_addr, ticks);
}

fn store_tick_data(pool_addr: &str, ticks: &HashMap<i32, i128>) {
    if let Ok(mut store) = tick_store().lock() {
        store.insert(pool_addr.to_string(), ticks.clone());
    }
}

/// Get tick data for a pool (used by cached_as_liquidity to export with tick data).
pub fn get_tick_data(pool_addr: &str) -> Option<HashMap<i32, i128>> {
    tick_store().lock().ok()?.get(pool_addr).cloned()
}
