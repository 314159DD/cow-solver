//! bench_pool_sync - measures pool registry sync/query latency.
//!
//! Two sub-benchmarks:
//!   1. `registry_build`  - time to construct a PoolRegistry and insert N pools
//!   2. `pair_query`      - time to call `pools_for_pair()` on a warm registry
//!
//! These model the two hot paths for pool data:
//!   - Cold start: reading all pools from RPC into the registry
//!   - Hot path:   querying the registry on every auction
//!
//! Run: cargo bench -p benchmarks --bench pool_sync

use benchmarks::{DAI, USDC, WETH, make_registry, make_v2_pool};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use solver_engine::liquidity::registry::PoolRegistry;

fn bench_registry_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool_sync_build");

    for n_pools in [10usize, 100, 500, 1_000] {
        group.throughput(Throughput::Elements(n_pools as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(n_pools),
            &n_pools,
            |b, &n| {
                b.iter(|| {
                    let mut reg = PoolRegistry::new();
                    for i in 0..n {
                        let r0 = 1_000_000_000_000_000_000u128 * (i as u128 + 1);
                        let r1 = 3_700_000_000u128 * (i as u128 + 1);
                        reg.add_pool(make_v2_pool(WETH, USDC, r0, r1));
                    }
                    reg
                });
            },
        );
    }
    group.finish();
}

fn bench_pair_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool_sync_query");

    for n_pools in [10usize, 100, 500, 1_000] {
        // Build registry once outside the hot loop
        let registry = make_registry(n_pools);

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(n_pools),
            &n_pools,
            |b, _| {
                b.iter(|| {
                    // Query the most common pair (WETH/USDC) - best case
                    let _ = registry.pools_for_pair(WETH, USDC);
                    // Query a rarer pair (USDC/DAI) - exercises full scan
                    let _ = registry.pools_for_pair(USDC, DAI);
                    // Query a pair with no pools
                    let _ = registry.pools_for_pair(WETH, "0xdeadbeef");
                });
            },
        );
    }
    group.finish();
}

fn bench_all_pools_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool_sync_iter");

    for n_pools in [100usize, 1_000] {
        let registry = make_registry(n_pools);

        group.throughput(Throughput::Elements(registry.all_pools().len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(n_pools),
            &n_pools,
            |b, _| {
                b.iter(|| {
                    // Simulate scanning all pools to find best price (hot path during solving)
                    registry
                        .all_pools()
                        .iter()
                        .filter(|p| {
                            let addr = p.address();
                            !addr.is_empty()
                        })
                        .count()
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_registry_build, bench_pair_query, bench_all_pools_iter);
criterion_main!(benches);
