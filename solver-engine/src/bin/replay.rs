//! Auction Replay CLI (A.7)
//!
//! Usage:
//!   cargo run --bin replay -- --summary
//!   cargo run --bin replay -- --last 100 --summary
//!   cargo run --bin replay -- --last 100 --by-strategy
//!   cargo run --bin replay -- --list 20
//!   cargo run --bin replay -- --auction-id <ID>

use clap::Parser;
use solver_engine::replay;

#[derive(Parser)]
#[command(name = "replay", about = "Auction replay and analysis tool")]
struct Cli {
    /// Only consider the last N auctions
    #[arg(long)]
    last: Option<u64>,

    /// Show summary statistics
    #[arg(long)]
    summary: bool,

    /// Show per-strategy breakdown
    #[arg(long)]
    by_strategy: bool,

    /// List recent auction IDs
    #[arg(long)]
    list: Option<u64>,

    /// Load and display a specific auction by ID
    #[arg(long)]
    auction_id: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    if cli.summary {
        match replay::query_summary(cli.last) {
            Ok(s) => {
                println!("=== Auction Replay Summary ===");
                println!("Total auctions:   {}", s.total_auctions);
                println!("Submitted:        {} ({:.1}%)", s.submitted, pct(s.submitted, s.total_auctions));
                println!("Empty:            {} ({:.1}%)", s.empty, pct(s.empty, s.total_auctions));
                println!("Errors:           {} ({:.1}%)", s.errors, pct(s.errors, s.total_auctions));
                println!("Avg response:     {:.0} ms", s.avg_response_ms);
                println!("Fallback used:    {} ({:.1}%)", s.fallback_count, pct(s.fallback_count, s.total_auctions));
                println!("Avg score (wei):  {:.0}", s.avg_score_wei);
            }
            Err(e) => eprintln!("Error querying summary: {e}"),
        }
    }

    if cli.by_strategy {
        match replay::query_by_strategy(cli.last) {
            Ok(strategies) => {
                println!("\n=== Strategy Breakdown ===");
                println!("{:<20} {:>10} {:>20}", "Strategy", "Submitted", "Avg Score (wei)");
                println!("{}", "-".repeat(52));
                for s in &strategies {
                    println!("{:<20} {:>10} {:>20.0}", s.strategy, s.times_submitted, s.avg_score_wei);
                }
            }
            Err(e) => eprintln!("Error querying strategies: {e}"),
        }
    }

    if let Some(n) = cli.list {
        match replay::list_recent(n) {
            Ok(auctions) => {
                println!("\n=== Recent Auctions ===");
                println!("{:<40} {:>12} {:>10} {:>8} {:>12} {:>12}", "Auction ID", "Timestamp", "Result", "Time(ms)", "Our Score", "Winner");
                println!("{}", "-".repeat(96));
                for row in &auctions {
                    let winner = row.winning_solver.as_deref().unwrap_or("-");
                    println!("{:<40} {:>12} {:>10} {:>8} {:>12} {:>12}",
                        row.id, row.received_at, row.result, row.response_time_ms,
                        row.our_score_wei, winner);
                }
            }
            Err(e) => eprintln!("Error listing auctions: {e}"),
        }
    }

    if let Some(ref id) = cli.auction_id {
        match replay::load_auction(id) {
            Ok(Some((auction, solution))) => {
                println!("\n=== Auction: {} ===", id);
                println!("--- Auction JSON ---");
                println!("{auction}");
                println!("--- Solution JSON ---");
                println!("{solution}");
            }
            Ok(None) => eprintln!("Auction {id} not found"),
            Err(e) => eprintln!("Error loading auction: {e}"),
        }
    }

    if !cli.summary && !cli.by_strategy && cli.list.is_none() && cli.auction_id.is_none() {
        println!("No action specified. Use --help for usage.");
    }
}

fn pct(part: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { (part as f64 / total as f64) * 100.0 }
}
