//! Bebop RFQ + JAM aggregator integration.
//!
//! Two modes:
//! - RFQ (PMM): Private market maker quotes — best prices, guaranteed fill
//!   Endpoint: GET https://api.bebop.xyz/pmm/{network}/v3/quote
//! - JAM: DEX aggregation fallback — broader pair coverage
//!   Endpoint: GET https://api.bebop.xyz/jam/{network}/v2/quote
//!
//! Auth: source + source-auth headers (from BEBOP_SOURCE + BEBOP_SOURCE_AUTH env)
//! Rate limit: 60 quotes / 4 seconds per chain (authenticated)

use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::debug;

use super::{AggregatorQuote, RateLimiter};

/// CoW Protocol settlement contract — used as taker for quoting.
const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

pub struct BebopAggregator {
    client: reqwest::Client,
    rate_limiter: RateLimiter,
    source: String,
    source_auth: String,
}

impl BebopAggregator {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("reqwest client");

        Self {
            client,
            rate_limiter: RateLimiter::new(Duration::from_millis(100)), // 10 req/s (under 15/s limit)
            source: std::env::var("BEBOP_SOURCE").unwrap_or_default(),
            source_auth: std::env::var("BEBOP_SOURCE_AUTH").unwrap_or_default(),
        }
    }

    /// Primary: RFQ quote from private market makers.
    /// Falls back to JAM (DEX aggregation) if RFQ has no liquidity.
    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        chain_id: u64,
    ) -> Result<AggregatorQuote> {
        let network = bebop_network(chain_id)
            .ok_or_else(|| anyhow::anyhow!("Bebop: unsupported chain {}", chain_id))?;

        // Try RFQ first (better prices from private MMs)
        match self.rfq_quote(network, sell_token, buy_token, sell_amount).await {
            Ok(quote) if quote.buy_amount_u128() > 0 => {
                debug!(aggregator = "bebop-rfq", buy = %quote.buy_amount, "RFQ quote received");
                return Ok(quote);
            }
            Ok(_) => {
                debug!(aggregator = "bebop-rfq", "RFQ returned zero — trying JAM");
            }
            Err(e) => {
                debug!(aggregator = "bebop-rfq", error = %e, "RFQ failed — trying JAM");
            }
        }

        // Fallback: JAM DEX aggregation
        self.jam_quote(network, sell_token, buy_token, sell_amount).await
    }

    /// RFQ quote: private market maker pricing with calldata.
    async fn rfq_quote(
        &self,
        network: &str,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
    ) -> Result<AggregatorQuote> {
        self.rate_limiter.wait().await;

        let sell_cs = super::checksum_address(sell_token);
        let buy_cs = super::checksum_address(buy_token);

        let url = format!("https://api.bebop.xyz/pmm/{}/v3/quote", network);

        let mut req = self.client.get(&url).query(&[
            ("sell_tokens", sell_cs.as_str()),
            ("buy_tokens", buy_cs.as_str()),
            ("sell_amounts", sell_amount),
            ("taker_address", SETTLEMENT_CONTRACT),
            ("gasless", "false"),
            ("expiry_type", "short"),
            ("approval_type", "Standard"),
        ]);

        if !self.source.is_empty() {
            req = req.query(&[("source", self.source.as_str())]);
        }
        if !self.source_auth.is_empty() {
            req = req.query(&[("source-auth", self.source_auth.as_str())]);
        }

        let resp = req.send().await.context("Bebop RFQ: request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Bebop RFQ returned {}: {}", status, &body[..body.len().min(200)]);
        }

        let body: serde_json::Value = resp.json().await.context("Bebop RFQ: invalid JSON")?;

        if body.get("error").is_some() && body.get("buyTokens").is_none() {
            bail!("Bebop RFQ error: {}", &body.to_string()[..200.min(body.to_string().len())]);
        }

        parse_bebop_response(&body, "bebop-rfq")
    }

    /// JAM quote: DEX aggregation fallback.
    async fn jam_quote(
        &self,
        network: &str,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
    ) -> Result<AggregatorQuote> {
        self.rate_limiter.wait().await;

        let sell_cs = super::checksum_address(sell_token);
        let buy_cs = super::checksum_address(buy_token);

        let url = format!("https://api.bebop.xyz/jam/{}/v2/quote", network);

        let mut req = self.client.get(&url).query(&[
            ("sell_tokens", sell_cs.as_str()),
            ("buy_tokens", buy_cs.as_str()),
            ("sell_amounts", sell_amount),
            ("taker_address", SETTLEMENT_CONTRACT),
            ("gasless", "false"),
            ("approval_type", "Standard"),
            ("slippage", "50"), // 50 bps = 0.5%
        ]);

        if !self.source.is_empty() {
            req = req.query(&[("source", self.source.as_str())]);
        }
        if !self.source_auth.is_empty() {
            req = req.query(&[("source-auth", self.source_auth.as_str())]);
        }

        let resp = req.send().await.context("Bebop JAM: request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Bebop JAM returned {}: {}", status, &body[..body.len().min(200)]);
        }

        let body: serde_json::Value = resp.json().await.context("Bebop JAM: invalid JSON")?;

        if body.get("error").is_some() && body.get("buyTokens").is_none() {
            bail!("Bebop JAM error: {}", &body.to_string()[..200.min(body.to_string().len())]);
        }

        parse_bebop_response(&body, "bebop-jam")
    }
}

/// Parse the common Bebop response format (shared by RFQ and JAM).
fn parse_bebop_response(body: &serde_json::Value, source_name: &str) -> Result<AggregatorQuote> {
    // Buy amount from buyTokens map
    let buy_amount = body["buyTokens"]
        .as_object()
        .and_then(|m| m.values().next())
        .and_then(|v| v["amount"].as_str())
        .unwrap_or("0")
        .to_string();

    if buy_amount == "0" {
        bail!("{}: no buy amount in response", source_name);
    }

    // Gas from tx object or gasFee
    let gas_estimate = body["tx"]["gas"]
        .as_u64()
        .or_else(|| {
            body["gasFee"]["native"]
                .as_str()
                .and_then(|s| s.parse::<u128>().ok())
                .map(|wei| (wei / 1_000_000_000) as u64) // convert wei to gas units approx
        })
        .unwrap_or(200_000);

    // Calldata from tx object (only present with gasless=false)
    let calldata = body["tx"]["data"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let to = body["tx"]["to"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let value = body["tx"]["value"]
        .as_str()
        .unwrap_or("0")
        .to_string();

    Ok(AggregatorQuote {
        buy_amount,
        gas_estimate,
        calldata,
        to,
        value,
        sources: vec![source_name.to_string()],
    })
}

/// Map chain ID to Bebop network name.
fn bebop_network(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        1 => Some("ethereum"),
        10 => Some("optimism"),
        56 => Some("bsc"),
        137 => Some("polygon"),
        8453 => Some("base"),
        42161 => Some("arbitrum"),
        324 => Some("zksync"),
        534352 => Some("scroll"),
        81457 => Some("blast"),
        _ => None,
    }
}

