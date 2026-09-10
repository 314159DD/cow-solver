//! bench_solve_throughput — measures how many auction solve calls per second
//! the solver engine can handle.
//!
//! Results saved to benchmarks/results/ via Criterion's HTML output and
//! the custom CSV sink in benchmark.sh.
//!
//! Run: cargo bench -p benchmarks --bench solve_throughput

use benchmarks::{make_auction, make_cow_auction};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use solver_engine::solver::solve;

fn bench_solve_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("solve_throughput");

    // Vary auction size (number of orders) to show how throughput scales.
    for n_orders in [1usize, 5, 10, 25, 50, 100] {
        group.throughput(Throughput::Elements(n_orders as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(n_orders),
            &n_orders,
            |b, &n| {
                b.iter(|| {
                    let auction = make_auction(n);
                    rt.block_on(async { solve(auction).await })
                });
            },
        );
    }
    group.finish();

    // CoW matching benchmark: two opposite orders (no DEX needed)
    let mut cow_group = c.benchmark_group("solve_throughput_cow");
    cow_group.throughput(Throughput::Elements(2));
    cow_group.bench_function("cow_pair", |b| {
        b.iter(|| {
            let auction = make_cow_auction();
            rt.block_on(async { solve(auction).await })
        });
    });
    cow_group.finish();
}

criterion_group!(benches, bench_solve_throughput);
criterion_main!(benches);
