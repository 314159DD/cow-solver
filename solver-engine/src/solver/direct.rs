use std::collections::HashMap;
use std::sync::OnceLock;

use tracing::{debug, warn};

use crate::liquidity::curve;
use crate::liquidity::balancer_v2;
use crate::models::auction::AuctionInstance;
use crate::models::liquidity::{ConstantProductPool, ConcentratedLiquidityPool, Liquidity};
use crate::models::order::{Order, OrderKind};
use crate::models::solution::{
    Interaction, LiquidityInteraction, Score, Solution, Trade,
};

// ── Pool math registries (populated at startup from chain config) ────────────

static CURVE_POOLS: OnceLock<HashMap<String, curve::CurvePool>> = OnceLock::new();
static BALANCER_POOLS: OnceLock<HashMap<String, balancer_v2::BalancerWeightedPool>> = OnceLock::new();

/// Register Curve pools for proper StableSwap math (called at startup).
pub fn register_curve_pools(pools: HashMap<String, curve::CurvePool>) {
    let _ = CURVE_POOLS.set(pools);
}

/// Register Balancer weighted pools for proper weighted product math (called at startup).
pub fn register_balancer_pools(pools: HashMap<String, balancer_v2::BalancerWeightedPool>) {
    let _ = BALANCER_POOLS.set(pools);
}

fn get_curve_pool(address: &str) -> Option<&'static curve::CurvePool> {
    CURVE_POOLS.get()?.get(&address.to_lowercase())
}

fn get_balancer_pool(address: &str) -> Option<&'static balancer_v2::BalancerWeightedPool> {
    BALANCER_POOLS.get()?.get(&address.to_lowercase())
}

// ── Result type ───────────────────────────────────────────────────────────────

/// The result of routing a single order through a pool.
#[derive(Debug, Clone)]
pub struct OrderRoute {
    pub order_uid: String,
    /// Pool liquidity id from the auction
    pub pool_id: String,
    /// Amount executed (sell_amount for sell orders)
    pub executed_amount: u128,
    /// Actual output received
    pub output_amount: u128,
    /// Surplus over the order's limit price (in buy token units)
    pub surplus: u128,
    /// The interaction to encode for this trade
    pub interaction: Interaction,
    /// Clearing price: sell_token → buy_token as a rational (numerator/denominator)
    pub price_numerator: u128,
    pub price_denominator: u128,
}

// ── DirectSolver ──────────────────────────────────────────────────────────────

pub struct DirectSolver;

impl DirectSolver {
    /// Attempt to route a single order through the best available pool.
    ///
    /// Returns `None` if no pool can fill the order at its limit price.
    pub fn solve_order(
        order: &Order,
        liquidity: &[Liquidity],
    ) -> Option<OrderRoute> {
        let sell_amount = order.sell_amount.parse::<u128>().ok()?;
        let buy_amount = order.buy_amount.parse::<u128>().ok()?;

        if sell_amount == 0 || buy_amount == 0 {
            return None;
        }

        let mut best: Option<OrderRoute> = None;

        for pool in liquidity {
            let route = match pool {
                Liquidity::ConstantProduct(p) => {
                    Self::try_constant_product(order, p, sell_amount, buy_amount)
                }
                Liquidity::ConcentratedLiquidity(p) => {
                    Self::try_concentrated(order, p, sell_amount, buy_amount)
                }
                Liquidity::Stable(p) => {
                    // Try proper Curve StableSwap math if pool registered
                    if let Some(curve_pool) = get_curve_pool(&p.address) {
                        Self::try_curve_stable(order, p, curve_pool, sell_amount, buy_amount)
                    } else {
                        // Fallback to constant-product approximation
                        Self::try_constant_product(order, p, sell_amount, buy_amount)
                    }
                }
                Liquidity::WeightedProduct(p) => {
                    // Try proper Balancer weighted product math if pool registered
                    if let Some(bal_pool) = get_balancer_pool(&p.address) {
                        Self::try_balancer_weighted(order, p, bal_pool, sell_amount, buy_amount)
                    } else {
                        // Fallback to constant-product approximation
                        Self::try_constant_product(order, p, sell_amount, buy_amount)
                    }
                }
            };

            if let Some(route) = route {
                let better = best
                    .as_ref()
                    .is_none_or(|b| route.output_amount > b.output_amount);
                if better {
                    best = Some(route);
                }
            }
        }

        best
    }

