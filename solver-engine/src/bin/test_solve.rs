//! Local test harness: run the solver against saved auctions from replay DB.
//!
//! Usage:
//!   # Test last 20 settled auctions from replay DB:
//!   cargo run --bin test-solve -- --db data/replay.db --last 20
//!
//!   # Test a specific auction:
//!   cargo run --bin test-solve -- --db data/replay.db --auction-id 6796229
//!
//!   # Show only auctions where we're competitive:
//!   cargo run --bin test-solve -- --db data/replay.db --last 50 --filter accurate
//!
//!   # Show detailed per-trade scoring for one auction:
//!   cargo run --bin test-solve -- --db data/replay.db --auction-id 6796229 --verbose

use std::time::Instant;

use clap::Parser;

use solver_engine::models::auction::AuctionInstance;
use solver_engine::models::solution::Score;
use solver_engine::solver;

#[derive(Parser)]
#[command(name = "test-solve", about = "Local test harness — run solver against saved auctions")]
struct Args {
    /// Path to replay.db (copied from VPS)
    #[arg(long)]
    db: String,

    /// Test last N settled auctions
    #[arg(long, default_value = "20")]
    last: u64,

    /// Test a specific auction by ID
    #[arg(long)]
    auction_id: Option<String>,

    /// Filter results: "all", "accurate" (0.5-2x), "inflated" (>10x), "low" (<0.5x)
    #[arg(long, default_value = "all")]
    filter: String,

    /// Show detailed per-trade scoring
    #[arg(long)]
    verbose: bool,

    /// Compare-only mode: use VPS-stored scores instead of re-solving.
    /// Much faster (no solve, just DB read) and shows what the VPS ACTUALLY submitted.
    #[arg(long)]
    compare: bool,
}

#[tokio::main]
async fn main() {
    // Initialize minimal logging (only warnings unless verbose)
    let filter = if std::env::var("RUST_LOG").is_ok() {
        std::env::var("RUST_LOG").unwrap()
    } else {
        "warn".to_string()
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&filter))
        .init();

    let args = Args::parse();

    // Point replay DB at the local file (unsafe in Rust 2024 but fine for a test tool)
    unsafe {
        std::env::set_var("REPLAY_DB_PATH", &args.db);
        if std::env::var("CHAIN_ID").is_err() {
            std::env::set_var("CHAIN_ID", "42161");
        }
        std::env::set_var("SIMULATION_ENABLED", "false");
        std::env::set_var("AGG_ENABLED", "false");
        std::env::set_var("EBBO_CHECK_ENABLED", "false"); // Skip EBBO in test harness
    }

    println!("═══════════════════════════════════════════════════════════════════");
    println!(" CoW Solver - Local Test Harness");
    println!(" DB: {}", args.db);
    println!("═══════════════════════════════════════════════════════════════════");
    println!();

    if args.compare {
        run_compare(args.last, &args.filter).await;
    } else if let Some(auction_id) = &args.auction_id {
        run_single(auction_id, args.verbose).await;
    } else {
        run_batch(args.last, &args.filter, args.verbose).await;
    }
}

