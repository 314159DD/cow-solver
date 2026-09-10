//! 0x Swap API v2 aggregator integration.
//!
//! Docs: https://0x.org/docs/api
//!
//! v2 uses a unified endpoint for all chains:
//!   GET https://api.0x.org/swap/permit2/quote?chainId=42161&...
//! Auth: Headers `0x-api-key: {API_KEY}` + `0x-version: v2`

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tracing::debug;

use super::{AggregatorQuote, RateLimiter};

/// 0x Exchange Proxy address (same across most EVM chains).
const ZEROX_EXCHANGE_PROXY: &str = "0xDef1C0ded9bec7F1a1670819833240f027b25EfF";

/// v2 API base URL — unified for all chains (chain specified via query param).
const ZEROX_API_BASE: &str = "https://api.0x.org";

/// Supported chains for 0x v2 API.
fn is_supported_chain(chain_id: u64) -> bool {
    matches!(chain_id, 1 | 42161 | 10 | 137 | 8453 | 100 | 56 | 43114 | 534352)
}

// Keep for test compatibility
#[allow(dead_code)]
fn base_url(chain_id: u64) -> Option<&'static str> {
    if is_supported_chain(chain_id) { Some(ZEROX_API_BASE) } else { None }
}

/// Raw JSON response from the 0x v2 /swap/permit2/quote endpoint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ZeroxQuoteResponse {
    buy_amount: String,
    #[serde(default)]
    gas: Option<String>,
    #[serde(default)]
    estimated_gas: Option<String>,
    // v2 puts calldata under transaction.data (v1 had top-level data)
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    value: Option<String>,
    // v2 nests execution data under 'transaction'
    #[serde(default)]
    transaction: Option<ZeroxTransaction>,
    #[serde(default)]
    sources: Vec<ZeroxSource>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ZeroxTransaction {
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
struct ZeroxSource {
    name: String,
    #[serde(default)]
    proportion: Option<String>,
}

pub struct ZeroxAggregator {
    api_key: String,
    client: reqwest::Client,
    rate_limiter: RateLimiter,
}

impl ZeroxAggregator {
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client");

        Self {
            api_key,
            client,
            rate_limiter: RateLimiter::new(Duration::from_millis(250)), // 4 req/sec (under 5/s Standard limit)
        }
    }

    /// Create from environment variable ZEROX_API_KEY.
    /// Returns None only if explicitly disabled via ZEROX_ENABLED=false.
    /// Works without API key (lower rate limits) or with key (5 RPS).
    pub fn from_env() -> Option<Self> {
        if std::env::var("ZEROX_ENABLED").ok().as_deref() == Some("false") {
            return None;
        }
        let key = std::env::var("ZEROX_API_KEY").unwrap_or_default();
        Some(Self::new(key))
    }
}

impl ZeroxAggregator {
    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        chain_id: u64,
    ) -> Result<AggregatorQuote> {
        if !is_supported_chain(chain_id) {
            bail!("0x API: unsupported chain {}", chain_id);
        }

        // Respect rate limit (max 5 req/sec on Standard plan).
        self.rate_limiter.wait().await;

        // v2 API: allowance-holder endpoint (permit2 causes server errors with contract taker)
        let url = format!("{}/swap/allowance-holder/quote", ZEROX_API_BASE);
        debug!(
            aggregator = "0x",
            sell_token, buy_token, sell_amount, chain_id,
            "Requesting 0x v2 quote"
        );

        // taker address required by v2 — use a dummy EOA (settlement contract causes server errors)
        let taker = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"; // vitalik.eth (just for quoting)
        let chain_str = chain_id.to_string();

        let mut req = self
            .client
            .get(&url)
            .header("0x-version", "v2");
        if !self.api_key.is_empty() {
            req = req.header("0x-api-key", &self.api_key);
        }
        let resp = req
            .query(&[
                ("chainId", chain_str.as_str()),
                ("sellToken", sell_token),
                ("buyToken", buy_token),
                ("sellAmount", sell_amount),
                ("taker", taker),
            ])
            .send()
            .await
            .context("0x API: HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("0x API returned {}: {}", status, body);
        }

        let body_text = resp.text().await.context("0x API: failed to read body")?;

        // Check for "no liquidity" response before attempting full parse
        if body_text.contains("\"liquidityAvailable\":false") {
            bail!("0x API: no liquidity available for this pair");
        }
        // Check for error responses
        if body_text.contains("\"name\":\"") && body_text.contains("\"message\":\"") && !body_text.contains("\"buyAmount\"") {
            bail!("0x API error: {}", &body_text[..body_text.len().min(200)]);
        }

        let quote: ZeroxQuoteResponse = serde_json::from_str(&body_text)
            .with_context(|| {
                let preview = if body_text.len() > 200 { &body_text[..200] } else { &body_text };
                format!("0x API: JSON parse failed. Response preview: {}", preview)
            })?;

        // Extract execution data from either v2 transaction object or v1 top-level fields
        let (calldata, to_addr, tx_value) = if let Some(tx) = &quote.transaction {
            (tx.data.clone(), tx.to.clone(), tx.value.clone())
        } else {
            (
                quote.data.unwrap_or_default(),
                quote.to.unwrap_or_else(|| ZEROX_EXCHANGE_PROXY.to_string()),
                quote.value.unwrap_or_else(|| "0".to_string()),
            )
        };

        let gas_estimate: u64 = quote.transaction.as_ref()
            .and_then(|tx| tx.gas.as_deref())
            .or(quote.gas.as_deref())
            .or(quote.estimated_gas.as_deref())
            .and_then(|g| g.parse().ok())
            .unwrap_or(200_000);

        let sources: Vec<String> = quote
            .sources
            .iter()
            .filter(|s| {
                s.proportion
                    .as_deref()
                    .and_then(|p| p.parse::<f64>().ok())
                    .map(|p| p > 0.0)
                    .unwrap_or(false)
            })
            .map(|s| s.name.clone())
            .collect();

        Ok(AggregatorQuote {
            buy_amount: quote.buy_amount,
            gas_estimate,
            calldata,
            to: to_addr,
            value: tx_value,
            sources,
        })
    }

    fn name(&self) -> &str {
        "0x"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_arbitrum() {
        assert_eq!(base_url(42161), Some(ZEROX_API_BASE));
    }

    #[test]
    fn base_url_mainnet() {
        assert_eq!(base_url(1), Some(ZEROX_API_BASE));
    }

    #[test]
    fn base_url_unsupported() {
        assert_eq!(base_url(999), None);
    }

    #[test]
    fn exchange_proxy_is_checksummed() {
        assert!(ZEROX_EXCHANGE_PROXY.starts_with("0x"));
        assert_eq!(ZEROX_EXCHANGE_PROXY.len(), 42);
    }

    #[test]
    fn deserialize_quote_response() {
        let json = r#"{
            "buyAmount": "1000000",
            "gas": "200000",
            "data": "0xdeadbeef",
            "to": "0xDef1C0ded9bec7F1a1670819833240f027b25EfF",
            "value": "0",
            "sources": [
                {"name": "Uniswap_V3", "proportion": "0.8"},
                {"name": "SushiSwap", "proportion": "0.2"},
                {"name": "Balancer_V2", "proportion": "0"}
            ]
        }"#;
        let resp: ZeroxQuoteResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.buy_amount, "1000000");
        assert_eq!(resp.gas.as_deref(), Some("200000"));
        assert_eq!(resp.sources.len(), 3);
    }
}
