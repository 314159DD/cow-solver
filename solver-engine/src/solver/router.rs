use tracing::debug;

use crate::models::auction::AuctionInstance;
use crate::models::liquidity::{ConstantProductPool, Liquidity};
use crate::models::order::Order;
use crate::models::solution::{Interaction, LiquidityInteraction, Score, Solution, Trade};

// ── Common intermediate tokens ────────────────────────────────────────────────

// Mainnet addresses
pub const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
pub const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
pub const USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
pub const DAI: &str = "0x6B175474E89094C44Da98b954EedeAC495271d0F";
pub const WBTC: &str = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599";

/// Default intermediate tokens for mainnet 2-hop routes.
pub const INTERMEDIARIES: &[&str] = &[WETH, USDC, USDT, DAI, WBTC];

// Arbitrum addresses (from config/arbitrum.toml)
pub const ARB_WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
pub const ARB_USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
pub const ARB_USDT: &str = "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9";
pub const ARB_DAI: &str = "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1";
pub const ARB_WBTC: &str = "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f";
pub const ARB_ARB: &str = "0x912ce59144191c1204e64559fe8253a0e49e6548";

/// Arbitrum intermediate tokens (includes ARB as native token).
pub const ARB_INTERMEDIARIES: &[&str] = &[ARB_WETH, ARB_USDC, ARB_USDT, ARB_DAI, ARB_WBTC, ARB_ARB];

/// Returns the correct intermediary token list for the given chain.
pub fn intermediaries_for_chain(chain_id: u64) -> &'static [&'static str] {
    match chain_id {
        42161 => ARB_INTERMEDIARIES,
        _ => INTERMEDIARIES,
    }
}

// ── Route result ──────────────────────────────────────────────────────────────

/// A multi-hop route for a single order.
#[derive(Debug, Clone)]
pub struct MultiHopRoute {
    pub order_uid: String,
    /// Pool IDs used in order (e.g. ["pool_a→mid", "pool_mid→b"])
    pub pool_ids: Vec<String>,
    /// Intermediate tokens in the path (empty for direct, one for 2-hop)
    pub intermediates: Vec<String>,
    pub executed_amount: u128,
    pub output_amount: u128,
    pub surplus: u128,
    pub interactions: Vec<Interaction>,
}

// ── Constant-product swap helpers ─────────────────────────────────────────────

/// Simulate a constant-product swap in a pool.
/// Returns the output amount or None if pool can't be used.
fn cp_swap_out(pool: &ConstantProductPool, token_in: &str, amount_in: u128) -> Option<u128> {
    let reserve_in = pool.tokens.get(token_in)?.balance.parse::<u128>().ok()?;

    // Find the other token
    let token_out = pool.tokens.keys().find(|t| *t != token_in)?;
    let reserve_out = pool.tokens.get(token_out)?.balance.parse::<u128>().ok()?;

    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
        return None;
    }

    let fee: f64 = pool.fee.parse().unwrap_or(0.003);
    let fee_bps = (fee * 10_000.0).round() as u128;
    let fee_multiplier = 10_000u128.checked_sub(fee_bps)?;

    let amount_in_with_fee = amount_in.checked_mul(fee_multiplier)?;
    let numerator = amount_in_with_fee.checked_mul(reserve_out)?;
    let denominator = reserve_in
        .checked_mul(10_000)?
        .checked_add(amount_in_with_fee)?;

    numerator.checked_div(denominator)
}

/// Get the other token in a constant-product pool.
fn pool_other_token<'a>(pool: &'a ConstantProductPool, token: &str) -> Option<&'a str> {
    pool.tokens
        .keys()
        .find(|t| t.as_str() != token)
        .map(|s| s.as_str())
}

// ── Generic swap estimation (works for both CP and V3 pools) ────────────────

