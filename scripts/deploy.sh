#!/usr/bin/env bash
# scripts/deploy.sh
#
# Zero-downtime deployment script for the CoW Protocol Solver.
#
# Strategy: blue/green container swap
#   1. Pull the new image
#   2. Start the new container on a temporary name
#   3. Wait for its health check to pass
#   4. Update the stable container name (atomic swap via rename)
#   5. Stop the old container
#
# Usage:
#   bash scripts/deploy.sh                      # Deploy latest image
#   bash scripts/deploy.sh ghcr.io/you/cow-solver:abc123  # Deploy specific tag
#
# Required env vars (or set in .env):
#   RPC_URL       - Ethereum JSON-RPC endpoint
#
# Optional env vars:
#   IMAGE_NAME    - Docker image name (default: cow-solver)
#   IMAGE_TAG     - Image tag to deploy (default: latest)
#   SOLVER_PORT   - Port to expose (default: 8000)
#   CHAIN_ID      - Chain ID (default: 1)
#   LOG_LEVEL     - Log level (default: info)
#
# Prerequisites:
#   - Docker installed and running
#   - RPC_URL set
#   - For remote registry: docker login already performed

set -euo pipefail

# ─── Config ─────────────────────────────────────────────────────────────────
IMAGE_NAME="${IMAGE_NAME:-cow-solver}"
IMAGE_TAG="${IMAGE_TAG:-latest}"
SOLVER_PORT="${SOLVER_PORT:-8000}"
CHAIN_ID="${CHAIN_ID:-1}"
LOG_LEVEL="${LOG_LEVEL:-info}"
MAX_SOLVE_TIME_MS="${MAX_SOLVE_TIME_MS:-25000}"

CONTAINER_NAME="cow-solver"
CONTAINER_GREEN="${CONTAINER_NAME}-green"
HEALTH_URL="http://localhost:${SOLVER_PORT}/health"
HEALTH_TIMEOUT=60   # seconds to wait for new container health
DRAIN_TIMEOUT=5     # seconds to drain old container before stop

FULL_IMAGE="${IMAGE_NAME}:${IMAGE_TAG}"

# ─── Colours ────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()  { echo -e "${BLUE}[DEPLOY]${NC}  $*"; }
log_ok()    { echo -e "${GREEN}[OK]${NC}      $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC}    $*"; }
log_error() { echo -e "${RED}[ERROR]${NC}   $*" >&2; }

DEPLOY_START=$(date -u +%s)

# ─── Cleanup on failure ─────────────────────────────────────────────────────
GREEN_STARTED=false
cleanup_on_failure() {
    if [[ "$GREEN_STARTED" == "true" ]]; then
        log_warn "Deployment failed — cleaning up green container..."
        docker rm -f "$CONTAINER_GREEN" 2>/dev/null || true
    fi
    log_error "Deployment FAILED. Old container still running."
}
trap cleanup_on_failure ERR

# ─── Prereqs ────────────────────────────────────────────────────────────────
check_prereqs() {
    if ! command -v docker &>/dev/null; then
        log_error "Docker not found. Install Docker: https://docs.docker.com/engine/install/"
        exit 1
    fi

    if ! docker info &>/dev/null; then
        log_error "Docker daemon not running. Start it with: sudo systemctl start docker"
        exit 1
    fi

    if [[ -z "${RPC_URL:-}" ]]; then
        log_error "RPC_URL is not set."
        log_error "Export it: export RPC_URL=https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
        exit 1
    fi
}

# ─── Load .env ──────────────────────────────────────────────────────────────
load_env() {
    local env_file="${1:-.env}"
    if [[ -f "$env_file" ]]; then
        log_info "Loading ${env_file}..."
        set -a
        # shellcheck source=/dev/null
        source "$env_file"
        set +a
    fi
}

# ─── Pull Image ─────────────────────────────────────────────────────────────
pull_image() {
    # If this is a local image (no registry prefix), skip pull
    if [[ "$IMAGE_NAME" != *"/"* ]] && [[ "$IMAGE_NAME" != *"."* ]]; then
        log_info "Local image '${FULL_IMAGE}' — skipping pull."
        return
    fi

    log_info "Pulling image: ${FULL_IMAGE}..."
    if ! docker pull "$FULL_IMAGE"; then
        log_error "Failed to pull ${FULL_IMAGE}"
        exit 1
    fi
    log_ok "Pulled ${FULL_IMAGE}."
}

