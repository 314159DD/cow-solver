//! ParaSwap/Velora aggregator integration for Arbitrum.
//!
//! Free API, no key required. 21+ liquidity sources including:
//! UniswapV2/V3, SushiSwap, BalancerV2, Curve, Hashflow, DODO,
//! Camelot, Ramses, WooFi, AugustusRFQ (private), and more.
//!
//! Two-step flow:
//!   1. GET  /prices?network=42161&... → price route + priceRoute object
//!   2. POST /transactions/42161      → calldata for execution
//!
//! Docs: https://developers.velora.xyz (formerly developers.paraswap.io)

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tracing::debug;

use super::{AggregatorQuote, RateLimiter};

const PARASWAP_API: &str = "https://api.paraswap.io";

/// CoW Protocol settlement contract — used as sender for calldata generation.
const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParaswapPriceResponse {
    price_route: Option<serde_json::Value>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParaswapTxResponse {
    data: Option<String>,
    to: Option<String>,
    value: Option<String>,
    #[serde(default)]
    gas: Option<String>,
    error: Option<String>,
}

pub struct ParaswapAggregator {
    client: reqwest::Client,
    rate_limiter: RateLimiter,
}

impl ParaswapAggregator {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            rate_limiter: RateLimiter::new(Duration::from_millis(500)),
        }
    }

    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        chain_id: u64,
    ) -> Result<AggregatorQuote> {
        self.rate_limiter.wait().await;

        // Step 1: GET /prices → price route
        let prices_url = format!("{}/prices", PARASWAP_API);

        debug!(
            aggregator = "paraswap",
            sell_token, buy_token, sell_amount,
            "Requesting ParaSwap price"
        );

        let resp = self
            .client
            .get(&prices_url)
            .query(&[
                ("srcToken", sell_token),
                ("destToken", buy_token),
                ("amount", sell_amount),
                ("side", "SELL"),
                ("network", &chain_id.to_string()),
                ("partner", "cow-solver"),
            ])
            .send()
            .await
            .context("ParaSwap prices: request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("ParaSwap prices returned {}: {}", status, &body[..body.len().min(200)]);
        }

        let body: ParaswapPriceResponse = resp.json().await
            .context("ParaSwap prices: invalid JSON")?;

        if let Some(err) = body.error {
            bail!("ParaSwap prices error: {}", err);
        }

        let price_route = body.price_route.context("ParaSwap: no priceRoute")?;

        // Extract dest amount and gas from priceRoute
        let dest_amount = price_route["destAmount"]
            .as_str()
            .unwrap_or("0")
            .to_string();

        if dest_amount == "0" {
            bail!("ParaSwap: zero destAmount");
        }

        let gas_estimate: u64 = price_route["gasCost"]
            .as_str()
            .and_then(|g| g.parse().ok())
            .unwrap_or(300_000);

        // Extract sources
        let sources: Vec<String> = price_route["bestRoute"]
            .as_array()
            .map(|routes| {
                routes.iter()
                    .flat_map(|r| r["swaps"].as_array().into_iter().flatten())
                    .flat_map(|s| s["swapExchanges"].as_array().into_iter().flatten())
                    .filter_map(|e| e["exchange"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Step 2: POST /transactions/{network} → calldata
        let tx_url = format!("{}/transactions/{}", PARASWAP_API, chain_id);

        debug!(aggregator = "paraswap", "Building ParaSwap transaction");

        let tx_body = serde_json::json!({
            "srcToken": sell_token,
            "destToken": buy_token,
            "srcAmount": sell_amount,
            "destAmount": &dest_amount,
            "priceRoute": &price_route,
            "userAddress": SETTLEMENT_CONTRACT,
            "partner": "cow-solver",
            "ignoreChecks": true,
            "ignoreGasEstimation": true,
        });

        let tx_resp = self
            .client
            .post(&tx_url)
            .json(&tx_body)
            .send()
            .await
            .context("ParaSwap transactions: request failed")?;

        if !tx_resp.status().is_success() {
            let status = tx_resp.status();
            let body = tx_resp.text().await.unwrap_or_default();
            // Fall back to price-only quote if tx build fails
            debug!(
                aggregator = "paraswap",
                status = %status,
                "ParaSwap tx build failed, returning price-only quote"
            );
            return Ok(AggregatorQuote {
                buy_amount: dest_amount,
                gas_estimate,
                calldata: String::new(),
                to: String::new(),
                value: "0".to_string(),
                sources,
            });
        }

        let tx: ParaswapTxResponse = tx_resp.json().await
            .context("ParaSwap transactions: invalid JSON")?;

        if let Some(err) = tx.error {
            debug!(aggregator = "paraswap", error = %err, "ParaSwap tx error, price-only fallback");
            return Ok(AggregatorQuote {
                buy_amount: dest_amount,
                gas_estimate,
                calldata: String::new(),
                to: String::new(),
                value: "0".to_string(),
                sources,
            });
        }

        let calldata = tx.data.unwrap_or_default();
        let to = tx.to.unwrap_or_default();
        let value = tx.value.unwrap_or_else(|| "0".to_string());

        // Use gas from tx response if available
        let gas = tx.gas
            .and_then(|g| g.parse::<u64>().ok())
            .unwrap_or(gas_estimate);

        Ok(AggregatorQuote {
            buy_amount: dest_amount,
            gas_estimate: gas,
            calldata,
            to,
            value,
            sources,
        })
    }
}
