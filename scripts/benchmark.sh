#!/usr/bin/env bash
# benchmark.sh — run the full CoW solver benchmark suite and save results.
#
# Usage:
#   bash scripts/benchmark.sh              # run all benchmarks
#   bash scripts/benchmark.sh throughput   # run only solve_throughput
#   bash scripts/benchmark.sh quality      # run only route_quality
#   bash scripts/benchmark.sh pools        # run only pool_sync
#
# Output:
#   benchmarks/results/YYYY-MM-DD_HH-MM-SS/  — Criterion HTML + JSON reports
#   benchmarks/results/latest/               — symlink to the most recent run
#
# Prerequisites: cargo, Rust toolchain in PATH

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
RESULTS_DIR="${WORKSPACE_DIR}/benchmarks/results"
TIMESTAMP="$(date +%Y-%m-%d_%H-%M-%S)"
RUN_DIR="${RESULTS_DIR}/${TIMESTAMP}"

mkdir -p "${RUN_DIR}"

echo "============================================================"
echo " CoW Protocol Solver — Benchmark Suite"
echo " Workspace: ${WORKSPACE_DIR}"
echo " Results:   ${RUN_DIR}"
echo "============================================================"
echo ""

cd "${WORKSPACE_DIR}"

# ─── Build first (fail fast) ─────────────────────────────────────────────────
echo "[1/4] Building benchmarks (release)..."
cargo build -p benchmarks --release 2>&1 | tail -5
echo ""

# ─── Select which benches to run ─────────────────────────────────────────────
TARGET="${1:-all}"

run_bench() {
    local name="$1"
    echo "[bench] Running ${name}..."
    cargo bench -p benchmarks --bench "${name}" -- \
        --output-format bencher 2>&1 \
        | tee "${RUN_DIR}/${name}.txt"
    echo ""
}

case "${TARGET}" in
    throughput)
        run_bench solve_throughput
        ;;
    quality)
        run_bench route_quality
        ;;
    pools)
        run_bench pool_sync
        ;;
    all)
        run_bench solve_throughput
        run_bench route_quality
        run_bench pool_sync
        ;;
    *)
        echo "Unknown target: ${TARGET}. Use: all | throughput | quality | pools"
        exit 1
        ;;
esac

# ─── Copy Criterion HTML reports ─────────────────────────────────────────────
CRITERION_DIR="${WORKSPACE_DIR}/target/criterion"
if [ -d "${CRITERION_DIR}" ]; then
    echo "[3/4] Copying Criterion HTML reports..."
    cp -r "${CRITERION_DIR}" "${RUN_DIR}/criterion_reports" 2>/dev/null || true
fi

# ─── Extract key numbers into a summary CSV ──────────────────────────────────
SUMMARY_CSV="${RUN_DIR}/summary.csv"
echo "benchmark,metric,value,unit" > "${SUMMARY_CSV}"

for txt_file in "${RUN_DIR}"/*.txt; do
    bench_name="$(basename "${txt_file}" .txt)"
    # Criterion bencher format: "test <name> ... bench:   <N> ns/iter (+/- <M>)"
    while IFS= read -r line; do
        if [[ "${line}" =~ ^test[[:space:]](.+)[[:space:]]+bench:[[:space:]]+([0-9,]+)[[:space:]]ns/iter ]]; then
            test_name="${BASH_REMATCH[1]// /_}"
            ns_per_iter="${BASH_REMATCH[2]//,/}"
            echo "${bench_name},${test_name},${ns_per_iter},ns_per_iter" >> "${SUMMARY_CSV}"
        fi
    done < "${txt_file}"
done

echo "[4/4] Summary written to ${SUMMARY_CSV}"
cat "${SUMMARY_CSV}"
echo ""

# ─── Update 'latest' symlink ─────────────────────────────────────────────────
LATEST_LINK="${RESULTS_DIR}/latest"
rm -f "${LATEST_LINK}" 2>/dev/null || true
# Use relative path for portability
ln -sf "${TIMESTAMP}" "${LATEST_LINK}" 2>/dev/null || \
    echo "(Note: symlink creation failed — this is OK on Windows without elevated permissions)"

echo "============================================================"
echo " Done! Results: ${RUN_DIR}"
echo "============================================================"