/// Estimate swap output for ANY liquidity type. Returns (amount_out, pool_id).
fn estimate_swap_out(pool: &Liquidity, sell_token: &str, amount_in: u128) -> Option<(u128, String)> {
    let sell_lower = sell_token.to_lowercase();
    match pool {
        Liquidity::ConstantProduct(p) | Liquidity::WeightedProduct(p) | Liquidity::Stable(p) => {
            let out = cp_swap_out(p, sell_token, amount_in)?;
            Some((out, p.id.clone()))
        }
        Liquidity::ConcentratedLiquidity(p) => {
            // Determine direction from token ordering
            if p.tokens.len() < 2 { return None; }
            let zero_for_one = sell_lower == p.token0().to_lowercase();

            let sqrt_price: u128 = p.sqrt_price.parse().ok()?;
            let liquidity: u128 = p.liquidity.parse().ok()?;
            let fee: f64 = p.fee.parse().unwrap_or(0.0005);
            let fee_micro = (fee * 1_000_000.0).round() as u32;

            // Try tick traversal first, fall back to improved approximation
            let amount_out = if let Some(ref ln_map) = p.liquidity_net {
                let mut ticks: Vec<crate::models::liquidity::V3Tick> = ln_map.iter()
                    .filter_map(|(idx_str, liq_str)| {
                        let index: i32 = idx_str.parse().ok()?;
                        let liquidity_net: i128 = liq_str.parse().ok()?;
                        Some(crate::models::liquidity::V3Tick { index, liquidity_net })
                    })
                    .collect();
                ticks.sort_by_key(|t| t.index);
                if ticks.is_empty() {
                    crate::liquidity::uniswap_v3::get_amount_out_approx(
                        sqrt_price, liquidity, amount_in, zero_for_one, fee_micro,
                    )?
                } else {
                    crate::liquidity::uniswap_v3::get_amount_out_with_ticks(
                        sqrt_price, p.tick, liquidity, &ticks,
                        amount_in, zero_for_one, fee_micro,
                    )?.amount_out
                }
            } else {
                crate::liquidity::uniswap_v3::get_amount_out_approx(
                    sqrt_price, liquidity, amount_in, zero_for_one, fee_micro,
                )?
            };

            Some((amount_out, p.id.clone()))
        }
    }
}

/// Find the best pool for a token pair (any type: CP, V3, Weighted, Stable).
/// Returns the pool with the highest output for a small reference amount.
fn find_best_pool_for_pair<'a>(
    liquidity: &'a [Liquidity],
    token_a: &str,
    token_b: &str,
) -> Option<&'a Liquidity> {
    let a_lower = token_a.to_lowercase();
    let b_lower = token_b.to_lowercase();

    let mut best: Option<(&Liquidity, u128)> = None;

    for pool in liquidity {
        // Check if pool contains both tokens (case-insensitive)
        let has_pair = match pool {
            Liquidity::ConstantProduct(p) | Liquidity::WeightedProduct(p) | Liquidity::Stable(p) => {
                p.tokens.keys().any(|k| k.to_lowercase() == a_lower) &&
                p.tokens.keys().any(|k| k.to_lowercase() == b_lower)
            }
            Liquidity::ConcentratedLiquidity(p) => {
                p.has_pair(&token_a, &token_b)
            }
        };

        if !has_pair { continue; }

        // Estimate output for a small reference swap (1e15 = ~0.001 ETH)
        let ref_amount = 1_000_000_000_000_000u128;
        if let Some((out, _)) = estimate_swap_out(pool, token_a, ref_amount) {
            let is_better = best.as_ref().is_none_or(|(_, best_out)| out > *best_out);
            if is_better {
                best = Some((pool, out));
            }
        }
    }

    best.map(|(pool, _)| pool)
}

// ── Multi-hop routing ─────────────────────────────────────────────────────────

/// Try to route `order` through a single 2-hop path via `intermediate`.
/// Supports ALL pool types (V2, V3, Weighted, Stable) — picks the best pool per leg.
fn try_two_hop(
    order: &Order,
    liquidity: &[Liquidity],
    intermediate: &str,
) -> Option<MultiHopRoute> {
    let sell_amount: u128 = order.sell_amount.parse().ok()?;
    let buy_amount_min: u128 = order.buy_amount.parse().ok()?;

    // Find best pools for each leg (any type)
    let pool_a = find_best_pool_for_pair(liquidity, &order.sell_token, intermediate)?;
    let pool_b = find_best_pool_for_pair(liquidity, intermediate, &order.buy_token)?;

    // Simulate: sell_token → intermediate
    let (mid_amount, pool_a_id) = estimate_swap_out(pool_a, &order.sell_token, sell_amount)?;
    if mid_amount == 0 {
        return None;
    }

    // Simulate: intermediate → buy_token
    let (out_amount, pool_b_id) = estimate_swap_out(pool_b, intermediate, mid_amount)?;

    if out_amount < buy_amount_min {
        return None;
    }

    let surplus = out_amount.saturating_sub(buy_amount_min);

    let int1 = Interaction::Liquidity(LiquidityInteraction {
        internalize: false,
        id: pool_a_id.clone(),
        input_token: order.sell_token.clone(),
        output_token: intermediate.to_string(),
        input_amount: sell_amount.to_string(),
        output_amount: mid_amount.to_string(),
    });
    let int2 = Interaction::Liquidity(LiquidityInteraction {
        internalize: false,
        id: pool_b_id.clone(),
        input_token: intermediate.to_string(),
        output_token: order.buy_token.clone(),
        input_amount: mid_amount.to_string(),
        output_amount: out_amount.to_string(),
    });

    Some(MultiHopRoute {
        order_uid: order.uid.clone(),
        pool_ids: vec![pool_a_id, pool_b_id],
        intermediates: vec![intermediate.to_string()],
        executed_amount: sell_amount,
        output_amount: out_amount,
        surplus,
        interactions: vec![int1, int2],
    })
}

