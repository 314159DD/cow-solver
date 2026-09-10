//! Odos API aggregator integration.
//!
//! Docs: https://docs.odos.xyz
//! Arbitrum chain_id: 42161
//! No API key required for basic access.
//!
//! Two-step flow:
//! 1. POST /sor/quote/v2 — get quote with path info
//! 2. POST /sor/assemble — get executable calldata
//!
//! For speed in a solver context, we use /sor/quote/v2 with simple=true
//! which returns output amount without full path details.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::{AggregatorQuote, BudgetLimiter, RateLimiter, checksum_address};

const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OdosQuoteRequest {
    chain_id: u64,
    input_tokens: Vec<OdosInputToken>,
    output_tokens: Vec<OdosOutputToken>,
    slippage_limit_percent: f64,
    user_addr: String,
    #[serde(default)]
    referral_code: u32,
    compact: bool,
    disable_rfqs: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OdosInputToken {
    token_address: String,
    amount: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OdosOutputToken {
    token_address: String,
    proportion: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OdosQuoteResponse {
    out_amounts: Vec<String>,
    gas_estimate: Option<f64>,
    path_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OdosAssembleRequest {
    user_addr: String,
    path_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OdosAssembleResponse {
    transaction: OdosTx,
}

#[derive(Debug, Deserialize)]
struct OdosTx {
    to: String,
    data: String,
    value: String,
    gas: Option<u64>,
}

pub struct OdosAggregator {
    client: reqwest::Client,
    rate_limiter: RateLimiter,
    budget_limiter: BudgetLimiter,
    api_key: Option<String>,
}

impl OdosAggregator {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .expect("reqwest client");

        let api_key = std::env::var("ODOS_API_KEY").ok()
            .filter(|k| !k.is_empty());

        Self {
            client,
            rate_limiter: RateLimiter::new(Duration::from_millis(1000)), // 1 RPS max (free tier)
            budget_limiter: BudgetLimiter::from_env(),
            api_key,
        }
    }

    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        chain_id: u64,
    ) -> Result<AggregatorQuote> {
        // Check daily/hourly budget BEFORE waiting on rate limiter
        if !self.budget_limiter.try_acquire().await {
            let (daily_used, daily_max, hourly_used, hourly_max) = self.budget_limiter.usage().await;
            tracing::warn!(
                daily_used, daily_max, hourly_used, hourly_max,
                "Odos budget exhausted — skipping quote to preserve API quota"
            );
            bail!("Odos daily/hourly budget exhausted ({daily_used}/{daily_max} daily, {hourly_used}/{hourly_max} hourly)");
        }

        self.rate_limiter.wait().await;

        debug!(
            aggregator = "odos",
            sell_token, buy_token, sell_amount, chain_id,
            "Requesting Odos quote"
        );

        // Step 1: Get quote
        let quote_req = OdosQuoteRequest {
            chain_id,
            input_tokens: vec![OdosInputToken {
                token_address: checksum_address(sell_token),
                amount: sell_amount.to_string(),
            }],
            output_tokens: vec![OdosOutputToken {
                token_address: checksum_address(buy_token),
                proportion: 1.0,
            }],
            slippage_limit_percent: 0.3,
            user_addr: SETTLEMENT_CONTRACT.to_string(),
            referral_code: 0,
            compact: true,
            disable_rfqs: true,
        };

        // Try enterprise API first (if key configured), fall back to public.
        // Public API at api.odos.xyz works with NO auth — just Content-Type.
        // Enterprise API at enterprise-api.odos.xyz uses x-api-key header.
        // IMPORTANT: Do NOT send auth headers to public API — can cause silent rejection.
        let (base_url, use_key) = if self.api_key.is_some() {
            ("https://enterprise-api.odos.xyz", true)
        } else {
            ("https://api.odos.xyz", false)
        };

        let mut req = self.client
            .post(format!("{}/sor/quote/v2", base_url))
            .header("Content-Type", "application/json");

        if use_key {
            if let Some(key) = &self.api_key {
                req = req.header("x-api-key", key.as_str());
            }
        }

        let resp = req
            .json(&quote_req)
            .send()
            .await
            .context("Odos API: quote request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Odos API returned {}: {}", status, body);
        }

        let quote: OdosQuoteResponse = resp.json().await
            .context("Odos API: invalid quote JSON")?;

        let buy_amount = quote.out_amounts.first()
            .cloned()
            .unwrap_or_else(|| "0".to_string());
        let gas_estimate = quote.gas_estimate.map(|g| g as u64).unwrap_or(300_000);

        // Step 2: Assemble transaction (if we got a path_id)
        let (calldata, to, value) = if let Some(path_id) = &quote.path_id {
            let assemble_req = OdosAssembleRequest {
                user_addr: SETTLEMENT_CONTRACT.to_string(),
                path_id: path_id.clone(),
            };

            let mut assemble_req_builder = self.client
                .post(format!("{}/sor/assemble", base_url))
                .header("Content-Type", "application/json");
            if let Some(key) = &self.api_key {
                assemble_req_builder = assemble_req_builder.header("x-api-key", key.as_str());
            }
            match assemble_req_builder
                .json(&assemble_req)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<OdosAssembleResponse>().await {
                        Ok(assembled) => (
                            assembled.transaction.data,
                            assembled.transaction.to,
                            assembled.transaction.value,
                        ),
                        Err(_) => ("0x".to_string(), "0x".to_string(), "0".to_string()),
                    }
                }
                _ => ("0x".to_string(), "0x".to_string(), "0".to_string()),
            }
        } else {
            ("0x".to_string(), "0x".to_string(), "0".to_string())
        };

        Ok(AggregatorQuote {
            buy_amount,
            gas_estimate,
            calldata,
            to,
            value,
            sources: vec!["odos".to_string()],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_request_serializes() {
        let req = OdosQuoteRequest {
            chain_id: 42161,
            input_tokens: vec![OdosInputToken {
                token_address: "0xweth".to_string(),
                amount: "1000000000000000000".to_string(),
            }],
            output_tokens: vec![OdosOutputToken {
                token_address: "0xusdc".to_string(),
                proportion: 1.0,
            }],
            slippage_limit_percent: 0.3,
            user_addr: SETTLEMENT_CONTRACT.to_string(),
            referral_code: 0,
            compact: true,
            disable_rfqs: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("42161"));
        assert!(json.contains("proportion"));
    }
}
