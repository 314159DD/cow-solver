#!/usr/bin/env bash
# scripts/shadow_test.sh
#
# CoW Protocol Shadow Competition Testing
#
# Connects the solver to the CoW Protocol shadow/barn competition — a test
# environment where solutions are scored but NOT executed on-chain. Used to
# validate solution quality and competitiveness before production.
#
# ─── Prerequisites ──────────────────────────────────────────────────────────
# 1. Shadow competition access — contact the CoW Solvers Telegram:
#    https://t.me/cowprotocolsolver
#    Tell them your solver name and the public URL below. They'll register you.
#
# 2. Public HTTPS endpoint — the driver must be reachable from CoW servers:
#    Option A: ngrok      →  ngrok http 8000
#    Option B: VPS/server →  point solver at public IP, open port
#    Set PUBLIC_URL below or export it before running.
#
# 3. .env with RPC_URL (Alchemy/Infura Mainnet or Arbitrum endpoint)
#
# ─── Quick Start ────────────────────────────────────────────────────────────
# # Terminal 1: Start ngrok
# ngrok http 8000
#
# # Terminal 2: Run shadow test
# export PUBLIC_URL=https://xxxx.ngrok-free.app
# export RPC_URL=https://eth-mainnet.g.alchemy.com/v2/your-key
# bash scripts/shadow_test.sh
#
# ─── Tracked Metrics ────────────────────────────────────────────────────────
# auctions_received     - Number of /solve calls received
# solutions_submitted   - Non-empty solution responses
# solutions_valid       - Solutions that passed CoW validation
# solutions_winning     - Solutions that won the auction
# avg_score             - Our average solution score
# avg_winning_score     - Winning solution average score
# score_gap_pct         - How far behind the winner we are (%)
#
# ─── Environment Variables ──────────────────────────────────────────────────
# PUBLIC_URL            - Public HTTPS URL for your solver (required for registration)
# SOLVER_PORT           - Local solver port (default: 8000)
# CHAIN_ID              - 1=Mainnet, 42161=Arbitrum (default: 1)
# COMPETITION_ENV       - "shadow" or "barn" (default: shadow)
# METRICS_INTERVAL_SEC  - How often to print metrics (default: 3600 = 1hr)
# LOG_FILE              - Where to write structured logs (default: /tmp/shadow-metrics.log)
# SOLVER_LOG_FILE       - Where the solver writes logs (default: /tmp/cow-solver.log)

set -euo pipefail

# ─── Config ─────────────────────────────────────────────────────────────────
SOLVER_PORT="${SOLVER_PORT:-8000}"
SOLVER_URL="http://localhost:${SOLVER_PORT}"
CHAIN_ID="${CHAIN_ID:-1}"
COMPETITION_ENV="${COMPETITION_ENV:-shadow}"
METRICS_INTERVAL_SEC="${METRICS_INTERVAL_SEC:-3600}"
LOG_FILE="${LOG_FILE:-/tmp/shadow-metrics.log}"
SOLVER_LOG_FILE="${SOLVER_LOG_FILE:-/tmp/cow-solver.log}"
PUBLIC_URL="${PUBLIC_URL:-}"
SOLVER_PID=""

# CoW Protocol competition endpoints
if [[ "$COMPETITION_ENV" == "barn" ]]; then
    COW_API_BASE="https://barn.api.cow.fi"
    COMPETITION_NAME="Barn (staging)"
else
    COW_API_BASE="https://api.cow.fi"
    COMPETITION_NAME="Shadow (test)"
fi

# Chain-specific settlement contract
if [[ "$CHAIN_ID" == "42161" ]]; then
    SETTLEMENT_CONTRACT="0x9008D19f58AAbD9eD0D60971565AA8510560ab41"
    CHAIN_NAME="Arbitrum"
    NETWORK_PATH="arbitrum_one"
else
    SETTLEMENT_CONTRACT="0x9008D19f58AAbD9eD0D60971565AA8510560ab41"
    CHAIN_NAME="Mainnet"
    NETWORK_PATH="mainnet"
fi

# ─── Colours ────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}[INFO]${NC}    $*"; }
log_ok()      { echo -e "${GREEN}[OK]${NC}      $*"; }
log_warn()    { echo -e "${YELLOW}[WARN]${NC}    $*"; }
log_error()   { echo -e "${RED}[ERROR]${NC}   $*"; }
log_metric()  { echo -e "${CYAN}[METRIC]${NC}  $*"; }
log_header()  { echo -e "\n${BOLD}$*${NC}"; }