    /// Route all orders in the auction; collect routes and assemble a solution.
    pub fn solve_auction(auction: &AuctionInstance) -> Option<Solution> {
        if auction.orders.is_empty() || auction.liquidity.is_empty() {
            return None;
        }

        let mut routes = Vec::new();
        for order in &auction.orders {
            if let Some(route) = Self::solve_order(order, &auction.liquidity) {
                debug!(
                    order_uid = %route.order_uid,
                    pool_id = %route.pool_id,
                    surplus = route.surplus,
                    "Direct route found"
                );
                routes.push(route);
            }
        }

        if routes.is_empty() {
            return None;
        }

        // Build UDCP prices: token → price in reference currency (1e18 scale)
        // For now we use the ratio from each route; UDCP enforcement added in S2-2
        let mut prices: HashMap<String, String> = HashMap::new();
        let mut trades = Vec::new();
        let mut interactions = Vec::new();
        let mut total_surplus = 0u128;

        for route in &routes {
            // Get sell/buy tokens from order
            if let Some(order) = auction.orders.iter().find(|o| o.uid == route.order_uid) {
                // Price vector: set sell_token price = 1 (unit), buy_token = sell_amount/buy_amount
                // This is a simplified representation; UDCP enforcer will normalize in S2-2
                let sell_price = "1000000000000000000"; // 1e18
                let buy_price = if route.price_denominator > 0 {
                    let ratio = route.price_numerator as f64 / route.price_denominator as f64;
                    let scaled = (ratio * 1e18) as u128;
                    scaled.to_string()
                } else {
                    sell_price.to_string()
                };

                prices
                    .entry(order.sell_token.clone())
                    .or_insert_with(|| sell_price.to_string());
                prices
                    .entry(order.buy_token.clone())
                    .or_insert_with(|| buy_price);
            }

            // Normalize surplus by reference_price for fair scoring
            let buy_token = auction.orders.iter()
                .find(|o| o.uid == route.order_uid)
                .map(|o| o.buy_token.as_str())
                .unwrap_or("");
            let normalized = crate::solver::assembler::normalize_surplus(
                route.surplus, buy_token, &auction.tokens,
            );

            trades.push(Trade::fulfillment(
                &route.order_uid,
                route.executed_amount.to_string(),
            ));
            interactions.push(route.interaction.clone());
            total_surplus = total_surplus.saturating_add(normalized);
        }

        // Score: sum of normalized surplus (ETH-denominated)
        let score = Some(Score::Solver {
            score: total_surplus.to_string(),
        });

        Some(Solution {
            id: auction.id,
            prices,
            trades,
            pre_interactions: vec![],
            interactions,
            post_interactions: vec![],
            gas: None,
            score,
        })
    }

    // ── Pool type dispatch ─────────────────────────────────────────────────────

