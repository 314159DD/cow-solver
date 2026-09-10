//! 1inch API v6 aggregator integration.
//!
//! Docs: https://portal.1inch.dev/documentation/swap/swagger
//!
//! Quote endpoint: GET https://api.1inch.dev/swap/v6.0/{chainId}/quote
//! Swap endpoint:  GET https://api.1inch.dev/swap/v6.0/{chainId}/swap
//! Auth: Header `Authorization: Bearer {API_KEY}`

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tracing::debug;

use super::{AggregatorQuote, RateLimiter};

/// CoW Protocol settlement contract — used as the `from` address so 1inch
/// can simulate approvals correctly.
const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

/// Raw JSON response from the 1inch /quote endpoint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OneInchQuoteResponse {
    dst_amount: String,
    #[serde(default)]
    gas: Option<u64>,
}

/// Raw JSON response from the 1inch /swap endpoint (superset of quote).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OneInchSwapResponse {
    dst_amount: String,
    #[serde(default)]
    gas: Option<u64>,
    tx: OneInchTx,
    #[serde(default)]
    protocols: Vec<Vec<Vec<OneInchProtocol>>>,
}

#[derive(Debug, Deserialize)]
struct OneInchTx {
    data: String,
    to: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    gas: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OneInchProtocol {
    name: String,
    #[serde(default)]
    part: Option<f64>,
}

pub struct OneInchAggregator {
    api_key: String,
    client: reqwest::Client,
    rate_limiter: RateLimiter,
}

impl OneInchAggregator {
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client");

        Self {
            api_key,
            client,
            rate_limiter: RateLimiter::new(Duration::from_secs(1)),
        }
    }

    /// Create from environment variable ONEINCH_API_KEY.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("ONEINCH_API_KEY").ok()?;
        if key.is_empty() {
            return None;
        }
        Some(Self::new(key))
    }
}

impl OneInchAggregator {
    pub async fn get_quote(
        &self,
        sell_token: &str,
        buy_token: &str,
        sell_amount: &str,
        chain_id: u64,
    ) -> Result<AggregatorQuote> {
        // Respect rate limit (max 1 req/sec).
        self.rate_limiter.wait().await;

        // Use the /swap endpoint to get both quote and calldata in one call.
        let url = format!(
            "https://api.1inch.dev/swap/v6.0/{}/swap",
            chain_id
        );

        debug!(
            aggregator = "1inch",
            sell_token, buy_token, sell_amount, chain_id,
            "Requesting 1inch quote"
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .query(&[
                ("src", sell_token),
                ("dst", buy_token),
                ("amount", sell_amount),
                ("from", SETTLEMENT_CONTRACT),
                ("slippage", "1"), // 1%
                ("disableEstimate", "true"), // skip on-chain simulation for speed
            ])
            .send()
            .await
            .context("1inch API: HTTP request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("1inch API returned {}: {}", status, body);
        }

        let swap: OneInchSwapResponse =
            resp.json().await.context("1inch API: invalid JSON")?;

        let gas_estimate = swap
            .tx
            .gas
            .or(swap.gas)
            .unwrap_or(250_000); // conservative default

        // Flatten the nested protocols array to get source names.
        let sources: Vec<String> = swap
            .protocols
            .iter()
            .flat_map(|step| step.iter())
            .flat_map(|route| route.iter())
            .filter(|p| p.part.unwrap_or(0.0) > 0.0)
            .map(|p| p.name.clone())
            .collect();

        Ok(AggregatorQuote {
            buy_amount: swap.dst_amount,
            gas_estimate,
            calldata: swap.tx.data,
            to: swap.tx.to,
            value: swap.tx.value.unwrap_or_else(|| "0".to_string()),
            sources,
        })
    }

    fn name(&self) -> &str {
        "1inch"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settlement_contract_address_valid() {
        assert!(SETTLEMENT_CONTRACT.starts_with("0x"));
        assert_eq!(SETTLEMENT_CONTRACT.len(), 42);
    }

    #[test]
    fn deserialize_swap_response() {
        let json = r#"{
            "dstAmount": "2000000",
            "gas": 180000,
            "tx": {
                "data": "0xcafe",
                "to": "0x1111111254eeb25477b68fb85ed929f73a960582",
                "value": "0",
                "gas": 200000
            },
            "protocols": [
                [[{"name": "UNISWAP_V3", "part": 60.0}, {"name": "SUSHI", "part": 40.0}]]
            ]
        }"#;
        let resp: OneInchSwapResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.dst_amount, "2000000");
        assert_eq!(resp.tx.to, "0x1111111254eeb25477b68fb85ed929f73a960582");
        assert_eq!(resp.protocols.len(), 1);
    }

    #[test]
    fn deserialize_quote_response() {
        let json = r#"{
            "dstAmount": "5000000",
            "gas": 150000
        }"#;
        let resp: OneInchQuoteResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.dst_amount, "5000000");
        assert_eq!(resp.gas, Some(150000));
    }
}
