//! OpenOcean aggregator integration.
//!
//! Free API, no key required. 1,000+ liquidity sources across 40+ chains.
//! Single-step flow: GET /v4/{chain}/swap → calldata ready to use.
//!
//! Docs: https://docs.openocean.finance/dev/aggregator-api-and-sdk/aggregator-api

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tracing::debug;

use super::{AggregatorQuote, RateLimiter};

const OPENOCEAN_API: &str = "https://open-api.openocean.finance/v4";

/// CoW Protocol settlement contract.
const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

#[derive(Debug, Deserialize)]
struct OpenOceanResponse {
    code: Option<i32>,
    data: Option<OpenOceanData>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenOceanData {
    #[serde(default)]
    out_amount: Option<String>,
    #[serde(default)]
    estimated_gas: Option<u64>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

pub struct OpenOceanAggregator {
    client: reqwest::Client,
    rate_limiter: RateLimiter,
}

impl OpenOceanAggregator {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            rate_limiter: RateLimiter::new(Duration::from_millis(500)), // 2 RPS
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

        // Single-step: GET /v4/arbitrum/swap
        let url = format!("{}/arbitrum/swap", OPENOCEAN_API);

        debug!(
            aggregator = "openocean",
            sell_token, buy_token, sell_amount,
            "Requesting OpenOcean swap"
        );

        let resp = self
            .client
            .get(&url)
            .query(&[
                ("inTokenAddress", sell_token),
                ("outTokenAddress", buy_token),
                ("amount", sell_amount),
                ("account", SETTLEMENT_CONTRACT),
                ("slippage", "1"), // 1%
                ("gasPrice", "0.1"), // 0.1 gwei (Arbitrum is cheap)
            ])
            .send()
            .await
            .context("OpenOcean API: request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("OpenOcean API returned {}: {}", status, &body[..body.len().min(200)]);
        }

        let body: OpenOceanResponse = resp.json().await
            .context("OpenOcean API: invalid JSON")?;

        if body.code.unwrap_or(0) != 200 {
            bail!("OpenOcean API error: {}", body.error.unwrap_or_else(|| format!("code {:?}", body.code)));
        }

        let data = body.data.context("OpenOcean: no data in response")?;

        let buy_amount = data.out_amount.unwrap_or_default();
        if buy_amount == "0" || buy_amount.is_empty() {
            bail!("OpenOcean: zero outAmount");
        }

        let gas_estimate = data.estimated_gas.unwrap_or(300_000);
        let calldata = data.data.unwrap_or_default();
        let to = data.to.unwrap_or_default();
        let value = data.value.unwrap_or_else(|| "0".to_string());

        Ok(AggregatorQuote {
            buy_amount,
            gas_estimate,
            calldata,
            to,
            value,
            sources: vec!["openocean".to_string()],
        })
    }
}