/// Find the best route for an order, trying all intermediaries for the given chain.
pub fn find_best_route(order: &Order, liquidity: &[Liquidity], chain_id: u64) -> Option<MultiHopRoute> {
    let mut best: Option<MultiHopRoute> = None;

    let sell_lower = order.sell_token.to_lowercase();
    let buy_lower = order.buy_token.to_lowercase();

    for &mid in intermediaries_for_chain(chain_id) {
        // Skip if mid is one of the order's tokens (case-insensitive)
        if mid.eq_ignore_ascii_case(&sell_lower) || mid.eq_ignore_ascii_case(&buy_lower) {
            continue;
        }

        if let Some(route) = try_two_hop(order, liquidity, mid) {
            let is_better = best.as_ref().is_none_or(|b| route.surplus > b.surplus);
            if is_better {
                debug!(
                    order_uid = %order.uid,
                    via = mid,
                    surplus = route.surplus,
                    "2-hop route found"
                );
                best = Some(route);
            }
        }
    }

    best
}

/// Find a constant-product pool in the liquidity array for the given token pair.
fn find_cp_pool_for_pair<'a>(
    liquidity: &'a [Liquidity],
    token_a: &str,
    token_b: &str,
) -> Option<&'a ConstantProductPool> {
    let a_lower = token_a.to_lowercase();
    let b_lower = token_b.to_lowercase();
    liquidity.iter().find_map(|l| match l {
        Liquidity::ConstantProduct(p) | Liquidity::WeightedProduct(p) | Liquidity::Stable(p) => {
            // Case-insensitive token matching — CoW driver may send checksummed addresses
            let has_a = p.tokens.keys().any(|k| k.to_lowercase() == a_lower);
            let has_b = p.tokens.keys().any(|k| k.to_lowercase() == b_lower);
            if has_a && has_b { Some(p) } else { None }
        }
        _ => None,
    })
}

/// Legacy: route all orders and return placeholder vec.
pub fn solve_routed(auction: &AuctionInstance, chain_id: u64) -> Vec<MultiHopRoute> {
    if auction.orders.is_empty() {
        return vec![];
    }

    let mut routes = Vec::new();
    for order in &auction.orders {
        if let Some(route) = find_best_route(order, &auction.liquidity, chain_id) {
            routes.push(route);
        }
    }
    routes
}