# ─── Metric Counters (in-process tracking) ──────────────────────────────────
AUCTIONS_RECEIVED=0
SOLUTIONS_SUBMITTED=0
SOLUTIONS_EMPTY=0
TOTAL_SCORE=0
SCORE_SAMPLES=0
SESSION_START=$(date -u +%s)

# ─── Cleanup ────────────────────────────────────────────────────────────────
cleanup() {
    if [[ -n "$SOLVER_PID" ]] && kill -0 "$SOLVER_PID" 2>/dev/null; then
        log_info "Stopping solver (PID $SOLVER_PID)..."
        kill "$SOLVER_PID" 2>/dev/null || true
        wait "$SOLVER_PID" 2>/dev/null || true
        log_ok "Solver stopped cleanly."
    fi
    print_final_summary
}
trap cleanup EXIT INT TERM

# ─── Prereqs ────────────────────────────────────────────────────────────────
check_prereqs() {
    local missing=()
    command -v curl  >/dev/null 2>&1 || missing+=("curl")
    command -v cargo >/dev/null 2>&1 || missing+=("cargo")
    command -v jq    >/dev/null 2>&1 || missing+=("jq")

    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "Missing required tools: ${missing[*]}"
        log_warn "Install with: apt-get install -y curl jq"
        exit 1
    fi

    if [[ -z "${RPC_URL:-}" ]]; then
        log_error "RPC_URL is not set. Export it before running:"
        log_error "  export RPC_URL=https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
        exit 1
    fi
}

# ─── Public URL Validation ──────────────────────────────────────────────────
check_public_url() {
    log_header "── Public Endpoint Check ──────────────────────────────────────"

    if [[ -z "$PUBLIC_URL" ]]; then
        log_warn "PUBLIC_URL is not set."
        log_warn ""
        log_warn "For shadow competition, your solver needs a public HTTPS endpoint."
        log_warn "Options:"
        log_warn "  1. ngrok:   ngrok http ${SOLVER_PORT}"
        log_warn "              Then: export PUBLIC_URL=https://xxxx.ngrok-free.app"
        log_warn "  2. VPS/server: Deploy solver to a VPS and set PUBLIC_URL=https://your-vps-ip:8000"
        log_warn ""
        log_warn "Running in LOCAL-ONLY mode (no shadow competition connectivity)."
        return 1
    fi

    # Strip trailing slash
    PUBLIC_URL="${PUBLIC_URL%/}"

    # Verify our local solver is reachable
    if ! curl -sf "${SOLVER_URL}/health" >/dev/null 2>&1; then
        log_error "Local solver not responding at ${SOLVER_URL}/health"
        return 1
    fi

    log_ok "Public URL configured: ${PUBLIC_URL}"
    log_info "Registration URL for CoW team: ${PUBLIC_URL}/solve"
    log_info "Telegram: https://t.me/cowprotocolsolver"
    log_info "Tell them: solver name + ${PUBLIC_URL}/solve + chain: ${CHAIN_NAME}"
    return 0
}

# ─── Verify CoW API Connectivity ────────────────────────────────────────────
check_cow_api() {
    log_header "── CoW Protocol API Connectivity ──────────────────────────────"

    local endpoint="${COW_API_BASE}/${NETWORK_PATH}/api/v1/version"
    log_info "Checking CoW API: ${endpoint}"

    local response
    if response=$(curl -sf --max-time 10 "$endpoint" 2>/dev/null); then
        local version
        version=$(echo "$response" | jq -r '.version // "unknown"')
        log_ok "CoW API reachable — version: ${version}"
        log_info "Environment: ${COMPETITION_NAME}"
        return 0
    else
        log_warn "CoW API unreachable at ${endpoint}"
        log_warn "This is expected if you are not yet registered for shadow competition."
        return 1
    fi
}

# ─── Build Solver ───────────────────────────────────────────────────────────
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

    export SOLVER_PORT
    export RPC_URL="${RPC_URL}"
    export CHAIN_ID="${CHAIN_ID}"
    export LOG_LEVEL="${LOG_LEVEL:-info}"

    cargo run --release -p solver-engine > "${SOLVER_LOG_FILE}" 2>&1 &
    SOLVER_PID=$!

    local attempts=0
    while [[ $attempts -lt 30 ]]; do
        if curl -sf "${SOLVER_URL}/health" >/dev/null 2>&1; then
            log_ok "Solver is up (PID $SOLVER_PID)."
            return
        fi
        sleep 0.5
        (( attempts++ ))
    done

    log_error "Solver did not start within 15 seconds. Logs:"
    cat "${SOLVER_LOG_FILE}"
    exit 1
}

