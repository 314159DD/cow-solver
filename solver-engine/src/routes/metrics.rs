use crate::monitoring;

/// GET /metrics — returns Prometheus text exposition format (0.0.4).
///
/// Suitable for scraping with Prometheus or any compatible agent (Grafana Agent,
/// VictoriaMetrics, etc.). Also human-readable as plain text.
pub async fn get_metrics() -> impl axum::response::IntoResponse {
    monitoring::metrics_handler().await
}