    fn try_constant_product(
        order: &Order,
        pool: &ConstantProductPool,
        sell_amount: u128,
        buy_amount_limit: u128,
    ) -> Option<OrderRoute> {
        // Find which token is sell and which is buy
        let pool_tokens: Vec<&String> = pool.tokens.keys().collect();
        if pool_tokens.len() < 2 {
            return None;
        }

        let has_sell = pool.tokens.contains_key(&order.sell_token);
        let has_buy = pool.tokens.contains_key(&order.buy_token);
        if !has_sell || !has_buy {
            return None;
        }

        // Get reserves for sell and buy tokens
        let reserve_in = pool
            .tokens
            .get(&order.sell_token)?
            .balance
            .parse::<u128>()
            .ok()?;
        let reserve_out = pool
            .tokens
            .get(&order.buy_token)?
            .balance
            .parse::<u128>()
            .ok()?;

        if reserve_in == 0 || reserve_out == 0 {
            return None;
        }

        // Parse fee (e.g. "0.003" → 30 bps → fee_multiplier = 9970 / 10000)
        let fee: f64 = pool.fee.parse().unwrap_or(0.003);
        let fee_bps = (fee * 10_000.0).round() as u128;
        let fee_multiplier = 10_000u128.checked_sub(fee_bps)?;

        // Constant product: amount_out = (amount_in * fee_mult * reserve_out)
        //                                / (reserve_in * 10000 + amount_in * fee_mult)
        let amount_in = match order.kind {
            OrderKind::Sell => sell_amount,
            OrderKind::Buy => {
                // For buy orders: compute required sell amount to receive buy_amount_limit
                // amount_in = (reserve_in * buy_amount * 10000)
                //             / ((reserve_out - buy_amount) * fee_mult)
                let buy_exact = buy_amount_limit;
                if buy_exact >= reserve_out {
                    return None;
                }
                let numerator = reserve_in
                    .checked_mul(buy_exact)?
                    .checked_mul(10_000)?;
                let denominator = reserve_out
                    .checked_sub(buy_exact)?
                    .checked_mul(fee_multiplier)?;
                numerator.checked_div(denominator)?.checked_add(1)?
            }
        };

        let amount_in_with_fee = amount_in.checked_mul(fee_multiplier)?;
        let numerator = amount_in_with_fee.checked_mul(reserve_out)?;
        let denominator = reserve_in
            .checked_mul(10_000)?
            .checked_add(amount_in_with_fee)?;
        let mut amount_out = numerator.checked_div(denominator)?;

        if amount_out < buy_amount_limit {
            return None; // Can't satisfy limit price
        }

        // Bug #3 fix: Cap buy order output at order.buy_amount.
        // The AMM amount_in uses ceiling division (+1), which can route to
        // buy slightly more than requested. The reference solver caps this.
        if order.kind == OrderKind::Buy {
            amount_out = amount_out.min(buy_amount_limit);
        }

        let surplus = amount_out.saturating_sub(buy_amount_limit);

        let interaction = Interaction::Liquidity(LiquidityInteraction {
            internalize: false,
            id: pool.id.clone(),
            input_token: order.sell_token.clone(),
            output_token: order.buy_token.clone(),
            input_amount: amount_in.to_string(),
            output_amount: amount_out.to_string(),
        });

        Some(OrderRoute {
            order_uid: order.uid.clone(),
            pool_id: pool.id.clone(),
            executed_amount: amount_in,
            output_amount: amount_out,
            surplus,
            interaction,
            price_numerator: amount_out,
            price_denominator: amount_in,
        })
    }

    /// Route through a Curve StableSwap pool using proper invariant math.
    fn try_curve_stable(
        order: &Order,
        cp_pool: &ConstantProductPool,
        curve_pool: &curve::CurvePool,
        sell_amount: u128,
        buy_amount_limit: u128,
    ) -> Option<OrderRoute> {
        // Find token indices in the Curve pool
        let sell_lower = order.sell_token.to_lowercase();
        let buy_lower = order.buy_token.to_lowercase();
        let i = curve_pool.tokens.iter().position(|t| t.to_lowercase() == sell_lower)?;
        let j = curve_pool.tokens.iter().position(|t| t.to_lowercase() == buy_lower)?;

        let amount_out = curve::get_dy(curve_pool, i, j, sell_amount)?;

        if amount_out < buy_amount_limit {
            return None; // Can't satisfy limit price
        }

        let surplus = amount_out.saturating_sub(buy_amount_limit);

        let interaction = Interaction::Liquidity(LiquidityInteraction {
            internalize: false,
            id: cp_pool.id.clone(),
            input_token: order.sell_token.clone(),
            output_token: order.buy_token.clone(),
            input_amount: sell_amount.to_string(),
            output_amount: amount_out.to_string(),
        });

        Some(OrderRoute {
            order_uid: order.uid.clone(),
            pool_id: cp_pool.id.clone(),
            executed_amount: sell_amount,
            output_amount: amount_out,
            surplus,
            interaction,
            price_numerator: amount_out,
            price_denominator: sell_amount,
        })
    }