# ─── Parse Metrics from Solver Logs ─────────────────────────────────────────
parse_log_metrics() {
    local log_file="${1:-$SOLVER_LOG_FILE}"
    local since_ts="${2:-0}"

    if [[ ! -f "$log_file" ]]; then
        return
    fi

    # Count auctions received (JSON log lines with "auction" in context)
    local received
    received=$(grep -c '"solve"' "$log_file" 2>/dev/null || echo "0")

    # Count non-empty solutions
    local submitted
    submitted=$(grep -c '"solutions_count":[^0]' "$log_file" 2>/dev/null || echo "0")

    # Extract score data from logs if structured JSON logging is used
    local scores
    scores=$(grep '"score"' "$log_file" 2>/dev/null | jq -r '.score // empty' 2>/dev/null || echo "")

    local total_score=0
    local score_count=0
    while IFS= read -r score; do
        [[ -z "$score" ]] && continue
        total_score=$(echo "$total_score + $score" | bc -l 2>/dev/null || echo "$total_score")
        (( score_count++ ))
    done <<< "$scores"

    echo "${received}:${submitted}:${total_score}:${score_count}"
}

# ─── Simulate a Single Auction (Validation Test) ────────────────────────────
test_sample_auction() {
    log_header "── Sample Auction Validation ──────────────────────────────────"

    local auction_file="${AUCTION_FILE:-data/sample_auction.json}"
    if [[ ! -f "$auction_file" ]]; then
        log_warn "Sample auction not found: ${auction_file} — skipping validation test."
        return
    fi

    log_info "Sending sample auction to solver..."
    local start_ms
    start_ms=$(date +%s%N 2>/dev/null || echo "0")

    local response
    response=$(curl -sf --max-time 30 -X POST "${SOLVER_URL}/solve" \
        -H "Content-Type: application/json" \
        -d "@${auction_file}" 2>/dev/null || echo '{"solutions":[]}')

    local end_ms
    end_ms=$(date +%s%N 2>/dev/null || echo "0")
    local duration_ms=$(( (end_ms - start_ms) / 1000000 ))

    local solutions_count
    solutions_count=$(echo "$response" | jq '.solutions | length' 2>/dev/null || echo "0")

    if [[ "$solutions_count" -gt 0 ]]; then
        log_ok "Solver returned ${solutions_count} solution(s) in ${duration_ms}ms."
        SOLUTIONS_SUBMITTED=$(( SOLUTIONS_SUBMITTED + 1 ))

        # Extract scores for analysis
        local scores
        scores=$(echo "$response" | jq -r '.solutions[].score // empty' 2>/dev/null || echo "")
        while IFS= read -r score; do
            [[ -z "$score" ]] && continue
            log_metric "Solution score: ${score}"
        done <<< "$scores"
    else
        log_warn "Solver returned empty solution in ${duration_ms}ms."
        log_warn "This is valid — solver found no profitable opportunity."
        SOLUTIONS_EMPTY=$(( SOLUTIONS_EMPTY + 1 ))
    fi

    AUCTIONS_RECEIVED=$(( AUCTIONS_RECEIVED + 1 ))
    echo "$response" | jq . 2>/dev/null || echo "$response"
}

# ─── Fetch Recent Competition Results ───────────────────────────────────────
fetch_competition_results() {
    log_header "── Recent Competition Results ─────────────────────────────────"

    local endpoint="${COW_API_BASE}/${NETWORK_PATH}/api/v1/solver_competition/latest"
    log_info "Fetching latest competition data..."

    local response
    if response=$(curl -sf --max-time 10 "$endpoint" 2>/dev/null); then
        local auction_id tx winner winner_score
        auction_id=$(echo "$response" | jq -r '.auctionId // "unknown"')
        tx=$(echo "$response" | jq -r '.transactionHash // "pending"')
        winner=$(echo "$response" | jq -r '.solutions[0].solver // "unknown"' 2>/dev/null)
        winner_score=$(echo "$response" | jq -r '.solutions[0].score // "0"' 2>/dev/null)

        log_metric "Latest auction: #${auction_id}"
        log_metric "Winner: ${winner}"
        log_metric "Winning score: ${winner_score}"
        if [[ "$tx" != "pending" ]]; then
            log_metric "Tx: https://etherscan.io/tx/${tx}"
        fi
    else
        log_warn "Could not fetch competition results (may not be registered yet)."
    fi
}