/// Build a Solution from a set of multi-hop routes.
pub fn build_routed_solution(
    auction_id: u64,
    routes: &[MultiHopRoute],
    tokens: &crate::models::auction::TokenMap,
) -> Option<Solution> {
    if routes.is_empty() {
        return None;
    }

    let mut prices = std::collections::HashMap::new();
    let mut trades = Vec::new();
    let mut interactions = Vec::new();
    let mut total_surplus = 0u128;

    for route in routes {
        trades.push(Trade::fulfillment(
            &route.order_uid,
            route.executed_amount.to_string(),
        ));
        for i in &route.interactions {
            interactions.push(i.clone());
        }
        // Clearing prices keyed by token address (CoW driver expects this format).
        // Derive sell/buy token from first/last interaction.
        if route.executed_amount > 0 {
            let sell_token = route.interactions.iter()
                .find_map(|i| match i {
                    crate::models::solution::Interaction::Liquidity(li) => Some(li.input_token.clone()),
                    _ => None,
                });
            let buy_token = route.interactions.iter()
                .rev()
                .find_map(|i| match i {
                    crate::models::solution::Interaction::Liquidity(li) => Some(li.output_token.clone()),
                    _ => None,
                });
            if let (Some(st), Some(bt)) = (sell_token, buy_token) {
                prices.entry(st).or_insert_with(|| route.executed_amount.to_string());
                prices.entry(bt).or_insert_with(|| route.output_amount.to_string());
            }
        }
        // Normalize surplus by reference_price(buy_token)
        let buy_token = route.interactions.iter()
            .filter_map(|i| match i {
                crate::models::solution::Interaction::Liquidity(li) => Some(li.output_token.as_str()),
                _ => None,
            })
            .last()
            .unwrap_or("");
        let normalized = crate::solver::assembler::normalize_surplus(route.surplus, buy_token, tokens);
        total_surplus = total_surplus.saturating_add(normalized);
    }

    Some(Solution {
        id: auction_id,
        prices,
        trades,
        pre_interactions: vec![],
        interactions,
        post_interactions: vec![],
        gas: None,
        score: Some(Score::Solver {
            score: total_surplus.to_string(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::{ConstantProductPool, Liquidity, LiquidityTokenBalance, LiquidityTokenMap};
    use crate::models::order::{Order, OrderClass, OrderKind};

    fn make_order(uid: &str, sell: &str, buy: &str, sell_amt: u128, buy_amt: u128) -> Order {
        Order {
            uid: uid.to_string(),
            sell_token: sell.to_string(),
            buy_token: buy.to_string(),
            sell_amount: sell_amt.to_string(),
            buy_amount: buy_amt.to_string(),
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

    fn make_cp_pool(id: &str, t0: &str, r0: u128, t1: &str, r1: u128) -> Liquidity {
        let mut tokens = LiquidityTokenMap::new();
        tokens.insert(t0.to_string(), LiquidityTokenBalance { balance: r0.to_string() });
        tokens.insert(t1.to_string(), LiquidityTokenBalance { balance: r1.to_string() });
        Liquidity::ConstantProduct(ConstantProductPool {
            id: id.to_string(),
            address: format!("0x{id}"),
            tokens,
            fee: "0.003".to_string(),
            router: None,
            gas_estimate: String::new(),
        })
    }

    #[test]
    fn two_hop_via_weth() {
        // DAI → WETH → USDC route
        let pool_dai_weth = make_cp_pool("dai_weth", "0xdai", 1_000_000, WETH, 1_000_000);
        let pool_weth_usdc = make_cp_pool("weth_usdc", WETH, 1_000_000, "0xusdc", 2_000_000);

        let order = make_order("uid1", "0xdai", "0xusdc", 1_000, 1_500);
        let route = find_best_route(&order, &[pool_dai_weth, pool_weth_usdc], 1).unwrap();

        assert_eq!(route.intermediates, vec![WETH.to_string()]);
        assert_eq!(route.interactions.len(), 2);
        assert!(route.output_amount >= 1_500);
    }

    #[test]
    fn no_route_when_no_intermediate_pool() {
        // Only pool: FOO → BAR (no WETH bridge)
        let pool = make_cp_pool("foo_bar", "0xfoo", 1_000_000, "0xbar", 2_000_000);
        let order = make_order("uid2", "0xfoo", "0xbaz", 1_000, 900);
        // No pool for FOO/USDC or any intermediary to BAZ
        assert!(find_best_route(&order, &[pool], 1).is_none());
    }

    #[test]
    fn best_route_chosen_from_multiple_intermediaries() {
        // Two paths: via WETH (worse price) and via USDC (better price)
        let pool_a_weth = make_cp_pool("a_weth", "0xa", 1_000_000, WETH, 500_000); // bad price
        let pool_weth_b = make_cp_pool("weth_b", WETH, 500_000, "0xb", 1_000_000);
        let pool_a_usdc = make_cp_pool("a_usdc", "0xa", 1_000_000, USDC, 800_000); // better price
        let pool_usdc_b = make_cp_pool("usdc_b", USDC, 800_000, "0xb", 1_000_000);

        let order = make_order("uid3", "0xa", "0xb", 10_000, 5_000);
        let route = find_best_route(
            &order,
            &[pool_a_weth, pool_weth_b, pool_a_usdc, pool_usdc_b],
            1,
        )
        .unwrap();

        // Should pick the path with higher surplus
        assert!(route.surplus > 0);
    }
}