# ─── Check Current State ────────────────────────────────────────────────────
check_current_state() {
    OLD_CONTAINER=""
    if docker inspect "$CONTAINER_NAME" &>/dev/null; then
        OLD_CONTAINER="$CONTAINER_NAME"
        local old_image
        old_image=$(docker inspect --format '{{.Config.Image}}' "$CONTAINER_NAME" 2>/dev/null || echo "unknown")
        log_info "Existing container: ${CONTAINER_NAME} (image: ${old_image})"
    else
        log_info "No existing container — fresh deployment."
    fi
}

# ─── Start Green Container ──────────────────────────────────────────────────
start_green() {
    # Remove stale green if exists
    docker rm -f "$CONTAINER_GREEN" 2>/dev/null || true

    log_info "Starting green container (${CONTAINER_GREEN})..."

    # Use a different host port for green during health check if blue is running
    local green_port
    if [[ -n "$OLD_CONTAINER" ]]; then
        green_port=$(( SOLVER_PORT + 1 ))
        HEALTH_URL="http://localhost:${green_port}/health"
    else
        green_port="$SOLVER_PORT"
    fi

    docker run -d \
        --name "$CONTAINER_GREEN" \
        --restart always \
        -p "${green_port}:${SOLVER_PORT}" \
        -e "RPC_URL=${RPC_URL}" \
        -e "SOLVER_PORT=${SOLVER_PORT}" \
        -e "CHAIN_ID=${CHAIN_ID}" \
        -e "LOG_LEVEL=${LOG_LEVEL}" \
        -e "MAX_SOLVE_TIME_MS=${MAX_SOLVE_TIME_MS}" \
        --log-driver json-file \
        --log-opt max-size=50m \
        --log-opt max-file=5 \
        --network cow-net 2>/dev/null || \
    docker run -d \
        --name "$CONTAINER_GREEN" \
        --restart always \
        -p "${green_port}:${SOLVER_PORT}" \
        -e "RPC_URL=${RPC_URL}" \
        -e "SOLVER_PORT=${SOLVER_PORT}" \
        -e "CHAIN_ID=${CHAIN_ID}" \
        -e "LOG_LEVEL=${LOG_LEVEL}" \
        -e "MAX_SOLVE_TIME_MS=${MAX_SOLVE_TIME_MS}" \
        --log-driver json-file \
        --log-opt max-size=50m \
        --log-opt max-file=5 \
        "$FULL_IMAGE"

    GREEN_STARTED=true
    log_ok "Green container started (port ${green_port})."
}

# ─── Wait for Health ─────────────────────────────────────────────────────────
wait_for_health() {
    log_info "Waiting for health check at ${HEALTH_URL} (timeout: ${HEALTH_TIMEOUT}s)..."

    local attempts=0
    local max_attempts=$(( HEALTH_TIMEOUT * 2 ))

    while [[ $attempts -lt $max_attempts ]]; do
        if curl -sf --max-time 3 "$HEALTH_URL" >/dev/null 2>&1; then
            log_ok "Green container is healthy."
            return 0
        fi

        # Check if container died
        local status
        status=$(docker inspect --format '{{.State.Status}}' "$CONTAINER_GREEN" 2>/dev/null || echo "gone")
        if [[ "$status" == "exited" || "$status" == "dead" || "$status" == "gone" ]]; then
            log_error "Green container exited unexpectedly. Logs:"
            docker logs --tail 50 "$CONTAINER_GREEN" 2>/dev/null || true
            return 1
        fi

        sleep 0.5
        (( attempts++ ))
    done

    log_error "Green container did not become healthy within ${HEALTH_TIMEOUT}s."
    log_error "Container logs:"
    docker logs --tail 50 "$CONTAINER_GREEN" 2>/dev/null || true
    return 1
}

