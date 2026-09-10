#!/usr/bin/env bash
# scripts/local_test.sh
#
# Local end-to-end test for the CoW Protocol Solver.
# Starts the solver, verifies health, sends a sample auction, and cleans up.
#
# Usage:
#   bash scripts/local_test.sh
#
# Optional env vars:
#   SOLVER_PORT  - Port to use (default: 8000)
#   RPC_URL      - Ethereum RPC endpoint (required for full run; omit to skip RPC tests)
#   SKIP_BUILD   - Set to 1 to skip cargo build step (if already built)

set -euo pipefail

SOLVER_PORT="${SOLVER_PORT:-8000}"
SOLVER_URL="http://localhost:${SOLVER_PORT}"
SOLVER_PID=""

# ─── Colours ────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Colour

log_info()    { echo -e "${BLUE}[INFO]${NC}  $*"; }
log_ok()      { echo -e "${GREEN}[OK]${NC}    $*"; }
log_warn()    { echo -e "${YELLOW}[WARN]${NC}  $*"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $*"; }

# ─── Cleanup ────────────────────────────────────────────────────────────────
cleanup() {
    if [[ -n "$SOLVER_PID" ]] && kill -0 "$SOLVER_PID" 2>/dev/null; then
        log_info "Stopping solver (PID $SOLVER_PID)..."
        kill "$SOLVER_PID" 2>/dev/null || true
        wait "$SOLVER_PID" 2>/dev/null || true
        log_ok "Solver stopped cleanly."
    fi
}
trap cleanup EXIT INT TERM

# ─── Prereqs ────────────────────────────────────────────────────────────────
check_prereqs() {
    local missing=()
    command -v curl  >/dev/null 2>&1 || missing+=("curl")
    command -v cargo >/dev/null 2>&1 || missing+=("cargo")

    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "Missing required tools: ${missing[*]}"
        exit 1
    fi

    if command -v jq >/dev/null 2>&1; then
        JQ="jq ."
    else
        log_warn "jq not found — raw JSON will be printed."
        JQ="cat"
    fi
}

# ─── Build ──────────────────────────────────────────────────────────────────
build_solver() {
    if [[ "${SKIP_BUILD:-0}" == "1" ]]; then
        log_info "Skipping build (SKIP_BUILD=1)."
        return
    fi
    log_info "Building solver-engine (release)..."
    cargo build --release -p solver-engine 2>&1
    log_ok "Build succeeded."
}

# ─── Start Solver ───────────────────────────────────────────────────────────
start_solver() {
    log_info "Starting solver on port ${SOLVER_PORT}..."

    # Export env so the solver picks them up
    export SOLVER_PORT
    # RPC_URL may be unset — solver will handle it (may log a warning)
    export RPC_URL="${RPC_URL:-}"

    cargo run --release -p solver-engine > /tmp/cow-solver.log 2>&1 &
    SOLVER_PID=$!

    # Wait up to 10 seconds for the server to become healthy
    local attempts=0
    local max_attempts=20
    while [[ $attempts -lt $max_attempts ]]; do
        if curl -sf "${SOLVER_URL}/health" >/dev/null 2>&1; then
            log_ok "Solver is up (PID $SOLVER_PID)."
            return
        fi
        sleep 0.5
        (( attempts++ ))
    done

    log_error "Solver did not start within 10 seconds. Logs:"
    cat /tmp/cow-solver.log
    exit 1
}

# ─── Health Check ───────────────────────────────────────────────────────────
test_health() {
    log_info "Testing GET /health..."
    local response
    response=$(curl -sf "${SOLVER_URL}/health")
    echo "$response" | $JQ
    log_ok "Health check passed."
}

# ─── Solve Endpoint ─────────────────────────────────────────────────────────
test_solve() {
    local auction_file="${1:-data/sample_auction.json}"

    if [[ ! -f "$auction_file" ]]; then
        log_error "Sample auction file not found: $auction_file"
        exit 1
    fi

    log_info "Testing POST /solve with ${auction_file}..."
    local response
    response=$(curl -sf -X POST "${SOLVER_URL}/solve" \
        -H "Content-Type: application/json" \
        -d "@${auction_file}")

    echo "$response" | $JQ

    # Validate response has "solutions" key
    if command -v jq >/dev/null 2>&1; then
        local solutions_count
        solutions_count=$(echo "$response" | jq '.solutions | length')
        log_ok "/solve returned ${solutions_count} solution(s) (empty is fine for Sprint 1)."
    else
        log_ok "/solve returned a response."
    fi
}

# ─── Metrics Endpoint (optional) ────────────────────────────────────────────
test_metrics() {
    log_info "Testing GET /metrics (if implemented)..."
    if curl -sf "${SOLVER_URL}/metrics" 2>/dev/null | $JQ; then
        log_ok "/metrics returned data."
    else
        log_warn "/metrics not yet implemented — skipping."
    fi
}

# ─── CoW Driver (optional, requires services repo) ──────────────────────────
run_with_driver() {
    local driver_bin
    driver_bin="${COW_SERVICES_DIR:-}/target/release/driver"

    if [[ ! -x "$driver_bin" ]]; then
        log_warn "CoW driver binary not found at ${driver_bin}."
        log_warn "To run with the full driver, clone https://github.com/cowprotocol/services"
        log_warn "and set COW_SERVICES_DIR=/path/to/services."
        log_warn "Then: cargo build --release -p driver"
        return
    fi

    if [[ -z "${RPC_URL:-}" ]]; then
        log_warn "RPC_URL not set — skipping driver test."
        return
    fi

    log_info "Running with CoW driver..."
    "$driver_bin" \
        --config scripts/driver.config.toml \
        --ethrpc "$RPC_URL" \
        --log-level info &
    local driver_pid=$!
    sleep 5
    kill "$driver_pid" 2>/dev/null || true
    log_ok "Driver ran for 5 seconds — check logs above."
}

# ─── Main ───────────────────────────────────────────────────────────────────
main() {
    echo ""
    echo "╔══════════════════════════════════════════╗"
    echo "║   CoW Protocol Solver — Local Test Run   ║"
    echo "╚══════════════════════════════════════════╝"
    echo ""

    check_prereqs
    build_solver
    start_solver
    test_health
    test_solve "data/sample_auction.json"
    test_metrics
    run_with_driver

    echo ""
    log_ok "All tests passed! ✓"
    echo ""
}

main "$@"