/// Compare-only mode: read VPS-stored scores from DB, no re-solving.
/// This shows what the VPS ACTUALLY computed and submitted.
async fn run_compare(last_n: u64, filter: &str) {
    let rows = match solver_engine::replay::list_recent(last_n * 5) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("DB error: {}", e);
            return;
        }
    };

    let settled: Vec<_> = rows.iter()
        .filter(|r| {
            r.winning_score_wei.is_some()
                && r.winning_score_wei.as_deref() != Some("0")
                && r.winning_score_wei.as_deref() != Some("")
        })
        .take(last_n as usize)
        .collect();

    if settled.is_empty() {
        println!("  No settled auctions with winner data found.");
        return;
    }

    println!(" [COMPARE MODE — using VPS-stored scores, no re-solving]");
    println!();
    println!(" {:>11} {:>14} {:>14} {:>8} {:>10} {:>7}",
        "Auction", "VPS Score", "Winner", "Ratio", "Class", "Δ");
    println!(" {} {} {} {} {} {}",
        "─".repeat(11), "─".repeat(14), "─".repeat(14), "─".repeat(8),
        "─".repeat(10), "─".repeat(7));

    let mut total = 0u32;
    let mut accurate = 0u32;
    let mut high = 0u32;
    let mut inflated = 0u32;
    let mut low = 0u32;
    let mut very_low = 0u32;
    let mut would_win = 0u32;
    let mut ratios: Vec<f64> = Vec::new();

    for row in &settled {
        let our_wei: u128 = row.our_score_wei.parse().unwrap_or(0);
        let winner_wei: u128 = row.winning_score_wei.as_ref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if winner_wei == 0 || our_wei == 0 { continue; }

        let our_gwei = our_wei / 1_000_000_000;
        let win_gwei = winner_wei / 1_000_000_000;
        let ratio = our_wei as f64 / winner_wei as f64;
        let delta_pct = (ratio - 1.0) * 100.0;

        let class = if ratio >= 0.5 && ratio <= 2.0 { "ACCURATE" }
            else if ratio > 10.0 { "INFLATED" }
            else if ratio > 2.0 { "HIGH" }
            else if ratio >= 0.1 { "LOW" }
            else { "VERY_LOW" };

        let show = match filter {
            "accurate" => class == "ACCURATE",
            "inflated" => class == "INFLATED",
            "low" => class == "LOW" || class == "VERY_LOW",
            "high" => class == "HIGH",
            _ => true,
        };

        total += 1;
        match class {
            "ACCURATE" => accurate += 1,
            "HIGH" => high += 1,
            "INFLATED" => inflated += 1,
            "LOW" => low += 1,
            _ => very_low += 1,
        }
        if ratio >= 0.9 && ratio <= 1.1 && our_wei > winner_wei {
            would_win += 1;
        }
        ratios.push(ratio);

        if show {
            let delta_str = if delta_pct >= 0.0 {
                format!("+{:.1}%", delta_pct)
            } else {
                format!("{:.1}%", delta_pct)
            };
            let class_colored = match class {
                "ACCURATE" => format!("\x1b[32m{}\x1b[0m", class),
                "HIGH" => format!("\x1b[33m{}\x1b[0m", class),
                "INFLATED" => format!("\x1b[31m{}\x1b[0m", class),
                "LOW" => format!("\x1b[36m{}\x1b[0m", class),
                _ => format!("\x1b[35m{}\x1b[0m", class),
            };
            println!(" {:>11} {:>11} gw {:>11} gw {:>7.2}x {:>10} {:>7}",
                row.id, format_num(our_gwei), format_num(win_gwei),
                ratio, class_colored, delta_str);
        }
    }

    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = if !ratios.is_empty() { ratios[ratios.len() / 2] } else { 0.0 };

    println!();
    println!("═══════════════════════════════════════════════════════════════════");
    println!(" Summary (VPS-stored scores)");
    println!("═══════════════════════════════════════════════════════════════════");
    println!(" Total:      {} auctions", total);
    println!(" \x1b[32mACCURATE\x1b[0m:  {:>3} ({:.1}%)", accurate, accurate as f64 / total.max(1) as f64 * 100.0);
    println!(" \x1b[33mHIGH\x1b[0m:      {:>3} ({:.1}%)", high, high as f64 / total.max(1) as f64 * 100.0);
    println!(" \x1b[31mINFLATED\x1b[0m:  {:>3} ({:.1}%)", inflated, inflated as f64 / total.max(1) as f64 * 100.0);
    println!(" \x1b[36mLOW\x1b[0m:       {:>3} ({:.1}%)", low, low as f64 / total.max(1) as f64 * 100.0);
    println!(" \x1b[35mVERY_LOW\x1b[0m:  {:>3} ({:.1}%)", very_low, very_low as f64 / total.max(1) as f64 * 100.0);
    println!();
    println!(" Median ratio: {:.2}x", median);
    println!(" Would-win:    {} ({:.1}%)", would_win, would_win as f64 / total.max(1) as f64 * 100.0);
    println!("═══════════════════════════════════════════════════════════════════");
}

