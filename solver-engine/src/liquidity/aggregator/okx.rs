//! OKX DEX API aggregator integration.
//!
//! The CoW Protocol reference solver uses OKX as an external DEX aggregator.
//! OKX aggregates across 100+ liquidity sources on Arbitrum including
//! their own private market making.
//!
//! API: GET https://www.okx.com/api/v5/dex/aggregator/swap
//! Requires: OKX API key (free to obtain at okx.com)
//!
//! Reference: github.com/cowprotocol/services/tree/main/crates/solvers/src/infra/dex/okx

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tracing::debug;

use super::{AggregatorQuote, RateLimiter};

const OKX_API_BASE: &str = "https://www.okx.com/api/v5/dex/aggregator";

/// CoW Protocol settlement contract.
const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

#[derive(Debug, Deserialize)]
struct OkxResponse {
    code: String,
    data: Option<Vec<OkxSwapData>>,
    msg: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxSwapData {
    #[serde(default)]
    to_token_amount: String,
    #[serde(default)]
    estimate_gas_fee: Option<String>,
    #[serde(default)]
    tx: Option<OkxTx>,
    #[serde(default)]
    router_result: Option<OkxRouterResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxTx {
    #[serde(default)]
    data: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    value: String,
    #[serde(default)]
    gas: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxRouterResult {
    #[serde(default)]
    to_token_amount: String,
}

pub struct OkxAggregator {
    api_key: String,
    client: reqwest::Client,
    rate_limiter: RateLimiter,
}

impl OkxAggregator {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            rate_limiter: RateLimiter::new(Duration::from_millis(500)),
        }
    }

    /// Create from environment variable OKX_API_KEY.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("OKX_API_KEY").ok()?;
        if key.is_empty() {
            return None;
        }
        Some(Self::new(key))
    }

    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        chain_id: u64,
    ) -> Result<AggregatorQuote> {
        self.rate_limiter.wait().await;

        // Use /swap endpoint to get calldata (not just /quote which is price-only)
        let url = format!("{}/swap", OKX_API_BASE);

        debug!(
            aggregator = "okx",
            sell_token, buy_token, sell_amount,
            "Requesting OKX swap"
        );

        let chain_str = chain_id.to_string();
        let settlement = SETTLEMENT_CONTRACT.to_string();
        let resp = self
            .client
            .get(&url)
            .header("Ok-Access-Key", &self.api_key)
            .query(&[
                ("chainId", chain_str.as_str()),
                ("fromTokenAddress", sell_token),
                ("toTokenAddress", buy_token),
                ("amount", sell_amount),
                ("userWalletAddress", settlement.as_str()),
                ("slippage", "0.01"),
            ])
            .send()
            .await
            .context("OKX API: request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("OKX API returned {}: {}", status, &body[..body.len().min(200)]);
        }

        let body: OkxResponse = resp.json().await.context("OKX API: invalid JSON")?;

        if body.code != "0" {
            bail!("OKX API error {}: {}", body.code, body.msg.unwrap_or_default());
        }

        let data = body.data
            .and_then(|d| d.into_iter().next())
            .context("OKX API: empty response")?;

        let buy_amount = data.to_token_amount;
        if buy_amount == "0" || buy_amount.is_empty() {
            bail!("OKX API: zero buy amount");
        }

        let gas_estimate: u64 = data.estimate_gas_fee
            .as_deref()
            .and_then(|g| g.parse().ok())
            .unwrap_or(300_000);

        // Extract calldata from tx object
        let (calldata, to, value) = if let Some(tx) = data.tx {
            let gas = tx.gas.and_then(|g| g.parse::<u64>().ok()).unwrap_or(gas_estimate);
            (tx.data, tx.to, tx.value)
        } else {
            (String::new(), String::new(), "0".to_string())
        };

        Ok(AggregatorQuote {
            buy_amount,
            gas_estimate,
            calldata,
            to,
            value,
            sources: vec!["OKX".to_string()],
        })
    }
}
