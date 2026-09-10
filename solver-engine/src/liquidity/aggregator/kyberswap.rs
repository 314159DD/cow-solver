//! KyberSwap Aggregator integration for Arbitrum.
//!
//! Free API with `x-client-id` header. 40+ DEX sources including RFQ
//! (Hashflow, 1inch Limit Orders). Two-step flow:
//!   1. GET  /routes          → route summary with routeSummary
//!   2. POST /route/build     → encoded swap calldata
//!
//! Docs: https://docs.kyberswap.com/kyberswap-solutions/kyberswap-aggregator/aggregator-api-specification

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tracing::debug;

use super::{AggregatorQuote, RateLimiter};

const KYBER_API_BASE: &str = "https://aggregator-api.kyberswap.com";

/// CoW Protocol settlement contract.
const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

// ── Response types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KyberRoutesResponse {
    code: Option<i32>,
    message: Option<String>,
    data: Option<KyberRoutesData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KyberRoutesData {
    route_summary: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KyberBuildResponse {
    code: Option<i32>,
    message: Option<String>,
    data: Option<KyberBuildData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KyberBuildData {
    #[serde(default)]
    data: String,
    #[serde(default)]
    router_address: String,
    #[serde(default)]
    gas: Option<String>,
}

// ── Aggregator ──────────────────────────────────────────────────────────────

pub struct KyberSwapAggregator {
    client: reqwest::Client,
    rate_limiter: RateLimiter,
    client_id: String,
}

impl KyberSwapAggregator {
    pub fn new() -> Self {
        let client_id = std::env::var("KYBER_CLIENT_ID")
            .unwrap_or_else(|_| "cow-solver".to_string());
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            rate_limiter: RateLimiter::new(Duration::from_millis(250)), // ~4 RPS
            client_id,
        }
    }

    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        _chain_id: u64,
    ) -> Result<AggregatorQuote> {
        self.rate_limiter.wait().await;

        // Step 1: GET /arbitrum/api/v1/routes
        let routes_url = format!("{}/arbitrum/api/v1/routes", KYBER_API_BASE);

        debug!(
            aggregator = "kyberswap",
            sell_token, buy_token, sell_amount,
            "Requesting KyberSwap route"
        );

        let routes_resp = self
            .client
            .get(&routes_url)
            .header("x-client-id", &self.client_id)
            .query(&[
                ("tokenIn", sell_token),
                ("tokenOut", buy_token),
                ("amountIn", sell_amount),
                ("saveGas", "false"),
                ("gasInclude", "true"),
            ])
            .send()
            .await
            .context("KyberSwap routes: request failed")?;

        if !routes_resp.status().is_success() {
            let status = routes_resp.status();
            let body = routes_resp.text().await.unwrap_or_default();
            bail!("KyberSwap routes returned {}: {}", status, &body[..body.len().min(200)]);
        }

        let routes: KyberRoutesResponse = routes_resp.json().await
            .context("KyberSwap routes: invalid JSON")?;

        if routes.code.unwrap_or(0) != 0 {
            bail!("KyberSwap routes error: {}", routes.message.unwrap_or_default());
        }

        let route_summary = routes.data
            .and_then(|d| d.route_summary)
            .context("KyberSwap: no routeSummary")?;

        let amount_out = route_summary["amountOut"]
            .as_str()
            .unwrap_or("0")
            .to_string();

        if amount_out == "0" {
            bail!("KyberSwap: zero amountOut");
        }

        let gas_estimate: u64 = route_summary["gas"]
            .as_str()
            .and_then(|g| g.parse().ok())
            .unwrap_or(300_000);

        // Step 2: POST /arbitrum/api/v1/route/build
        let build_url = format!("{}/arbitrum/api/v1/route/build", KYBER_API_BASE);

        let build_body = serde_json::json!({
            "routeSummary": &route_summary,
            "sender": SETTLEMENT_CONTRACT,
            "recipient": SETTLEMENT_CONTRACT,
            "slippageTolerance": 100, // 1% in bps
        });

        let build_resp = self
            .client
            .post(&build_url)
            .header("x-client-id", &self.client_id)
            .json(&build_body)
            .send()
            .await
            .context("KyberSwap build: request failed")?;

        if !build_resp.status().is_success() {
            let status = build_resp.status();
            let body = build_resp.text().await.unwrap_or_default();
            debug!(
                aggregator = "kyberswap",
                status = %status,
                "KyberSwap build failed, returning price-only"
            );
            return Ok(AggregatorQuote {
                buy_amount: amount_out,
                gas_estimate,
                calldata: String::new(),
                to: String::new(),
                value: "0".to_string(),
                sources: vec!["kyberswap".to_string()],
            });
        }

        let build: KyberBuildResponse = build_resp.json().await
            .context("KyberSwap build: invalid JSON")?;

        if build.code.unwrap_or(0) != 0 {
            debug!(
                aggregator = "kyberswap",
                error = %build.message.unwrap_or_default(),
                "KyberSwap build error, returning price-only"
            );
            return Ok(AggregatorQuote {
                buy_amount: amount_out,
                gas_estimate,
                calldata: String::new(),
                to: String::new(),
                value: "0".to_string(),
                sources: vec!["kyberswap".to_string()],
            });
        }

        let build_data = build.data.context("KyberSwap: no build data")?;

        let gas = build_data.gas
            .and_then(|g| g.parse::<u64>().ok())
            .unwrap_or(gas_estimate);

        Ok(AggregatorQuote {
            buy_amount: amount_out,
            gas_estimate: gas,
            calldata: build_data.data,
            to: build_data.router_address,
            value: "0".to_string(),
            sources: vec!["kyberswap".to_string()],
        })
    }
}
