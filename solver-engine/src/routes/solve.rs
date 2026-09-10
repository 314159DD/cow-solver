use std::time::Instant;

use axum::{Json, body::Bytes, http::StatusCode, response::IntoResponse};
use tracing::{debug, info, warn};

use crate::accounting;
use crate::attribution::{self, AttributionStage};
use crate::competition;
use crate::freshness::{self, FreshnessInput};
use crate::models::auction::AuctionInstance;
use crate::models::solution::{Score, SolveResponse};
use crate::monitoring;
use crate::pool_indexer;
use crate::replay;
use crate::simulation;
use crate::solver;
use crate::submission::{self, PolicyInput};
use crate::triage;

/// 1 ETH in wei — orders above this are considered "large"
const LARGE_ORDER_THRESHOLD_WEI: u128 = 1_000_000_000_000_000_000;

/// POST /solve — receives a CoW auction, returns candidate solutions.
///
/// Uses iterative deepening: 5 phases of increasing depth, each improving
/// on the previous best score. Returns the best solution found across all phases.
pub async fn solve(body: Bytes) -> impl IntoResponse {
    let auction: AuctionInstance = match serde_json::from_slice(&body) {
        Ok(a) => a,
        Err(e) => {
            warn!(
                error = %e,
                result = "error",
                "Failed to deserialize auction payload"
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Invalid auction payload: {e}") })),
            )
                .into_response();
        }
    };

    let auction_id = auction.id;
    let configured_chain_id: u64 = std::env::var("CHAIN_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let chain_id = auction.chain_id.unwrap_or(configured_chain_id);
    let orders_count = auction.orders.len();

    // Log WETH reference_price for scoring diagnostics
    let weth_addr = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
    let usdc_addr = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
    let weth_ref = auction.tokens.get(weth_addr)
        .or_else(|| auction.tokens.iter().find(|(k,_)| k.to_lowercase() == weth_addr).map(|(_,v)| v))
        .and_then(|t| t.reference_price.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("none");
    let usdc_ref = auction.tokens.get(usdc_addr)
        .or_else(|| auction.tokens.iter().find(|(k,_)| k.to_lowercase() == usdc_addr).map(|(_,v)| v))
        .and_then(|t| t.reference_price.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("none");

    info!(
        auction_id,
        orders = orders_count,
        driver_pools = auction.liquidity.len(),
        tokens = auction.tokens.len(),
        weth_reference_price = weth_ref,
        usdc_reference_price = usdc_ref,
        "Auction received"
    );

    // B.1: Advance block tracker from auction metadata (fast path — no RPC needed)
    let auction_block = auction.block.unwrap_or(0);
    if auction_block > 0 {
        pool_indexer::set_block(auction_block);
        pool_indexer::seed_from_auction(&auction.liquidity);
        // Mark auction pools as "hot" for priority refresh (cost optimization)
        for pool in &auction.liquidity {
            pool_indexer::mark_hot(pool.address());
        }
    }

    // Extract ETH/USD price from auction token reference prices.
    // USDC (6 decimals) reference_price = price of 1 USDC in ETH (as a float string).
    // ETH price in USD = 1 / usdc_ref_price * 10^(18-6) ... but CoW normalizes to 18 decimals.
    // Simpler: look for WETH's reference_price (should be ~1.0 in ETH terms) and USDC's.
    // USDC ref_price ≈ 0.00037 ETH → 1 ETH ≈ 2700 USD.
    {
        // Well-known USDC addresses on Arbitrum
        let usdc_addrs = [
            "0xaf88d065e77c8cc2239327c5edb3a432268e5831", // native USDC
            "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8", // USDC.e
        ];
        for addr in &usdc_addrs {
            if let Some(token) = auction.tokens.get(*addr) {
                if let Some(ref_price_str) = &token.reference_price {
                    if let Ok(ref_price) = ref_price_str.parse::<f64>() {
                        if ref_price > 0.0 {
                            // ref_price is in ETH per 1 USDC (adjusted for decimals).
                            // CoW reference prices are in "atoms of reference token per atom of this token"
                            // For USDC (6 dec) → ETH (18 dec): ref_price already accounts for decimal diff.
                            // ETH price in USD ≈ 1e18 / ref_price (since ref_price = atoms_ETH per atom_USDC)
                            // But actually: ref_price for USDC means "1 USDC atom = ref_price ETH atoms"
                            // So 1 USDC ($1) = ref_price / 1e12 ETH (adjusting 6→18 dec)
                            // ETH price = 1 / (ref_price / 1e12) = 1e12 / ref_price
                            let eth_usd = 1e12 / ref_price;
                            if eth_usd > 100.0 && eth_usd < 100_000.0 {
                                // Sanity: ETH should be $100-$100k
                                monitoring::set_eth_price_usd_cents((eth_usd * 100.0) as u64);
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    // Determine if any order is "large" (> 1 ETH equivalent in sell amount)
    let has_large_orders = auction.orders.iter().any(|o| {
        o.sell_amount.parse::<u128>().unwrap_or(0) >= LARGE_ORDER_THRESHOLD_WEI
    });

    // B.5: Fast triage — classify before committing full compute
    let (triage_class, triage_reason) = triage::classify(&auction);
    if triage_class == triage::TriageClass::Skip {
        info!(
            auction_id,
            chain_id,
            orders_count,
            triage_class = %triage_class,
            triage_reason,
            result = "empty",
            "Auction skipped by triage"
        );
        return (StatusCode::OK, Json(SolveResponse { solutions: vec![] })).into_response();
    }

    // If driver sent 0 parseable pools, fall back to cached pools.
    // Mark relevant pools as "hot" so background indexer refreshes them
    // every 30 seconds (zero additional RPC cost — uses existing cycle).
    let mut auction = auction;
    if auction.liquidity.is_empty() {
        // Mark pools touching order tokens as hot for priority refresh
        let order_tokens: std::collections::HashSet<String> = auction.orders.iter()
            .flat_map(|o| [o.sell_token.to_lowercase(), o.buy_token.to_lowercase()])
            .collect();
        let mut hot_count = 0usize;
        for entry in pool_indexer::pool_cache().iter() {
            let snap = entry.value();
            if order_tokens.contains(&snap.token0.to_lowercase())
                || order_tokens.contains(&snap.token1.to_lowercase())
            {
                pool_indexer::mark_hot(&snap.address);
                hot_count += 1;
            }
        }

        let cached = pool_indexer::cached_as_liquidity();
        if !cached.is_empty() {
            debug!(
                auction_id,
                cached_pools = cached.len(),
                hot_marked = hot_count,
                "Using cached pools (hot-marked for background refresh)"
            );
            auction.liquidity = cached;
        }
    }

    // Keep orders for settlement encoding
    let auction_orders = auction.orders.clone();

    let start = Instant::now();

    // ── SINGLE-PASS SOLVING (driver pools only) ────────────────────────
    // The CoW driver sends current-block reserves in auction.liquidity.
    // These are the ONLY pools we trust for routing. Our cached pools have
    // stale reserves that produce inflated surplus → inflated scores.
    //
    // Previous two-pass approach replaced driver pools with cached pools
    // in Pass 2, which was the root cause of 1000x score inflation.
    info!(
        auction_id,
        driver_pools = auction.liquidity.len(),
        cached_pools = pool_indexer::pool_count(),
        "Solving with driver-provided pools only (no stale cache mixing)"
    );
    // Hard timeout: if solve takes longer than 20s, return whatever we have.
    // The CoW driver expects a response within ~25s. We leave 5s buffer.
    let max_solve_ms: u64 = std::env::var("MAX_SOLVE_TIME_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);

    let outcome = match tokio::time::timeout(
        std::time::Duration::from_millis(max_solve_ms),
        solver::solve(auction),
    ).await {
        Ok(result) => result,
        Err(_) => {
            warn!(auction_id, "Solve timed out after {}ms — returning empty", max_solve_ms);
            solver::SolveOutcome {
                response: SolveResponse { solutions: vec![] },
                strategies_ran: vec!["timeout"],
                winning_strategy: "",
                used_fallback: false,
                phase_reached: 0,
                phase_times_ms: [0; 5],
                phase_scores: [0; 5],
                phase_decisions: vec![],
            }
        }
    };

    let response_time_ms = start.elapsed().as_millis() as u64;
    let solutions_found = outcome.response.solutions.len();

    let best_score_wei: u128 = outcome.response.solutions.iter()
        .filter_map(|s| s.score.as_ref())
        .find_map(|score| match score {
            Score::Solver { score } => score.parse::<u128>().ok(),
            _ => None,
        })
        .unwrap_or(0);

    // ── Attribution: record all strategies that ran (C.3) ─────────────────
    let best_strategy = outcome.winning_strategy;

    // Score sanity check: typical winners score 1e14–1e16 wei.
    // Anything above 1e16 is almost certainly stale pool data or scoring bug.
    const SCORE_SANITY_BOUND: u128 = 10_000_000_000_000_000; // 1e16
    if best_score_wei > SCORE_SANITY_BOUND {
        warn!(
            auction_id = auction_id,
            best_score_wei,
            sanity_bound = SCORE_SANITY_BOUND,
            strategy = best_strategy,
            "Score exceeds sanity bound — likely stale pool reserves"
        );
    }
    for strategy in &outcome.strategies_ran {
        attribution::record(strategy, AttributionStage::Generated);
    }

    // ── Simulation: validate the best solution (A.5) ────────────────────
    let sim_result = if solutions_found > 0 {
        let best_solution = &outcome.response.solutions[0];
        let sim = simulation::simulate_solution(best_solution, chain_id, &auction_orders).await;
        if sim.simulated && sim.success {
            // Simulation is structural (not EVM fork), so any strategy that produced
            // candidates with trades+prices would also pass. Record SimPassed for all
            // strategies that generated candidates, not just the winner.
            for pd in &outcome.phase_decisions {
                if pd.candidates_produced > 0 {
                    attribution::record(pd.strategy, AttributionStage::SimPassed);
                }
            }
        } else if sim.simulated {
            attribution::record(best_strategy, AttributionStage::SimFailed);
        }
        sim
    } else {
        simulation::SimResult::skipped()
    };

    // ── Freshness scoring (B.4) ─────────────────────────────────────────
    let current_block = if auction_block > 0 {
        auction_block
    } else {
        pool_indexer::current_block()
    };
    let freshness_score = freshness::score(&FreshnessInput {
        current_block,
        cache_block: pool_indexer::current_block().saturating_sub(1),
        rpc_latency_ms: 0,
        rfq_quote_age_ms: u64::MAX,
        sim_block: sim_result.block_number,
    });

    // ── Submission policy (C.2) ─────────────────────────────────────────
    let submission_decision = if solutions_found > 0 {
        let elapsed_secs = start.elapsed().as_secs();
        let max_solve_secs: u64 = 25;
        let time_remaining = max_solve_secs.saturating_sub(elapsed_secs);
        let decision = submission::evaluate(&PolicyInput {
            score_wei: best_score_wei as i128,
            freshness: freshness_score.clone(),
            simulated: sim_result.simulated,
            sim_reverted: !sim_result.success && sim_result.simulated,
            time_remaining_secs: time_remaining,
            previous_score_wei: 0,
            replacement_gas_wei: 0,
            has_large_orders,
        });
        attribution::record(best_strategy, AttributionStage::PolicyApproved);
        Some(decision)
    } else {
        None
    };

    let should_submit = submission_decision
        .as_ref()
        .map(|d| d.timing != submission::TimingDecision::Hold)
        .unwrap_or(false);

    let result = if solutions_found > 0 && should_submit {
        attribution::record(best_strategy, AttributionStage::Submitted);
        "submitted"
    } else if solutions_found > 0 {
        "held"
    } else {
        "empty"
    };

    // ── Structured log with phase metadata ───────────────────────────────
    info!(
        auction_id,
        chain_id,
        orders_count,
        triage_class = %triage_class,
        strategies_attempted = ?outcome.strategies_ran,
        solutions_found,
        best_score_wei,
        response_time_ms,
        result,
        used_fallback = outcome.used_fallback,
        sim_success = sim_result.success,
        sim_simulated = sim_result.simulated,
        freshness_confidence = freshness_score.confidence,
        freshness_bottleneck = freshness_score.bottleneck.as_str(),
        phase_reached = outcome.phase_reached,
        phase1_ms = outcome.phase_times_ms[0],
        phase2_ms = outcome.phase_times_ms[1],
        phase3_ms = outcome.phase_times_ms[2],
        phase4_ms = outcome.phase_times_ms[3],
        phase5_ms = outcome.phase_times_ms[4],
        phase1_score = outcome.phase_scores[0],
        phase5_score = outcome.phase_scores[4],
        "Auction solved"
    );

    // Log per-phase decisions for debugging
    for pd in &outcome.phase_decisions {
        debug!(
            auction_id,
            phase = pd.phase,
            strategy = pd.strategy,
            candidates = pd.candidates_produced,
            score = pd.best_score,
            improved = pd.improved,
            reason = pd.reason,
            "Phase decision"
        );
    }

    // record_solve is called below AFTER the hold decision, so metrics are accurate.
    monitoring::record_phase(outcome.phase_reached, &outcome.phase_times_ms);
    if outcome.used_fallback {
        monitoring::record_fallback();
    }
    if !best_strategy.is_empty() {
        monitoring::record_strategy(best_strategy);
    }

    // Fire-and-forget: record auction to replay DB (A.7)
    let auction_json = String::from_utf8_lossy(&body).to_string();
    let solution_json = serde_json::to_string(&outcome.response).unwrap_or_default();
    tokio::spawn(replay::record_auction(replay::AuctionRecord {
        auction_id: auction_id.to_string(),
        chain_id,
        orders_count,
        auction_json,
        solution_json,
        our_score_wei: best_score_wei.to_string(),
        response_time_ms,
        strategies_used: format!("{:?}", outcome.strategies_ran),
        strategy_submitted: best_strategy.to_string(),
        used_fallback: outcome.used_fallback,
        result: result.to_string(),
    }));

    // Fire-and-forget: record predicted settlement to accounting DB (A.6)
    if solutions_found > 0 && should_submit {
        let sub_mode = submission_decision
            .as_ref()
            .map(|d| d.mode.as_str())
            .unwrap_or("standard")
            .to_string();
        tokio::spawn(accounting::record_predicted(accounting::PredictedSettlement {
            auction_id: auction_id.to_string(),
            chain_id,
            predicted_surplus_wei: best_score_wei.to_string(),
            predicted_gas_cost_wei: sim_result.gas_used.to_string(),
            predicted_net_score_wei: best_score_wei.to_string(),
            winning_strategy: best_strategy.to_string(),
            simulated: sim_result.simulated,
            submission_mode: sub_mode,
        }));
    }

    // Queue competition lookup for ALL auctions — even empties.
    // For submitted auctions: compare our score vs winner to measure accuracy.
    // For empty auctions: see what the winner scored (learn what we're missing).
    tokio::spawn(competition::queue_lookup(auction_id, best_score_wei));

    // Return empty if submission policy says hold
    if solutions_found > 0 && !should_submit {
        monitoring::record_solve(response_time_ms, solutions_found, best_score_wei, false);
        debug!(
            auction_id,
            reason = submission_decision.as_ref().map(|d| d.reason).unwrap_or(""),
            "Solution held by submission policy"
        );
        return (StatusCode::OK, Json(SolveResponse { solutions: vec![] })).into_response();
    }

    monitoring::record_solve(response_time_ms, solutions_found, best_score_wei, solutions_found > 0);
    (StatusCode::OK, Json(outcome.response)).into_response()
}
