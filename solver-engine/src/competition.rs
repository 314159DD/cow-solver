//! Shadow Competition Tracker
//!
//! Queries the CoW API after each auction to fetch the actual winner's score,
//! computes our rank and gap, and stores the results for dashboard display.
//!
//! ## Kill Switch
//! Set `COMPETITION_ENABLED=false` to disable.
//!
//! ## Required Config
//! - `COW_API_BASE` — CoW API base URL (default: `https://api.cow.fi/arbitrum`)
//! - `SOLVER_ADDRESS` — our solver's Ethereum address (to find ourselves in rankings)
//! - `COMPETITION_POLL_DELAY_SECS` — delay before querying results (default: 90)

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ── Types ────────────────────────────────────────────────────────────────────

/// A queued auction waiting for competition result lookup.
#[derive(Debug, Clone)]
pub struct PendingLookup {
    pub auction_id: u64,
    pub our_score_wei: u128,
    pub queued_at: u64, // unix seconds
    pub retries: u8,
}

/// Result of a competition lookup for one auction.
#[derive(Debug, Clone, Default)]
pub struct CompetitionResult {
    pub auction_id: u64,
    pub winner_solver: String,
    pub winner_score_wei: u128,
    pub our_score_wei: u128,
    pub our_rank: u32,
    pub total_solvers: u32,
    pub score_delta_wei: i128, // positive = we beat winner, negative = we lost
    pub score_delta_pct: f64,  // % gap to winner (negative = behind)
}

/// Aggregated competitiveness stats for the dashboard.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CompetitivenessStats {
    pub total_compared: u64,
    pub wins: u64,
    pub within_10pct: u64,
    pub within_50pct: u64,
    pub median_delta_pct: f64,
    pub best_rank: u32,
    pub avg_rank: f64,
    pub top_winner: String,
    pub top_winner_count: u64,
    /// Theoretical earnings if we had won (in gwei)
    pub theoretical_earnings_gwei: u64,
    /// Theoretical earnings in ETH (gwei / 1e9)
    pub theoretical_earnings_eth: f64,
    /// Per-win average earnings in gwei
    pub per_win_avg_gwei: f64,
    /// Total surplus across ALL auctions we participated in (gwei) — shows what we generated
    pub total_surplus_gwei: u64,
    /// Closest miss: best delta_pct where we didn't win
    pub closest_miss_pct: f64,
    pub closest_miss_auction: u64,
    /// Closest miss surplus in gwei (what we would have earned on the closest miss)
    pub closest_miss_surplus_gwei: f64,
    /// Scoring accuracy: % of settled auctions where our score is within 2x of winner
    pub score_accuracy_pct: f64,
    /// Scoring accuracy: % where score is still >10x inflated (formula bug indicator)
    pub score_inflated_pct: f64,
}

// ── Global queue ─────────────────────────────────────────────────────────────

static PENDING_QUEUE: OnceLock<Arc<Mutex<VecDeque<PendingLookup>>>> = OnceLock::new();
static RESULTS: OnceLock<Arc<Mutex<Vec<CompetitionResult>>>> = OnceLock::new();

fn pending_queue() -> &'static Arc<Mutex<VecDeque<PendingLookup>>> {
    PENDING_QUEUE.get_or_init(|| Arc::new(Mutex::new(VecDeque::new())))
}

fn results_store() -> &'static Arc<Mutex<Vec<CompetitionResult>>> {
    RESULTS.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Queue an auction for competition result lookup.
/// Called from the solve handler after each submitted solution.
pub async fn queue_lookup(auction_id: u64, our_score_wei: u128) {
    if !is_enabled() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut q = pending_queue().lock().await;
    // Cap queue size to prevent memory leaks
    if q.len() > 10_000 {
        q.pop_front();
    }
    q.push_back(PendingLookup {
        auction_id,
        our_score_wei,
        queued_at: now,
        retries: 0,
    });
}

/// Bootstrap the in-memory competition results from the replay DB.
/// Called once at startup so the dashboard isn't empty after a restart.
pub async fn bootstrap_from_replay() {
    match crate::replay::load_competition_history(2000) {
        Ok(results) if !results.is_empty() => {
            let count = results.len();
            let mut store = results_store().lock().await;
            // Prepend historical data (oldest first, newest last)
            let mut historical = results;
            historical.reverse(); // replay returns newest-first, we want chronological
            historical.append(&mut *store);
            *store = historical;
            info!(bootstrapped = count, "Loaded competition history from replay DB");
        }
        Ok(_) => {
            debug!("No competition history in replay DB to bootstrap");
        }
        Err(e) => {
            warn!(error = %e, "Failed to bootstrap competition history from replay DB");
        }
    }
}

