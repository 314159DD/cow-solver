//! bench_route_quality — compares our solver score against historical winning
//! solution scores from real CoW Protocol auctions.
//!
//! Score ratio = our_score / winning_score.
//! A ratio of 1.0 means we match the historical winner; > 1.0 means we beat it.
//! Currently the solver stubs return empty solutions (ratio 0.0), which
//! establishes the baseline before real strategies are implemented.
//!
//! Run: cargo bench -p benchmarks --bench route_quality

use benchmarks::{load_historical_auctions, make_auction};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use solver_engine::solver::solve;

/// Parse a decimal-string score as f64, returning 0.0 on failure.
fn parse_score(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
}

/// Compute a rough numeric score from the solution:
/// sum of fulfillment trade executed_amounts (proxy for volume captured).
fn solution_score(response: &solver_engine::models::solution::SolveResponse) -> f64 {
    use solver_engine::models::solution::Trade;
    response
        .solutions
        .iter()
        .flat_map(|s| s.trades.iter())
        .map(|t| match t {
            Trade::Fulfillment(f) => f.executed_amount.parse::<f64>().unwrap_or(0.0),
        })
        .sum()
}

fn bench_route_quality(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let historical = load_historical_auctions();

    let mut group = c.benchmark_group("route_quality");

    if historical.is_empty() {
        // No historical data yet — fall back to synthetic fixtures and report 0.0 baseline
        eprintln!("[route_quality] No historical auction files found in benchmarks/data/auctions/");
        eprintln!("[route_quality] Benchmarking against synthetic fixtures (score ratio will be 0.0 until strategies are implemented)");

        for n_orders in [1usize, 5, 10] {
            group.bench_with_input(
                BenchmarkId::new("synthetic_score_ratio", n_orders),
                &n_orders,
                |b, &n| {
                    b.iter(|| {
                        let auction = make_auction(n);
                        let response = rt.block_on(async { solve(auction).await });
                        let our_score = solution_score(&response);
                        // Synthetic "winning score" = 1 unit per order (placeholder)
                        let winning_score = n as f64;
                        our_score / winning_score.max(1.0)
                    });
                },
            );
        }
    } else {
        for (idx, (auction, winning_score_str)) in historical.iter().enumerate() {
            let winning_score = parse_score(winning_score_str);
            let n_orders = auction.orders.len();

            group.bench_with_input(
                BenchmarkId::new(
                    format!("auction_{:04}_n{}", idx + 1, n_orders),
                    idx,
                ),
                &idx,
                |b, &i| {
                    b.iter(|| {
                        let auction = historical[i].0.clone();
                        let response = rt.block_on(async { solve(auction).await });
                        let our_score = solution_score(&response);
                        // Score ratio: > 1.0 means we beat the historical winner
                        our_score / winning_score.max(1.0)
                    });
                },
            );
        }
        eprintln!(
            "[route_quality] Loaded {} historical auctions. Average winning score: {:.2}",
            historical.len(),
            historical
                .iter()
                .map(|(_, s)| parse_score(s))
                .sum::<f64>()
                / historical.len() as f64
        );
    }

    group.finish();
}

criterion_group!(benches, bench_route_quality);
criterion_main!(benches);