# ─── Hourly Metrics Report ───────────────────────────────────────────────────
print_metrics() {
    local now
    now=$(date -u +%s)
    local elapsed=$(( now - SESSION_START ))
    local elapsed_min=$(( elapsed / 60 ))
    local elapsed_hr=$(( elapsed / 3600 ))

    log_header "═══════════════════════════════════════════════════════"
    log_header "  CoW Shadow Competition Metrics — $(date -u '+%Y-%m-%d %H:%M UTC')"
    log_header "═══════════════════════════════════════════════════════"

    log_metric "Environment:           ${COMPETITION_NAME}"
    log_metric "Chain:                 ${CHAIN_NAME} (ID: ${CHAIN_ID})"
    log_metric "Session duration:      ${elapsed_hr}h ${elapsed_min}m"
    log_metric ""
    log_metric "Auctions received:     ${AUCTIONS_RECEIVED}"
    log_metric "Solutions submitted:   ${SOLUTIONS_SUBMITTED}"
    log_metric "Empty solutions:       ${SOLUTIONS_EMPTY}"

    if [[ $AUCTIONS_RECEIVED -gt 0 ]]; then
        local submit_rate
        submit_rate=$(echo "scale=1; ${SOLUTIONS_SUBMITTED} * 100 / ${AUCTIONS_RECEIVED}" | bc -l 2>/dev/null || echo "N/A")
        log_metric "Submission rate:       ${submit_rate}%"
    fi

    if [[ $SCORE_SAMPLES -gt 0 ]]; then
        local avg_score
        avg_score=$(echo "scale=2; ${TOTAL_SCORE} / ${SCORE_SAMPLES}" | bc -l 2>/dev/null || echo "N/A")
        log_metric "Avg score:             ${avg_score}"
    else
        log_metric "Avg score:             N/A (need live competition data)"
    fi

    log_header "═══════════════════════════════════════════════════════"

    # Write to log file
    {
        echo "---"
        echo "timestamp: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo "auctions_received: ${AUCTIONS_RECEIVED}"
        echo "solutions_submitted: ${SOLUTIONS_SUBMITTED}"
        echo "solutions_empty: ${SOLUTIONS_EMPTY}"
        echo "session_elapsed_sec: ${elapsed}"
    } >> "${LOG_FILE}"
}

# ─── Score Gap Analysis ──────────────────────────────────────────────────────
score_gap_analysis() {
    log_header "── Score Gap Analysis ─────────────────────────────────────────"
    log_info "Comparing our scores against competition winners..."

    # Fetch last 10 competition results
    local endpoint="${COW_API_BASE}/${NETWORK_PATH}/api/v1/solver_competition"
    local response
    if ! response=$(curl -sf --max-time 15 "${endpoint}?limit=10" 2>/dev/null); then
        log_warn "Cannot fetch competition history — not yet registered or API unavailable."
        log_warn ""
        log_warn "Once registered, this section will show:"
        log_warn "  - Our score vs winning score per auction"
        log_warn "  - % gap to winner"
        log_warn "  - Which order types we are losing on"
        return
    fi

    local auction_count
    auction_count=$(echo "$response" | jq '. | length' 2>/dev/null || echo "0")
    log_info "Analyzing last ${auction_count} auctions..."

    # For each auction, find our solver and compare to winner
    local total_gap=0
    local gap_count=0

    while IFS= read -r auction; do
        local auction_id winner_score our_score
        auction_id=$(echo "$auction" | jq -r '.auctionId // ""')
        winner_score=$(echo "$auction" | jq -r '.solutions[0].score // "0"')
        # Look for our solver address in the results
        our_score=$(echo "$auction" | jq -r --arg solver "${PUBLIC_URL:-}" \
            '.solutions[] | select(.solverAddress == $solver) | .score // "0"' 2>/dev/null || echo "0")

        if [[ -n "$auction_id" && "$winner_score" != "0" ]]; then
            if [[ -n "$our_score" && "$our_score" != "0" ]]; then
                local gap
                gap=$(echo "scale=1; (${winner_score} - ${our_score}) * 100 / ${winner_score}" | bc -l 2>/dev/null || echo "N/A")
                log_metric "Auction #${auction_id}: our=${our_score} winner=${winner_score} gap=${gap}%"
                total_gap=$(echo "$total_gap + ${gap}" | bc -l 2>/dev/null || echo "$total_gap")
                (( gap_count++ ))
            else
                log_metric "Auction #${auction_id}: winning_score=${winner_score} (we did not compete)"
            fi
        fi
    done < <(echo "$response" | jq -c '.[]' 2>/dev/null)

    if [[ $gap_count -gt 0 ]]; then
        local avg_gap
        avg_gap=$(echo "scale=1; ${total_gap} / ${gap_count}" | bc -l 2>/dev/null || echo "N/A")
        log_metric "Average score gap to winner: ${avg_gap}%"
        log_metric "Competed in ${gap_count} / ${auction_count} auctions"
    fi
}