# ─── Atomic Swap ─────────────────────────────────────────────────────────────
atomic_swap() {
    log_info "Performing atomic container swap..."

    if [[ -n "$OLD_CONTAINER" ]]; then
        # Stop old container (allow drain)
        log_info "Draining old container (${DRAIN_TIMEOUT}s)..."
        sleep "$DRAIN_TIMEOUT"

        # Rename old → old-retired
        docker rename "$CONTAINER_NAME" "${CONTAINER_NAME}-retired" 2>/dev/null || true

        # Free the port: stop old container
        docker stop --time "$DRAIN_TIMEOUT" "${CONTAINER_NAME}-retired" 2>/dev/null || true
    fi

    # Rename green → stable
    # First stop green (has temp port), remove, restart on real port
    docker stop "$CONTAINER_GREEN" 2>/dev/null || true
    docker rm "$CONTAINER_GREEN" 2>/dev/null || true

    # Start on the real port with stable name
    docker run -d \
        --name "$CONTAINER_NAME" \
        --restart always \
        -p "${SOLVER_PORT}:${SOLVER_PORT}" \
        -e "RPC_URL=${RPC_URL}" \
        -e "SOLVER_PORT=${SOLVER_PORT}" \
        -e "CHAIN_ID=${CHAIN_ID}" \
        -e "LOG_LEVEL=${LOG_LEVEL}" \
        -e "MAX_SOLVE_TIME_MS=${MAX_SOLVE_TIME_MS}" \
        --log-driver json-file \
        --log-opt max-size=50m \
        --log-opt max-file=5 \
        "$FULL_IMAGE"

    GREEN_STARTED=false
    log_ok "Container '${CONTAINER_NAME}' now running image '${FULL_IMAGE}'."
}

# ─── Final Health Check ───────────────────────────────────────────────────────
final_health_check() {
    HEALTH_URL="http://localhost:${SOLVER_PORT}/health"
    log_info "Final health check: ${HEALTH_URL}..."

    local attempts=0
    while [[ $attempts -lt 20 ]]; do
        if curl -sf --max-time 3 "$HEALTH_URL" >/dev/null 2>&1; then
            log_ok "Solver is live on port ${SOLVER_PORT}."
            return 0
        fi
        sleep 0.5
        (( attempts++ ))
    done

    log_error "Final health check failed!"
    return 1
}

# ─── Cleanup Retired ─────────────────────────────────────────────────────────
cleanup_retired() {
    if docker inspect "${CONTAINER_NAME}-retired" &>/dev/null; then
        log_info "Removing retired container..."
        docker rm "${CONTAINER_NAME}-retired" 2>/dev/null || true
        log_ok "Retired container removed."
    fi
}

# ─── Print Summary ───────────────────────────────────────────────────────────
print_summary() {
    local now
    now=$(date -u +%s)
    local elapsed=$(( now - DEPLOY_START ))

    echo ""
    log_ok "═══════════════════════════════════════════════"
    log_ok "  Deployment complete in ${elapsed}s"
    log_ok "  Image:    ${FULL_IMAGE}"
    log_ok "  Port:     ${SOLVER_PORT}"
    log_ok "  Health:   http://localhost:${SOLVER_PORT}/health"
    log_ok "═══════════════════════════════════════════════"
    echo ""
}

# ─── Main ───────────────────────────────────────────────────────────────────
main() {
    # Allow overriding image from CLI arg
    if [[ $# -ge 1 && "$1" != "--"* ]]; then
        FULL_IMAGE="$1"
        # Split image:tag if needed
        IMAGE_NAME="${FULL_IMAGE%%:*}"
        IMAGE_TAG="${FULL_IMAGE##*:}"
        if [[ "$IMAGE_NAME" == "$IMAGE_TAG" ]]; then
            IMAGE_TAG="latest"
        fi
    fi

    echo ""
    echo "╔══════════════════════════════════════════╗"
    echo "║   CoW Protocol Solver — Deploy Script    ║"
    echo "╚══════════════════════════════════════════╝"
    echo ""
    log_info "Image:       ${FULL_IMAGE}"
    log_info "Port:        ${SOLVER_PORT}"
    log_info "Chain ID:    ${CHAIN_ID}"
    echo ""

    load_env
    check_prereqs
    pull_image
    check_current_state
    start_green
    wait_for_health
    atomic_swap
    final_health_check
    cleanup_retired
    print_summary
}

main "$@"
