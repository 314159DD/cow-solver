//! Auction audit tool — deep-dive comparison of our solution vs winner.
//!
//! Usage:
//!   cargo run --bin audit -- --auction-id 6787407
//!   cargo run --bin audit -- --last 10
//!   cargo run --bin audit -- --summary

use std::collections::HashMap;
use std::path::Path;

use clap::Parser;

#[derive(Parser)]
#[command(name = "audit", about = "Audit auction scoring vs competition")]
struct Args {
    /// Analyze a specific auction by ID
    #[arg(long)]
    auction_id: Option<u64>,

    /// Show summary of last N compared auctions
    #[arg(long, default_value = "20")]
    last: usize,

    /// Show overall scoring diagnosis
    #[arg(long)]
    summary: bool,
}

fn main() {
    let args = Args::parse();

    if let Some(auction_id) = args.auction_id {
        audit_auction(auction_id);
    } else if args.summary {
        show_summary();
    } else {
        show_recent(args.last);
    }
}

fn audit_auction(auction_id: u64) {
    // Try to load competition JSON
    let comp_path = format!("data/competition/{}.json", auction_id);
    if Path::new(&comp_path).exists() {
        let json = std::fs::read_to_string(&comp_path).unwrap_or_default();
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json) {
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("Auction #{}", auction_id);
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

            // Parse solutions
            let solutions = data["solutions"].as_array()
                .or_else(|| data["auctionResult"]["solutions"].as_array());

            if let Some(solutions) = solutions {
                println!("\n{} solvers submitted solutions:\n", solutions.len());
                for (i, sol) in solutions.iter().enumerate() {
                    let solver = sol["solverName"].as_str()
                        .or_else(|| sol["solver"].as_str())
                        .unwrap_or("unknown");
                    let score: String = sol["score"].as_str().map(|s| s.to_string())
                        .or_else(|| sol["score"].as_u64().map(|n| n.to_string()))
                        .unwrap_or_else(|| "0".to_string());
                    let ranking = sol["ranking"].as_u64().unwrap_or(0);
                    let orders = sol["orders"].as_array().map(|a| a.len()).unwrap_or(0);

                    let score_u128: u128 = score.parse::<u128>().unwrap_or(0);
                    let score_gwei = score_u128 as f64 / 1e9;
                    let score_eth = score_u128 as f64 / 1e18;

                    let marker = if ranking == 1 { " ← WINNER" } else { "" };
                    println!("  #{} {} — score: {} gwei ({:.6} ETH), {} orders{}",
                        ranking, solver, score_gwei as u64, score_eth, orders, marker);
                }
            }
        } else {
            println!("Failed to parse competition JSON for auction {}", auction_id);
        }
    } else {
        println!("No competition data stored for auction {}.", auction_id);
        println!("Competition JSONs are stored in data/competition/");
        println!("The solver must be running with COMPETITION_ENABLED=true");
    }
}

fn show_recent(n: usize) {
    let dir = "data/competition";
    if !Path::new(dir).exists() {
        println!("No competition data directory. Run the solver first.");
        return;
    }

    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "json").unwrap_or(false))
        .collect();

    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    files.truncate(n);

    if files.is_empty() {
        println!("No competition data files found.");
        return;
    }

    println!("Last {} compared auctions:\n", files.len());
    println!("{:<12} {:>15} {:>15} {:>10} {}", "Auction", "Winner Score", "Our Score*", "Ratio", "Winner");
    println!("{}", "-".repeat(70));

    for file in &files {
        let json = std::fs::read_to_string(file.path()).unwrap_or_default();
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json) {
            let solutions = data["solutions"].as_array()
                .or_else(|| data["auctionResult"]["solutions"].as_array());

            if let Some(solutions) = solutions {
                let winner = solutions.iter()
                    .find(|s| s["ranking"].as_u64() == Some(1))
                    .or_else(|| solutions.first());

                if let Some(winner) = winner {
                    let path = file.path();
                    let auction_id = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?");
                    let solver = winner["solverName"].as_str().unwrap_or("unknown");
                    let score_str: String = winner["score"].as_str().map(|s| s.to_string())
                        .or_else(|| winner["score"].as_u64().map(|n| n.to_string()))
                        .unwrap_or_else(|| "0".to_string());
                    let score: u128 = score_str.parse().unwrap_or(0);
                    let score_gwei = score as f64 / 1e9;

                    println!("{:<12} {:>12.0} gwei {:>12} {:>10} {}",
                        auction_id, score_gwei, "N/A*", "N/A", solver);
                }
            }
        }
    }

    println!("\n* Our score is stored in replay.db, not in competition JSON.");
    println!("  Use --auction-id <id> for detailed per-auction analysis.");
}

fn show_summary() {
    let dir = "data/competition";
    if !Path::new(dir).exists() {
        println!("No competition data directory.");
        return;
    }

    let files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "json").unwrap_or(false))
        .collect();

    println!("Competition data: {} auctions stored", files.len());

    // Collect winner scores
    let mut winner_scores: Vec<f64> = Vec::new();
    let mut solver_wins: HashMap<String, usize> = HashMap::new();

    for file in &files {
        let json = std::fs::read_to_string(file.path()).unwrap_or_default();
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json) {
            let solutions = data["solutions"].as_array()
                .or_else(|| data["auctionResult"]["solutions"].as_array());

            if let Some(solutions) = solutions {
                if let Some(winner) = solutions.iter().find(|s| s["ranking"].as_u64() == Some(1)) {
                    let score: u128 = winner["score"].as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    if score > 0 {
                        winner_scores.push(score as f64 / 1e9); // gwei
                    }
                    let name = winner["solverName"].as_str().unwrap_or("unknown").to_string();
                    *solver_wins.entry(name).or_insert(0) += 1;
                }
            }
        }
    }

    if winner_scores.is_empty() {
        println!("No winner scores found.");
        return;
    }

    winner_scores.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = winner_scores[winner_scores.len() / 2];
    let avg: f64 = winner_scores.iter().sum::<f64>() / winner_scores.len() as f64;
    let min = winner_scores.first().unwrap();
    let max = winner_scores.last().unwrap();

    println!("\nWinner score distribution (gwei):");
    println!("  Min:    {:.0}", min);
    println!("  Median: {:.0}", median);
    println!("  Avg:    {:.0}", avg);
    println!("  Max:    {:.0}", max);

    println!("\nTop winners:");
    let mut sorted_winners: Vec<_> = solver_wins.into_iter().collect();
    sorted_winners.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, count) in sorted_winners.iter().take(10) {
        println!("  {} — {} wins", name, count);
    }
}