/// Get aggregated competitiveness stats for the dashboard.
pub async fn get_stats() -> CompetitivenessStats {
    let results = results_store().lock().await;
    if results.is_empty() {
        return CompetitivenessStats::default();
    }

    let total = results.len() as u64;
    let wins = results.iter().filter(|r| r.our_rank == 1).count() as u64;
    let within_10 = results.iter().filter(|r| r.score_delta_pct.abs() <= 10.0 || r.our_rank == 1).count() as u64;
    let within_50 = results.iter().filter(|r| r.score_delta_pct.abs() <= 50.0 || r.our_rank == 1).count() as u64;

    // Median delta
    let mut deltas: Vec<f64> = results.iter().map(|r| r.score_delta_pct).collect();
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if deltas.is_empty() { 0.0 } else { deltas[deltas.len() / 2] };

    // Best rank
    let best_rank = results.iter().map(|r| r.our_rank).min().unwrap_or(0);
    let avg_rank = results.iter().map(|r| r.our_rank as f64).sum::<f64>() / total as f64;

    // Top winner (most frequent winning solver)
    let mut winner_counts: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for r in results.iter() {
        if !r.winner_solver.is_empty() {
            *winner_counts.entry(&r.winner_solver).or_default() += 1;
        }
    }
    let (top_winner, top_count) = winner_counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(name, count)| (name.to_string(), *count))
        .unwrap_or_default();

    // Theoretical earnings: sum of our net surplus for auctions where we were #1
    let theoretical_gwei: u64 = results
        .iter()
        .filter(|r| r.our_rank == 1)
        .map(|r| (r.our_score_wei / 1_000_000_000) as u64)
        .sum();
    let theoretical_eth = theoretical_gwei as f64 / 1_000_000_000.0;
    let per_win_avg_gwei = if wins > 0 {
        theoretical_gwei as f64 / wins as f64
    } else {
        0.0
    };

    // Total surplus across all auctions (not just wins)
    let total_surplus_gwei: u64 = results
        .iter()
        .map(|r| (r.our_score_wei / 1_000_000_000) as u64)
        .sum();

    // Closest miss: best delta where we didn't win
    let closest = results
        .iter()
        .filter(|r| r.our_rank > 1 && r.score_delta_pct > -100.0)
        .min_by(|a, b| a.score_delta_pct.abs().partial_cmp(&b.score_delta_pct.abs()).unwrap_or(std::cmp::Ordering::Equal));

    let closest_miss_surplus_gwei = closest
        .map(|c| c.our_score_wei as f64 / 1_000_000_000.0)
        .unwrap_or(0.0);

    // Scoring accuracy validation: how often is our score in a reasonable range?
    let settled: Vec<_> = results.iter().filter(|r| r.winner_score_wei > 0).collect();
    let settled_count = settled.len() as f64;
    let (score_accuracy_pct, score_inflated_pct) = if settled_count > 0.0 {
        let accurate = settled.iter().filter(|r| {
            let ratio = r.our_score_wei as f64 / r.winner_score_wei.max(1) as f64;
            ratio >= 0.5 && ratio <= 2.0
        }).count() as f64;
        let inflated = settled.iter().filter(|r| {
            let ratio = r.our_score_wei as f64 / r.winner_score_wei.max(1) as f64;
            ratio > 10.0
        }).count() as f64;
        (accurate / settled_count * 100.0, inflated / settled_count * 100.0)
    } else {
        (0.0, 0.0)
    };

    CompetitivenessStats {
        total_compared: total,
        wins,
        within_10pct: within_10,
        within_50pct: within_50,
        median_delta_pct: median,
        best_rank,
        avg_rank,
        top_winner,
        top_winner_count: top_count,
        theoretical_earnings_gwei: theoretical_gwei,
        theoretical_earnings_eth: theoretical_eth,
        per_win_avg_gwei,
        total_surplus_gwei,
        closest_miss_pct: closest.map(|c| c.score_delta_pct).unwrap_or(0.0),
        closest_miss_auction: closest.map(|c| c.auction_id).unwrap_or(0),
        closest_miss_surplus_gwei,
        score_accuracy_pct,
        score_inflated_pct,
    }
}

// ── Background polling task ─────────────────────────────────────────────────