# ─── Final Summary ───────────────────────────────────────────────────────────
print_final_summary() {
    echo ""
    log_header "╔══════════════════════════════════════════╗"
    log_header "║   Shadow Competition — Session Summary   ║"
    log_header "╚══════════════════════════════════════════╝"
    print_metrics
    echo ""
    log_info "Full metrics log: ${LOG_FILE}"
    log_info "Solver logs:      ${SOLVER_LOG_FILE}"
    echo ""
}

# ─── Metrics Monitoring Loop ─────────────────────────────────────────────────
run_metrics_loop() {
    log_info "Starting metrics loop (report every ${METRICS_INTERVAL_SEC}s)."
    log_info "Press Ctrl+C to stop and see final summary."
    log_info ""

    local next_report=$(( $(date +%s) + METRICS_INTERVAL_SEC ))

    while true; do
        sleep 10

        # Update counters from solver logs
        local log_data
        log_data=$(parse_log_metrics "$SOLVER_LOG_FILE")
        AUCTIONS_RECEIVED=$(echo "$log_data" | cut -d: -f1)
        SOLUTIONS_SUBMITTED=$(echo "$log_data" | cut -d: -f2)
        TOTAL_SCORE=$(echo "$log_data" | cut -d: -f3)
        SCORE_SAMPLES=$(echo "$log_data" | cut -d: -f4)

        # Check if solver is still running
        if [[ -n "$SOLVER_PID" ]] && ! kill -0 "$SOLVER_PID" 2>/dev/null; then
            log_error "Solver process died! PID $SOLVER_PID"
            log_info "Restarting solver..."
            start_solver
        fi

        # Hourly report
        local now
        now=$(date +%s)
        if [[ $now -ge $next_report ]]; then
            print_metrics
            next_report=$(( now + METRICS_INTERVAL_SEC ))
        fi
    done
}

# ─── Setup: Registration Checklist ──────────────────────────────────────────
print_registration_checklist() {
    log_header "── Shadow Competition Registration Checklist ──────────────────"
    echo ""
    echo "  To join the CoW Protocol shadow competition:"
    echo ""
    echo "  [ ] 1. Join CoW Solvers Telegram: https://t.me/cowprotocolsolver"
    echo "  [ ] 2. Set up a public HTTPS endpoint:"
    echo "         ngrok: ngrok http ${SOLVER_PORT}"
    echo "         VPS:   deploy solver to public IP"
    echo "  [ ] 3. Message the team with:"
    echo "         - Solver name (unique identifier)"
    echo "         - Your /solve endpoint URL"
    echo "         - Chain (mainnet or Arbitrum)"
    echo "  [ ] 4. Set PUBLIC_URL=<your-ngrok-or-vps-url>"
    echo "  [ ] 5. Wait for confirmation from the CoW team"
    echo "  [ ] 6. Observe incoming auctions in solver logs"
    echo ""
    echo "  Once registered, auctions will arrive every ~30 seconds."
    echo "  Metrics are tracked in: ${LOG_FILE}"
    echo ""
}

# ─── Main ───────────────────────────────────────────────────────────────────
main() {
    echo ""
    echo "╔══════════════════════════════════════════════════╗"
    echo "║  CoW Protocol Solver — Shadow Competition Test   ║"
    echo "╚══════════════════════════════════════════════════╝"
    echo ""

    # Load .env if present
    if [[ -f ".env" ]]; then
        set -a
        # shellcheck source=/dev/null
        source .env
        set +a
        log_info "Loaded .env"
    fi

    check_prereqs
    print_registration_checklist

    build_solver
    start_solver

    # Validate solver with local sample
    test_sample_auction

    # Check public URL and CoW API
    local has_public_url=false
    if check_public_url; then
        has_public_url=true
    fi
    check_cow_api || true

    # If registered (PUBLIC_URL set), fetch competition context
    if [[ "$has_public_url" == "true" ]]; then
        fetch_competition_results || true
        score_gap_analysis || true
    fi

    # Initial metrics report
    print_metrics

    # Enter monitoring loop — runs until Ctrl+C
    if [[ "${MONITOR:-1}" == "1" ]]; then
        run_metrics_loop
    else
        log_info "MONITOR=0 — exiting after initial validation."
    fi
}

main "$@"
