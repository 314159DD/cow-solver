//! POST /api/discover — trigger pool discovery from the dashboard.

use axum::{Json, http::StatusCode, response::IntoResponse};
use axum::extract::Query;
use tracing::info;

use super::dashboard::TokenParam;

/// POST /api/discover — trigger pool discovery.
/// Requires dashboard token. Runs async in background, returns immediately.
pub async fn trigger_discovery(Query(params): Query<TokenParam>) -> impl IntoResponse {
    if !super::dashboard::check_auth(&params) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized"}))).into_response();
    }

    if crate::pool_discovery::is_running() {
        return (StatusCode::CONFLICT, Json(serde_json::json!({
            "status": "already_running",
            "message": "Discovery is already in progress"
        }))).into_response();
    }

    // Get RPC URL and chain ID from env
    let rpc_url = std::env::var("RPC_URL").unwrap_or_default();
    let chain_id: u64 = std::env::var("CHAIN_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(42161);

    if rpc_url.is_empty() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
            "error": "RPC_URL not configured"
        }))).into_response();
    }

    info!("Pool discovery triggered from dashboard");

    // Run in background — don't block the HTTP response
    tokio::spawn(async move {
        match crate::pool_discovery::run_discovery(&rpc_url, chain_id).await {
            Ok(count) => info!(pools = count, "Dashboard-triggered discovery complete"),
            Err(e) => tracing::warn!(error = %e, "Dashboard-triggered discovery failed"),
        }
    });

    (StatusCode::OK, Json(serde_json::json!({
        "status": "started",
        "message": "Pool discovery started in background. Check dashboard for progress."
    }))).into_response()
}

/// GET /api/discovery-status — check discovery state.
pub async fn discovery_status(Query(params): Query<TokenParam>) -> impl IntoResponse {
    if !super::dashboard::check_auth(&params) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized"}))).into_response();
    }

    let running = crate::pool_discovery::is_running();
    let (last_run, pool_count) = crate::pool_discovery::last_discovery_info()
        .unwrap_or(("never".to_string(), 0));

    (StatusCode::OK, Json(serde_json::json!({
        "running": running,
        "last_run": last_run,
        "discovered_pools": pool_count,
        "indexed_pools": crate::pool_indexer::pool_count(),
    }))).into_response()
}