pub fn is_enabled() -> bool {
    std::env::var("COMPETITION_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true)
}

fn api_base() -> String {
    std::env::var("COW_API_BASE")
        .unwrap_or_else(|_| "https://api.cow.fi/arbitrum_one".to_string())
}

fn poll_delay_secs() -> u64 {
    std::env::var("COMPETITION_POLL_DELAY_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15) // 15s — Arbitrum settles fast, we want coverage
}

/// Run the competition polling background task.
/// Drains the pending queue, waits for settlement, queries CoW API.
pub async fn run_poll_task() {
    if !is_enabled() {
        info!("Competition tracker disabled via kill switch");
        return;
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let delay = poll_delay_secs();
    info!(delay_secs = delay, "Competition tracker started");

    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Drain items that are old enough
        let mut ready = Vec::new();
        {
            let mut q = pending_queue().lock().await;
            while let Some(front) = q.front() {
                if now.saturating_sub(front.queued_at) >= delay {
                    ready.push(q.pop_front().unwrap());
                } else {
                    break;
                }
            }
        }

        for lookup in ready {
            // Rate limit: 500ms between API calls (CoW API is generous)
            tokio::time::sleep(Duration::from_millis(500)).await;

            match fetch_competition(&client, lookup.auction_id).await {
                Ok(Some(result)) => {
                    let cr = CompetitionResult {
                        auction_id: lookup.auction_id,
                        winner_solver: result.winner_solver.clone(),
                        winner_score_wei: result.winner_score_wei,
                        our_score_wei: lookup.our_score_wei,
                        our_rank: result.our_rank,
                        total_solvers: result.total_solvers,
                        score_delta_wei: lookup.our_score_wei as i128 - result.winner_score_wei as i128,
                        score_delta_pct: if result.winner_score_wei > 0 {
                            (lookup.our_score_wei as f64 / result.winner_score_wei as f64 - 1.0) * 100.0
                        } else {
                            0.0
                        },
                    };

                    // Scoring formula validation: compare our score to winner
                    // After the CoW formula port, scores should be in the same
                    // ballpark as winners when routing through similar pools.
                    // Log accuracy bands so we can verify the fix is working.
                    if cr.winner_score_wei > 0 {
                        let ratio = cr.our_score_wei as f64 / cr.winner_score_wei as f64;
                        let accuracy = if ratio >= 0.5 && ratio <= 2.0 {
                            "ACCURATE"   // within 2x — formula is working
                        } else if ratio > 2.0 && ratio <= 10.0 {
                            "HIGH"       // 2-10x — possible stale pool data
                        } else if ratio > 10.0 {
                            "INFLATED"   // >10x — formula still broken
                        } else if ratio >= 0.1 {
                            "LOW"        // 0.1-0.5x — routing worse but formula ok
                        } else {
                            "VERY_LOW"   // <0.1x — different order set
                        };
                        info!(
                            auction_id = cr.auction_id,
                            ratio = format!("{:.2}x", ratio),
                            accuracy,
                            our = cr.our_score_wei,
                            winner = cr.winner_score_wei,
                            "SCORE_VALIDATION"
                        );
                    }

                    // Classify the result for competitive analysis
                    let classification = if cr.winner_score_wei == 0 {
                        "no_settle"
                    } else if cr.score_delta_pct >= -10.0 && cr.score_delta_pct <= 100.0 {
                        "competitive" // within range
                    } else if cr.score_delta_pct > 100.0 {
                        "inflated" // our score too high (stale data)
                    } else if cr.score_delta_pct < -80.0 {
                        "outclassed" // big auction we can't compete on
                    } else {
                        "close_loss" // -10% to -80% — winnable with better routing
                    };

                    let winner_eth = cr.winner_score_wei as f64 / 1e18;
                    let our_eth = cr.our_score_wei as f64 / 1e18;

                    info!(
                        auction_id = cr.auction_id,
                        winner = %cr.winner_solver,
                        winner_score = cr.winner_score_wei,
                        our_score = cr.our_score_wei,
                        our_rank = cr.our_rank,
                        total_solvers = cr.total_solvers,
                        delta_pct = format!("{:.1}%", cr.score_delta_pct),
                        class = classification,
                        winner_eth = format!("{:.6}", winner_eth),
                        our_eth = format!("{:.6}", our_eth),
                        winner_trades = result.winner_trades_count,
                        winner_orders_filled = result.winner_order_uids.len(),
                        "Competition result"
                    );

                    // Store raw competition JSON for offline audit tool
                    store_competition_json(lookup.auction_id, &result.raw_json);

                    // Update replay DB
                    if let Err(e) = crate::replay::update_competition_result(
                        &lookup.auction_id.to_string(),
                        &cr.winner_solver,
                        &cr.winner_score_wei.to_string(),
                    ) {
                        warn!(error = %e, auction_id = lookup.auction_id, "Failed to update replay DB with competition result");
                    }

                    // Store in memory for dashboard
                    let mut store = results_store().lock().await;
                    if store.len() > 10_000 {
                        store.drain(0..1000); // Keep last ~9000
                    }
                    store.push(cr);
                }
                Ok(None) => {
                    // Auction didn't settle yet or was empty. Retry once after 60s.
                    if lookup.retries == 0 {
                        let mut retry = lookup.clone();
                        retry.retries = 1;
                        retry.queued_at = now; // re-queue with fresh timestamp
                        let mut q = pending_queue().lock().await;
                        q.push_back(retry);
                    } else {
                        // Already retried — mark as "no settlement" for dashboard
                        let cr = CompetitionResult {
                            auction_id: lookup.auction_id,
                            winner_solver: "none".to_string(),
                            winner_score_wei: 0,
                            our_score_wei: lookup.our_score_wei,
                            our_rank: 0,
                            total_solvers: 0,
                            score_delta_wei: 0,
                            score_delta_pct: 0.0,
                        };
                        // Update replay DB so dashboard shows "no settle"
                        let _ = crate::replay::update_competition_result(
                            &lookup.auction_id.to_string(),
                            "none",
                            "0",
                        );
                        let mut store = results_store().lock().await;
                        store.push(cr);
                    }
                }
                Err(e) => {
                    debug!(auction_id = lookup.auction_id, error = %e, "Competition lookup failed");
                }
            }
        }
    }
}

// ── CoW API client ──────────────────────────────────────────────────────────

struct RawCompetitionResult {
    winner_solver: String,
    winner_score_wei: u128,
    our_rank: u32,
    total_solvers: u32,
    /// Number of trades in the winning solution
    winner_trades_count: u32,
    /// Order UIDs the winner filled (lowercase)
    winner_order_uids: Vec<String>,
    /// Full JSON of the competition response (for offline analysis)
    raw_json: String,
}

async fn fetch_competition(
    client: &reqwest::Client,
    auction_id: u64,
) -> Result<Option<RawCompetitionResult>, String> {
    let url = format!("{}/api/v1/solver_competition/{}", api_base(), auction_id);

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    if !resp.status().is_success() {
        return Err(format!("API returned {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    // Parse the competition response
    let solutions = body["solutions"].as_array()
        .or_else(|| body["auctionResult"]["solutions"].as_array());

    let solutions = match solutions {
        Some(s) => s,
        None => return Ok(None),
    };

    if solutions.is_empty() {
        return Ok(None);
    }

    // Find the actual winner: look for isWinner=true first, fall back to ranking=1,
    // then fall back to highest score.
    let winner = solutions.iter()
        .find(|s| s["isWinner"].as_bool() == Some(true))
        .or_else(|| solutions.iter().find(|s| s["ranking"].as_u64() == Some(1)))
        .or_else(|| solutions.iter().max_by_key(|s| {
            s["score"].as_u64().unwrap_or(0)
        }))
        .unwrap_or(&solutions[0]);

    let winner_solver = winner["solver"].as_str()
        .or_else(|| winner["solverName"].as_str())
        .unwrap_or("unknown")
        .to_string();

    // score can be a JSON number OR a string depending on the API version
    let winner_score_wei: u128 = winner["score"].as_u64().map(|n| n as u128)
        .or_else(|| winner["score"].as_str().and_then(|s| s.parse().ok()))
        .or_else(|| winner["objective"]["total"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0);

    // Find our rank (by solver address)
    let solver_addr = std::env::var("SOLVER_ADDRESS").unwrap_or_default().to_lowercase();
    let mut our_rank = 0u32;
    let total_solvers = solutions.len() as u32;

    if !solver_addr.is_empty() {
        for (i, sol) in solutions.iter().enumerate() {
            let addr = sol["solverAddress"].as_str()
                .or_else(|| sol["solver"].as_str())
                .unwrap_or("").to_lowercase();
            if addr == solver_addr {
                our_rank = sol["ranking"].as_u64().unwrap_or((i + 1) as u64) as u32;
                break;
            }
        }
    }

    // If we didn't find ourselves, rank = total + 1 (unranked)
    if our_rank == 0 {
        our_rank = total_solvers + 1;
    }

    // Extract winner's trade details
    let winner_trades = winner["orders"].as_array()
        .or_else(|| winner["trades"].as_array());
    let winner_trades_count = winner_trades.map(|t| t.len() as u32).unwrap_or(0);
    let winner_order_uids: Vec<String> = winner_trades
        .map(|trades| {
            trades.iter()
                .filter_map(|t| {
                    t["id"].as_str()
                        .or_else(|| t["orderUid"].as_str())
                        .or_else(|| t["uid"].as_str())
                        .map(|s| s.to_lowercase())
                })
                .collect()
        })
        .unwrap_or_default();

    // Store raw JSON for offline analysis
    let raw_json = serde_json::to_string(&body).unwrap_or_default();

    Ok(Some(RawCompetitionResult {
        winner_solver,
        winner_score_wei,
        our_rank,
        total_solvers,
        winner_trades_count,
        winner_order_uids,
        raw_json,
    }))
}

// ── Competition JSON storage (for offline audit) ────────────────────────────

fn store_competition_json(auction_id: u64, json: &str) {
    use std::fs;
    let dir = "data/competition";
    let _ = fs::create_dir_all(dir);
    // Keep last 500 files, rotate by deleting oldest
    let path = format!("{}/{}.json", dir, auction_id);
    if let Err(e) = fs::write(&path, json) {
        tracing::debug!(error = %e, "Failed to write competition JSON");
    }
}

/// Score anatomy for a single compared auction — breaks down WHERE score comes from.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoreAnatomyEntry {
    pub auction_id: u64,
    pub our_score_gwei: f64,
    pub winner_score_gwei: f64,
    pub ratio: f64,
    pub our_trades: u32,
    pub winner_trades: u32,
    pub classification: String,
}

/// Get score anatomy for the last N compared auctions.
pub async fn score_anatomy(limit: usize) -> Vec<ScoreAnatomyEntry> {
    let store = results_store().lock().await;
    store.iter()
        .rev()
        .filter(|cr| cr.winner_score_wei > 0) // only settled auctions
        .take(limit)
        .map(|cr| {
            let our_gwei = cr.our_score_wei as f64 / 1e9;
            let winner_gwei = cr.winner_score_wei as f64 / 1e9;
            let ratio = if winner_gwei > 0.0 { our_gwei / winner_gwei } else { 0.0 };
            let classification = if ratio >= 0.5 && ratio <= 2.0 {
                "accurate"
            } else if ratio > 10.0 {
                "inflated"
            } else if ratio > 2.0 {
                "high"
            } else if ratio >= 0.1 {
                "low"
            } else {
                "very_low"
            }.to_string();

            ScoreAnatomyEntry {
                auction_id: cr.auction_id,
                our_score_gwei: our_gwei,
                winner_score_gwei: winner_gwei,
                ratio,
                our_trades: 0, // TODO: store trade count
                winner_trades: 0,
                classification,
            }
        })
        .collect()
}

/// Auto-diagnosis: summarize scoring patterns over last N compared auctions.
pub async fn auto_diagnosis(n: usize) -> String {
    let store = results_store().lock().await;
    let settled: Vec<&CompetitionResult> = store.iter()
        .filter(|cr| cr.winner_score_wei > 0)
        .collect();

    if settled.is_empty() {
        return "No settled auctions yet — waiting for competition data.".to_string();
    }

    let recent: Vec<&CompetitionResult> = settled.iter()
        .rev()
        .take(n)
        .copied()
        .collect();

    let total = recent.len();
    let mut accurate = 0usize;
    let mut inflated = 0usize;
    let mut avg_ratio = 0.0f64;

    for cr in &recent {
        let ratio = cr.our_score_wei as f64 / cr.winner_score_wei.max(1) as f64;
        avg_ratio += ratio;
        if ratio >= 0.5 && ratio <= 2.0 { accurate += 1; }
        if ratio > 10.0 { inflated += 1; }
    }

    avg_ratio /= total as f64;

    let diagnosis = if inflated as f64 / total as f64 > 0.5 {
        "SCORING BUG: >50% of auctions have inflated scores (>10x winner). Fix scoring formula."
    } else if accurate as f64 / total as f64 > 0.5 {
        "SCORING OK: >50% of scores are within 2x of winner. Focus on routing improvements."
    } else if avg_ratio < 0.5 {
        "ROUTING WEAK: Scores are consistently below winners. Need better pool coverage or routing."
    } else {
        "MIXED: Some scores accurate, some inflated. Check specific trade types."
    };

    format!(
        "Last {} auctions: {:.0}% accurate, {:.0}% inflated, avg ratio {:.1}x. {}",
        total,
        accurate as f64 / total as f64 * 100.0,
        inflated as f64 / total as f64 * 100.0,
        avg_ratio,
        diagnosis
    )
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queue_and_stats_empty() {
        let stats = get_stats().await;
        assert_eq!(stats.total_compared, 0);
        assert_eq!(stats.wins, 0);
    }

    #[test]
    fn default_api_base() {
        // Should default to Arbitrum
        let base = api_base();
        assert!(base.contains("cow.fi") || base.contains("localhost"));
    }
}