    /// Route through a Balancer V2 Weighted pool using proper weighted product math.
    fn try_balancer_weighted(
        order: &Order,
        cp_pool: &ConstantProductPool,
        bal_pool: &balancer_v2::BalancerWeightedPool,
        sell_amount: u128,
        buy_amount_limit: u128,
    ) -> Option<OrderRoute> {
        let amount_out = balancer_v2::weighted_swap_out(
            bal_pool,
            &order.sell_token,
            &order.buy_token,
            sell_amount,
        )?;

        if amount_out < buy_amount_limit {
            return None;
        }

        let surplus = amount_out.saturating_sub(buy_amount_limit);

        let interaction = Interaction::Liquidity(LiquidityInteraction {
            internalize: false,
            id: cp_pool.id.clone(),
            input_token: order.sell_token.clone(),
            output_token: order.buy_token.clone(),
            input_amount: sell_amount.to_string(),
            output_amount: amount_out.to_string(),
        });

        Some(OrderRoute {
            order_uid: order.uid.clone(),
            pool_id: cp_pool.id.clone(),
            executed_amount: sell_amount,
            output_amount: amount_out,
            surplus,
            interaction,
            price_numerator: amount_out,
            price_denominator: sell_amount,
        })
    }

    fn try_concentrated(
        order: &Order,
        pool: &ConcentratedLiquidityPool,
        sell_amount: u128,
        buy_amount_limit: u128,
    ) -> Option<OrderRoute> {
        if !pool.has_pair(&order.sell_token, &order.buy_token) {
            return None;
        }

        let sqrt_price: u128 = pool.sqrt_price.parse().ok()?;
        let liquidity: u128 = pool.liquidity.parse().ok()?;
        let fee: f64 = pool.fee.parse().unwrap_or(0.0005);
        let fee_micro = (fee * 1_000_000.0).round() as u32;

        // Determine direction: token0 is the lexicographically smaller address
        let zero_for_one = order.sell_token.to_lowercase() == pool.token0().to_lowercase();

        let amount_in = match order.kind {
            OrderKind::Sell => sell_amount,
            OrderKind::Buy => {
                // For buy orders: we need to find the input that produces buy_amount_limit.
                // Use sell_amount as max input; we'll verify the output meets the limit.
                sell_amount
            }
        };

        // Try full tick-traversal math if tick data is available from CoW driver.
        // Falls back to single-tick approximation if no tick data.
        let amount_out = if let Some(ref ln_map) = pool.liquidity_net {
            // Convert HashMap<String, String> → sorted Vec<V3Tick>
            let mut ticks: Vec<crate::models::liquidity::V3Tick> = ln_map
                .iter()
                .filter_map(|(idx_str, liq_str)| {
                    let index: i32 = idx_str.parse().ok()?;
                    let liquidity_net: i128 = liq_str.parse().ok()?;
                    Some(crate::models::liquidity::V3Tick { index, liquidity_net })
                })
                .collect();
            ticks.sort_by_key(|t| t.index);

            if ticks.is_empty() {
                // No valid ticks — fall back to approximation
                crate::liquidity::uniswap_v3::get_amount_out_approx(
                    sqrt_price, liquidity, amount_in, zero_for_one, fee_micro,
                )?
            } else {
                // Full tick-traversal math — accurate price impact modeling
                let result = crate::liquidity::uniswap_v3::get_amount_out_with_ticks(
                    sqrt_price, pool.tick, liquidity, &ticks,
                    amount_in, zero_for_one, fee_micro,
                )?;
                result.amount_out
            }
        } else {
            // No tick data — use single-tick spot price approximation.
            // This underestimates output for large swaps that cross multiple ticks.
            if sell_amount > 1_000_000_000_000_000_000 {
                warn!(
                    pool_id = %pool.id,
                    sell_amount,
                    "V3 pool has no tick data — using single-tick approximation for large order (>1 ETH)"
                );
            }
            crate::liquidity::uniswap_v3::get_amount_out_approx(
                sqrt_price, liquidity, amount_in, zero_for_one, fee_micro,
            )?
        };

        if amount_out < buy_amount_limit {
            return None;
        }

        let surplus = amount_out.saturating_sub(buy_amount_limit);

        let interaction = Interaction::Liquidity(LiquidityInteraction {
            internalize: false,
            id: pool.id.clone(),
            input_token: order.sell_token.clone(),
            output_token: order.buy_token.clone(),
            input_amount: amount_in.to_string(),
            output_amount: amount_out.to_string(),
        });

        Some(OrderRoute {
            order_uid: order.uid.clone(),
            pool_id: pool.id.clone(),
            executed_amount: amount_in,
            output_amount: amount_out,
            surplus,
            interaction,
            price_numerator: amount_out,
            price_denominator: amount_in,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::liquidity::{ConstantProductPool, LiquidityTokenBalance, LiquidityTokenMap};
    use crate::models::order::{Order, OrderKind};

    fn make_order(
        uid: &str,
        sell_token: &str,
        buy_token: &str,
        sell_amount: u128,
        buy_amount: u128,
        kind: OrderKind,
    ) -> Order {
        Order {
            uid: uid.to_string(),
            sell_token: sell_token.to_string(),
            buy_token: buy_token.to_string(),
            sell_amount: sell_amount.to_string(),
            buy_amount: buy_amount.to_string(),
            fee_amount: "0".to_string(),
            kind,
            partially_fillable: false,
            class: Default::default(),
            sell_token_balance: None,
            buy_token_balance: None,
            signing_scheme: None,
            signature: None,
            receiver: None,
            app_data: None,
            valid_to: None,
        }
    }

    fn make_cp_pool(id: &str, t0: &str, r0: u128, t1: &str, r1: u128, fee: &str) -> Liquidity {
        let mut tokens = LiquidityTokenMap::new();
        tokens.insert(
            t0.to_string(),
            LiquidityTokenBalance { balance: r0.to_string() },
        );
        tokens.insert(
            t1.to_string(),
            LiquidityTokenBalance { balance: r1.to_string() },
        );
        Liquidity::ConstantProduct(ConstantProductPool {
            id: id.to_string(),
            address: format!("0x{id}"),
            tokens,
            fee: fee.to_string(),
            router: None,
            gas_estimate: String::new(),
        })
    }

    #[test]
    fn sell_order_above_limit_routed() {
        // Pool: 1e12 WETH / 2e15 USDC (1 WETH ≈ 2000 USDC)
        let pool = make_cp_pool(
            "pool1",
            "0xweth",
            1_000_000_000_000u128,       // 1e12
            "0xusdc",
            2_000_000_000_000_000u128,   // 2e15
            "0.003",
        );

        // Sell 1e9 WETH, minimum 1.9e12 USDC (slightly below market)
        let order = make_order("0xuid1", "0xweth", "0xusdc", 1_000_000_000, 1_900_000_000_000, OrderKind::Sell);

        let route = DirectSolver::solve_order(&order, &[pool]).unwrap();
        assert_eq!(route.order_uid, "0xuid1");
        assert!(route.output_amount >= 1_900_000_000_000);
        assert!(route.surplus > 0);
    }

    #[test]
    fn order_below_limit_returns_none() {
        // Pool: tiny reserves — can't satisfy a large buy_amount
        let pool = make_cp_pool(
            "pool2",
            "0xweth",
            1_000u128,
            "0xusdc",
            2_000u128,
            "0.003",
        );
        // Demand 10x what pool can offer
        let order = make_order("0xuid2", "0xweth", "0xusdc", 500, 100_000_000, OrderKind::Sell);
        assert!(DirectSolver::solve_order(&order, &[pool]).is_none());
    }

    #[test]
    fn wrong_pair_returns_none() {
        let pool = make_cp_pool("pool3", "0xweth", 1_000_000, "0xusdc", 2_000_000, "0.003");
        // Order for DAI/USDT — not in pool
        let order = make_order("0xuid3", "0xdai", "0xusdt", 1_000, 900, OrderKind::Sell);
        assert!(DirectSolver::solve_order(&order, &[pool]).is_none());
    }

    #[test]
    fn best_pool_selected() {
        // Two pools for same pair, second gives better output
        let pool1 = make_cp_pool(
            "small",
            "0xweth",
            10_000u128,
            "0xusdc",
            20_000u128,
            "0.003",
        );
        let pool2 = make_cp_pool(
            "large",
            "0xweth",
            1_000_000u128,
            "0xusdc",
            2_000_000u128,
            "0.003",
        );

        let order = make_order("0xuid4", "0xweth", "0xusdc", 100, 150, OrderKind::Sell);
        let route = DirectSolver::solve_order(&order, &[pool1, pool2]).unwrap();
        // Larger pool gives better price (less price impact)
        assert_eq!(route.pool_id, "large");
    }
}
