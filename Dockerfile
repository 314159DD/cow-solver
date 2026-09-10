# ─── Build stage ────────────────────────────────────────────────────────────
FROM rust:1.86-slim AS builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace manifests first to cache dependency downloads
COPY Cargo.toml Cargo.lock* ./
COPY solver-engine/Cargo.toml ./solver-engine/
COPY shared/Cargo.toml ./shared/
COPY benchmarks/Cargo.toml ./benchmarks/

# Create dummy source files so cargo can fetch and cache deps
RUN mkdir -p solver-engine/src shared/src benchmarks/src benchmarks/benches \
    && echo "fn main() {}" > solver-engine/src/main.rs \
    && echo "" > shared/src/lib.rs \
    && echo "" > benchmarks/src/lib.rs \
    && cargo build --release -p solver-engine 2>/dev/null || true \
    && rm -rf solver-engine/src shared/src benchmarks/src benchmarks/benches

# Copy actual source
COPY solver-engine/ ./solver-engine/
COPY shared/ ./shared/
COPY benchmarks/ ./benchmarks/

# Build the release binary
RUN cargo build --release -p solver-engine

# ─── Runtime stage ───────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the compiled binary
COPY --from=builder /app/target/release/solver-engine /app/solver-engine

# Copy sample data (for local testing)
COPY data/ ./data/

# Expose solver port
EXPOSE 8000

# Health check
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:${SOLVER_PORT:-8000}/health || exit 1

# Default environment
ENV SOLVER_PORT=8000 \
    CHAIN_ID=1 \
    LOG_LEVEL=info \
    MAX_SOLVE_TIME_MS=25000

ENTRYPOINT ["/app/solver-engine"]