async fn run_single(auction_id: &str, verbose: bool) {
    let (auction_json, _solution_json) = match solver_engine::replay::load_auction(auction_id) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            eprintln!("Auction {} not found in replay DB", auction_id);
            return;
        }
        Err(e) => {
            eprintln!("DB error: {}", e);
            return;
        }
    };

    let auction: AuctionInstance = match serde_json::from_str(&auction_json) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Failed to deserialize auction {}: {}", auction_id, e);
            return;
        }
    };

    if verbose {
        // Enable info logging for scoring diagnostics
        // (tracing is already initialized, so we re-set env and hope the filter picks it up)
        println!("  Orders: {}, Pools: {}, Tokens: {}", auction.orders.len(), auction.liquidity.len(), auction.tokens.len());
    }

    let t0 = Instant::now();
    let outcome = solver::solve(auction).await;
    let elapsed_ms = t0.elapsed().as_millis();

    let best_score: u128 = outcome.response.solutions.iter()
        .filter_map(|s| s.score.as_ref())
        .find_map(|s| match s {
            Score::Solver { score } => score.parse().ok(),
            _ => None,
        })
        .unwrap_or(0);

    let score_gwei = best_score / 1_000_000_000;

    println!("  Auction:   {}", auction_id);
    println!("  Score:     {} gwei ({:.6} ETH)", format_num(score_gwei), best_score as f64 / 1e18);
    println!("  Strategy:  {}", outcome.winning_strategy);
    println!("  Solutions: {}", outcome.response.solutions.len());
    println!("  Phases:    {} reached", outcome.phase_reached);
    println!("  Time:      {}ms", elapsed_ms);

    if verbose {
        for (i, pd) in outcome.phase_decisions.iter().enumerate() {
            println!("  Phase {}: {} — {} candidates, score {}, improved: {}, reason: {}",
                pd.phase, pd.strategy, pd.candidates_produced, pd.best_score, pd.improved, pd.reason);
        }
    }
}

