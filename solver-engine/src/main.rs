use std::net::SocketAddr;

use axum::{Router, routing::get};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{EnvFilter, fmt};

use solver_engine::config::Config;
use solver_engine::routes;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present
    let _ = dotenvy::dotenv();

    // Load config first so we can use log_level for the filter
    let config = Config::from_env()?;

    // Initialize structured JSON logging using configured level
    fmt()
        .with_env_filter(EnvFilter::new(&config.log_level))
        .json()
        .init();

    tracing::info!(
        port = config.solver_port,
        chain_id = config.chain_id,
        log_level = %config.log_level,
        max_solve_time_ms = config.max_solve_time_ms,
        driver_url = config.driver_url.as_deref().unwrap_or("<none>"),
        rpc_url = %config.rpc_url_redacted(),
        "Starting CoW Protocol Solver"
    );

    // Spawn monitoring background task (alert checks, hourly summaries, revenue persistence)
    tokio::spawn(solver_engine::monitoring::run_background_task());

    // Spawn gas price background refresh (B.3 — avoids RPC calls during solve window)
    let rpc = shared::rpc::EthClient::new(&config.rpc_url, config.chain_id);
    // Refresh every 300s (was 60s). With aggregator routing, gas prices are less critical.
    tokio::spawn(solver_engine::gas::oracle::run_gas_refresh(rpc, config.chain_id, 300));

    // Load discovered pools from file FIRST (zero RPC calls, instant).
    // Must happen BEFORE pool_indexer::run() so the initial full refresh
    // covers all discovered pools, not just the bootstrap set.
    let loaded = solver_engine::pool_discovery::load_from_file();
    tracing::info!(loaded, "Loaded discovered pools from file into indexer");

    // NOW spawn pool indexer — its initial full refresh will cover all loaded pools.
    tokio::spawn(solver_engine::pool_indexer::run(config.rpc_url.clone(), config.chain_id));

    // Auto-discover if pool file is empty or has very few pools.
    // Runs in background after 3s delay so the solver starts accepting auctions immediately.
    if loaded < 100 {
        let rpc_url = config.rpc_url.clone();
        let chain_id = config.chain_id;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            tracing::info!(current_pools = loaded, "Auto-discovering pools (loaded < 100)");
            match solver_engine::pool_discovery::run_discovery(&rpc_url, chain_id).await {
                Ok(count) => tracing::info!(discovered = count, "Auto-discovery complete"),
                Err(e) => tracing::warn!(error = %e, "Auto-discovery failed"),
            }
        });
    }

    // Bootstrap in-memory state from replay DB so dashboard survives restarts.
    // Must run BEFORE competition tracker starts adding new results.
    solver_engine::monitoring::bootstrap_from_replay();
    solver_engine::competition::bootstrap_from_replay().await;

    // Spawn competition tracker (queries CoW API for winner data after each auction)
    tokio::spawn(solver_engine::competition::run_poll_task());

    // Spawn subgraph updater (fetches V3 tick data from The Graph every 30s)
    tokio::spawn(solver_engine::subgraph::run_subgraph_updater());

    // Spawn WebSocket monitor (real-time V2 Sync + V3 Swap events, sub-second freshness)
    tokio::spawn(solver_engine::ws_monitor::run_ws_monitor());

    // Spawn Bebop Price API stream (real-time MM price levels for pre-trade filtering)
    let bebop_book = solver_engine::liquidity::bebop_pricer::new_price_book();
    tokio::spawn(solver_engine::liquidity::bebop_pricer::start_price_stream(
        bebop_book, config.chain_id,
    ));

    let app = Router::new()
        .route("/health", get(routes::health::health_check))
        .route("/metrics", get(routes::metrics::get_metrics))
        .route("/dashboard", get(routes::dashboard::dashboard_page))
        .route("/api/stats", get(routes::dashboard::api_stats))
        .route("/solve", axum::routing::post(routes::solve::solve))
        .route("/api/discover", axum::routing::post(routes::discovery::trigger_discovery))
        .route("/api/discovery-status", get(routes::discovery::discovery_status))
        .layer(axum::extract::DefaultBodyLimit::max(250 * 1024 * 1024)) // 250MB — CoW auctions with V3 tick data can be huge
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], config.solver_port));
    tracing::info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Server shut down gracefully");
    Ok(())
}

/// Resolves when SIGTERM or Ctrl-C is received.
async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received Ctrl-C, shutting down"),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down"),
    }
}
