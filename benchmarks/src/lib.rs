//! Benchmark helpers: fixture construction and data loading utilities.

use std::path::PathBuf;

use solver_engine::models::{
    auction::AuctionInstance,
    liquidity::{LiquiditySource, PoolKind, UniswapV2Pool},
    order::{Order, OrderClass, OrderKind},
};
use solver_engine::liquidity::registry::PoolRegistry;

// Well-known token addresses (mainnet)
pub const WETH: &str = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
pub const USDC: &str = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
pub const DAI: &str  = "0x6b175474e89094c44da98b954eedeac495271d0f";
pub const WBTC: &str = "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599";
pub const USDT: &str = "0xdac17f958d2ee523a2206206994597c13d831ec7";

// ─── Auction fixtures ────────────────────────────────────────────────────────

/// Build a synthetic `AuctionInstance` with `n` sell orders (WETH → USDC).
pub fn make_auction(n: usize) -> AuctionInstance {
    let orders = (0..n)
        .map(|i| make_sell_order(i as u64, WETH, USDC, 1_000_000_000_000_000_000, 3_700_000_000))
        .collect();
    AuctionInstance {
        id: 1,
        tokens: Default::default(),
        orders,
        liquidity: vec![],
        effective_gas_price: "30000000000".to_string(),
        deadline: None,
        chain_id: Some(1),
        block: None,
    }
}

/// Build a single sell order with all required fields.
pub fn make_sell_order(
    idx: u64,
    sell_token: &str,
    buy_token: &str,
    sell_amount: u128,
    buy_amount: u128,
) -> Order {
    Order {
        uid: format!("0x{:064x}{:016x}", idx, idx),
        sell_token: sell_token.to_string(),
        buy_token: buy_token.to_string(),
        sell_amount: sell_amount.to_string(),
        buy_amount: buy_amount.to_string(),
        fee_amount: "0".to_string(),
        kind: OrderKind::Sell,
        partially_fillable: false,
        class: OrderClass::Market,
        sell_token_balance: None,
        buy_token_balance: None,
        signing_scheme: None,
        signature: None,
        receiver: None,
        app_data: None,
        valid_to: None,
    }
}

/// Build a `CoW` pair: order_a sells WETH for USDC, order_b sells USDC for WETH.
pub fn make_cow_auction() -> AuctionInstance {
    let orders = vec![
        make_sell_order(1, WETH, USDC, 1_000_000_000_000_000_000, 3_700_000_000),
        make_sell_order(2, USDC, WETH, 3_700_000_000, 900_000_000_000_000_000),
    ];
    AuctionInstance {
        id: 2,
        tokens: Default::default(),
        orders,
        liquidity: vec![],
        effective_gas_price: "30000000000".to_string(),
        deadline: None,
        chain_id: Some(1),
        block: None,
    }
}

// ─── Pool registry fixtures ──────────────────────────────────────────────────

/// Build a `UniswapV2Pool` for a given token pair and reserves.
pub fn make_v2_pool(token0: &str, token1: &str, r0: u128, r1: u128) -> LiquiditySource {
    LiquiditySource::UniswapV2(UniswapV2Pool {
        address: format!("0x{:040x}", r0 ^ r1),
        kind: PoolKind::UniswapV2,
        token0: token0.to_string(),
        token1: token1.to_string(),
        reserve0: r0.to_string(),
        reserve1: r1.to_string(),
        fee_bps: 30,
    })
}

/// Build a pool registry with `n_pools` synthetic WETH/USDC pools.
pub fn make_registry(n_pools: usize) -> PoolRegistry {
    let mut reg = PoolRegistry::new();
    for i in 0..n_pools {
        let base_r0 = 1_000_000_000_000_000_000u128 * (i as u128 + 1);
        let base_r1 = 3_700_000_000u128 * (i as u128 + 1);
        reg.add_pool(make_v2_pool(WETH, USDC, base_r0, base_r1));
    }
    // Add USDC/DAI and WETH/DAI pairs for multi-hop
    for i in 0..n_pools / 2 {
        let base = i as u128 + 1;
        reg.add_pool(make_v2_pool(
            USDC,
            DAI,
            5_000_000_000u128 * base,
            5_000_000_000_000_000_000_000u128 * base,
        ));
        reg.add_pool(make_v2_pool(
            WETH,
            DAI,
            1_000_000_000_000_000_000u128 * base,
            3_700_000_000_000_000_000_000u128 * base,
        ));
    }
    reg
}

// ─── Historical auction loading ──────────────────────────────────────────────

/// Load all JSON auction fixtures from `benchmarks/data/auctions/`.
/// Returns `(auction, winning_score_str)` pairs.
/// `winning_score_str` is the score the historical winner achieved (decimal string).
pub fn load_historical_auctions() -> Vec<(AuctionInstance, String)> {
    let data_dir = auction_data_dir();
    let mut results = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&data_dir) {
        let mut paths: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        paths.sort_by_key(|e| e.file_name());

        for entry in paths {
            let content = match std::fs::read_to_string(entry.path()) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let v: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let winning_score = v
                .get("winning_score")
                .and_then(|s| s.as_str())
                .unwrap_or("0")
                .to_string();

            // Strip the benchmark envelope to get just the auction fields
            let auction_val = if v.get("auction").is_some() {
                v["auction"].clone()
            } else {
                v.clone()
            };
            if let Ok(auction) = serde_json::from_value::<AuctionInstance>(auction_val) {
                results.push((auction, winning_score));
            }
        }
    }
    results
}

/// Returns path to `benchmarks/data/auctions/` relative to the workspace root.
fn auction_data_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR points to `benchmarks/`; data is one level down.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("data").join("auctions")
}