async fn run_batch(last_n: u64, filter: &str, verbose: bool) {
    let rows = match solver_engine::replay::list_recent(last_n * 2) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("DB error: {}", e);
            return;
        }
    };

    // Filter to settled auctions (have winner data)
    let settled: Vec<_> = rows.iter()
        .filter(|r| r.winning_score_wei.is_some() && r.winning_score_wei.as_deref() != Some("0"))
        .take(last_n as usize)
        .collect();

    if settled.is_empty() {
        println!("  No settled auctions with winner data found in last {} entries.", last_n * 2);
        println!("  Make sure the competition tracker has run (needs ~90s per auction).");
        return;
    }

    println!(" {:>11} {:>14} {:>14} {:>8} {:>10} {:>7} {:>6}",
        "Auction", "Our Score", "Winner", "Ratio", "Class", "Δ", "Time");
    println!(" {} {} {} {} {} {} {}",
        "─".repeat(11), "─".repeat(14), "─".repeat(14), "─".repeat(8),
        "─".repeat(10), "─".repeat(7), "─".repeat(6));

    let mut total = 0u32;
    let mut accurate = 0u32;
    let mut high = 0u32;
    let mut inflated = 0u32;
    let mut low = 0u32;
    let mut very_low = 0u32;
    let mut would_win = 0u32;
    let mut ratios: Vec<f64> = Vec::new();
    let mut total_solve_ms: u64 = 0;

    for row in &settled {
        let winner_wei: u128 = row.winning_score_wei.as_ref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if winner_wei == 0 { continue; }

        // Load and solve the auction
        let (auction_json, _) = match solver_engine::replay::load_auction(&row.id) {
            Ok(Some(pair)) => pair,
            _ => continue,
        };

        let auction: AuctionInstance = match serde_json::from_str(&auction_json) {
            Ok(a) => a,
            Err(_) => continue,
        };

        let t0 = Instant::now();
        let outcome = solver::solve(auction).await;
        let elapsed_ms = t0.elapsed().as_millis() as u64;

        let our_score: u128 = outcome.response.solutions.iter()
            .filter_map(|s| s.score.as_ref())
            .find_map(|s| match s {
                Score::Solver { score } => score.parse().ok(),
                _ => None,
            })
            .unwrap_or(0);

        let ratio = if winner_wei > 0 { our_score as f64 / winner_wei as f64 } else { 0.0 };
        let delta_pct = (ratio - 1.0) * 100.0;

        let class = if ratio >= 0.5 && ratio <= 2.0 { "ACCURATE" }
            else if ratio > 10.0 { "INFLATED" }
            else if ratio > 2.0 { "HIGH" }
            else if ratio >= 0.1 { "LOW" }
            else { "VERY_LOW" };

        // Apply filter
        let show = match filter {
            "accurate" => class == "ACCURATE",
            "inflated" => class == "INFLATED",
            "low" => class == "LOW" || class == "VERY_LOW",
            "high" => class == "HIGH",
            _ => true,
        };

        total += 1;
        match class {
            "ACCURATE" => accurate += 1,
            "HIGH" => high += 1,
            "INFLATED" => inflated += 1,
            "LOW" => low += 1,
            _ => very_low += 1,
        }
        if ratio >= 0.9 && ratio <= 1.1 && our_score > winner_wei {
            would_win += 1;
        }
        ratios.push(ratio);
        total_solve_ms += elapsed_ms;

        if show {
            let our_gwei = our_score / 1_000_000_000;
            let win_gwei = winner_wei / 1_000_000_000;

            let delta_str = if delta_pct >= 0.0 {
                format!("+{:.1}%", delta_pct)
            } else {
                format!("{:.1}%", delta_pct)
            };

            // Color-code the class (ANSI)
            let class_colored = match class {
                "ACCURATE" => format!("\x1b[32m{}\x1b[0m", class),   // green
                "HIGH" => format!("\x1b[33m{}\x1b[0m", class),       // yellow
                "INFLATED" => format!("\x1b[31m{}\x1b[0m", class),   // red
                "LOW" => format!("\x1b[36m{}\x1b[0m", class),        // cyan
                _ => format!("\x1b[35m{}\x1b[0m", class),            // magenta
            };

            println!(" {:>11} {:>11} gw {:>11} gw {:>7.2}x {:>10} {:>7} {:>4}ms",
                row.id,
                format_num(our_gwei),
                format_num(win_gwei),
                ratio,
                class_colored,
                delta_str,
                elapsed_ms,
            );

            if verbose {
                println!("    strategy={}, phases={}, solutions={}",
                    outcome.winning_strategy, outcome.phase_reached, outcome.response.solutions.len());
            }
        }
    }

    // Summary
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = if !ratios.is_empty() { ratios[ratios.len() / 2] } else { 0.0 };
    let avg_solve = if total > 0 { total_solve_ms / total as u64 } else { 0 };

    println!();
    println!("═══════════════════════════════════════════════════════════════════");
    println!(" Summary");
    println!("═══════════════════════════════════════════════════════════════════");
    println!(" Total:      {} auctions", total);
    println!(" \x1b[32mACCURATE\x1b[0m:  {:>3} ({:.1}%)  ← 0.5x - 2.0x", accurate, accurate as f64 / total.max(1) as f64 * 100.0);
    println!(" \x1b[33mHIGH\x1b[0m:      {:>3} ({:.1}%)  ← 2.0x - 10.0x", high, high as f64 / total.max(1) as f64 * 100.0);
    println!(" \x1b[31mINFLATED\x1b[0m:  {:>3} ({:.1}%)  ← > 10.0x", inflated, inflated as f64 / total.max(1) as f64 * 100.0);
    println!(" \x1b[36mLOW\x1b[0m:       {:>3} ({:.1}%)  ← 0.1x - 0.5x", low, low as f64 / total.max(1) as f64 * 100.0);
    println!(" \x1b[35mVERY_LOW\x1b[0m:  {:>3} ({:.1}%)  ← < 0.1x", very_low, very_low as f64 / total.max(1) as f64 * 100.0);
    println!();
    println!(" Median ratio: {:.2}x", median);
    println!(" Would-win:    {} ({:.1}%)  ← ratio 0.9x-1.1x AND our > winner", would_win, would_win as f64 / total.max(1) as f64 * 100.0);
    println!(" Avg solve:    {}ms", avg_solve);
    println!("═══════════════════════════════════════════════════════════════════");
}

fn format_num(n: u128) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { result.push(','); }
        result.push(c);
    }
    result.chars().rev().collect()
}
