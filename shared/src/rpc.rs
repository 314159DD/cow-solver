/// Ethereum JSON-RPC client.
///
/// Implemented with raw reqwest HTTP calls (JSON-RPC 2.0) so it works
/// with the current toolchain. Replace internals with alloy when rustc >= 1.88.
///
/// All read operations are safe to call against any Alchemy/Infura endpoint
/// on the configured chain. Write operations (eth_sendRawTransaction) are NOT
/// implemented — this solver only reads chain state.
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::debug;

// ── JSON-RPC plumbing ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Deserialize, Debug)]
struct JsonRpcResponse {
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
}

// ── Multicall helpers ─────────────────────────────────────────────────────────

/// A single `eth_call` to batch inside a multicall
#[derive(Debug, Clone)]
pub struct CallRequest {
    /// Contract address (checksummed hex)
    pub to: String,
    /// ABI-encoded calldata (hex with 0x prefix)
    pub data: String,
}

/// Result of a single call inside a multicall batch
#[derive(Debug, Clone)]
pub struct CallResult {
    pub success: bool,
    /// Returned bytes as hex string
    pub return_data: String,
}

// ERC-20 selector constants (keccak256 first 4 bytes)
const SELECTOR_DECIMALS: &str = "0x313ce567";
const SELECTOR_BALANCE_OF: &str = "0x70a08231";

// ── EthClient ─────────────────────────────────────────────────────────────────

/// Ethereum RPC client — wraps JSON-RPC 2.0 over HTTP.
///
/// All methods return `anyhow::Result` and handle network/RPC errors.
/// Individual call failures are propagated; use `multicall` for batching.
#[derive(Debug, Clone)]
pub struct EthClient {
    pub rpc_url: String,
    pub chain_id: u64,
    http: Client,
}

impl EthClient {
    /// Create a new client. `rpc_url` must be an HTTP(S) Alchemy or Infura endpoint.
    pub fn new(rpc_url: impl Into<String>, chain_id: u64) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            chain_id,
            http: Client::new(),
        }
    }

    // ── Core RPC primitive ────────────────────────────────────────────────────

    async fn rpc_call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };
        let resp: JsonRpcResponse = self
            .http
            .post(&self.rpc_url)
            .json(&req)
            .send()
            .await?
            .json()
            .await?;

        if let Some(err) = resp.error {
            anyhow::bail!("RPC error {}: {}", err.code, err.message);
        }
        resp.result
            .ok_or_else(|| anyhow::anyhow!("RPC returned null result for {}", method))
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Returns the latest block number.
    pub async fn block_number(&self) -> anyhow::Result<u64> {
        let result = self.rpc_call("eth_blockNumber", json!([])).await?;
        let hex = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("eth_blockNumber: expected hex string"))?;
        let n = u64::from_str_radix(hex.trim_start_matches("0x"), 16)?;
        debug!(block_number = n, "eth_blockNumber");
        Ok(n)
    }

    /// Returns the ETH balance of `address` at the latest block (in wei, decimal string).
    pub async fn eth_balance(&self, address: &str) -> anyhow::Result<String> {
        let result = self
            .rpc_call("eth_getBalance", json!([address, "latest"]))
            .await?;
        let hex = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("eth_getBalance: expected hex string"))?;
        Ok(hex_to_decimal_string(hex))
    }

    /// Executes a raw `eth_call` at the latest block. Returns hex-encoded return data.
    pub async fn call(&self, to: &str, data: &str) -> anyhow::Result<String> {
        let result = self
            .rpc_call(
                "eth_call",
                json!([{ "to": to, "data": data }, "latest"]),
            )
            .await?;
        result
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("eth_call: expected hex string"))
    }

    /// Returns the ERC-20 token balance of `owner` (decimal string, raw units).
    pub async fn erc20_balance(&self, token: &str, owner: &str) -> anyhow::Result<String> {
        // balanceOf(address) — pad address to 32 bytes
        let padded = format!("{:0>64}", owner.trim_start_matches("0x").to_lowercase());
        let data = format!("{}{}", SELECTOR_BALANCE_OF, padded);
        let ret = self.call(token, &data).await?;
        Ok(hex_to_decimal_string(&ret))
    }

    /// Returns the ERC-20 token decimals (uint8).
    pub async fn erc20_decimals(&self, token: &str) -> anyhow::Result<u8> {
        let ret = self.call(token, SELECTOR_DECIMALS).await?;
        let hex = ret.trim_start_matches("0x");
        // Result is a 32-byte ABI-encoded uint8
        let val = u8::try_from(
            u64::from_str_radix(&hex[hex.len().saturating_sub(2)..], 16)
                .unwrap_or(18),
        )
        .unwrap_or(18);
        Ok(val)
    }

    /// Batches multiple `eth_call` requests into a single JSON-RPC batch call.
    ///
    /// One HTTP round-trip regardless of how many calls are in `calls`.
    /// The order of results matches the order of inputs.
    pub async fn multicall(&self, calls: Vec<CallRequest>) -> anyhow::Result<Vec<CallResult>> {
        if calls.is_empty() {
            return Ok(vec![]);
        }

        // Build a JSON-RPC batch request
        let batch: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                json!({
                    "jsonrpc": "2.0",
                    "id": i,
                    "method": "eth_call",
                    "params": [{ "to": c.to, "data": c.data }, "latest"]
                })
            })
            .collect();

        let resp = self
            .http
            .post(&self.rpc_url)
            .json(&batch)
            .send()
            .await?
            .json::<Vec<Value>>()
            .await?;

        // Re-order by id (batch responses may arrive out of order)
        let n = calls.len();
        let mut results = vec![
            CallResult {
                success: false,
                return_data: "0x".to_string(),
            };
            n
        ];

        for item in resp {
            let id = item["id"].as_u64().unwrap_or(0) as usize;
            if id >= n {
                continue;
            }
            if item["error"].is_null() || item["error"].is_null() {
                if let Some(data) = item["result"].as_str() {
                    results[id] = CallResult {
                        success: true,
                        return_data: data.to_string(),
                    };
                }
            }
            // If "error" is present, success stays false
        }

        debug!(batch_size = n, "multicall completed");
        Ok(results)
    }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

/// Convert a `0x`-prefixed hex string to a decimal string (for token amounts).
fn hex_to_decimal_string(hex: &str) -> String {
    let clean = hex.trim_start_matches("0x");
    if clean.is_empty() {
        return "0".to_string();
    }
    // Use u128 for amounts that fit; fall back to big-decimal approximation for larger
    if clean.len() <= 32 {
        if let Ok(n) = u128::from_str_radix(clean, 16) {
            return n.to_string();
        }
    }
    // For larger values (U256), do manual base conversion
    let mut bytes = [0u8; 32];
    let len = clean.len().min(64);
    let padded_clean = format!("{:0>64}", &clean[clean.len() - len..]);
    for (i, chunk) in padded_clean.as_bytes().chunks(2).enumerate() {
        if i >= 32 {
            break;
        }
        let s = std::str::from_utf8(chunk).unwrap_or("00");
        bytes[i] = u8::from_str_radix(s, 16).unwrap_or(0);
    }
    // Convert big-endian bytes to decimal via iterative division
    decimal_from_be_bytes(&bytes)
}

/// Convert 32 big-endian bytes to a decimal string.
fn decimal_from_be_bytes(bytes: &[u8; 32]) -> String {
    let mut n = [0u32; 8];
    for (i, chunk) in bytes.chunks(4).enumerate() {
        n[i] = u32::from_be_bytes(chunk.try_into().unwrap_or([0; 4]));
    }
    // Check if it fits in u128 (most practical amounts do)
    if n[0] == 0 && n[1] == 0 && n[2] == 0 && n[3] == 0 {
        let lo = ((n[4] as u128) << 96)
            | ((n[5] as u128) << 64)
            | ((n[6] as u128) << 32)
            | (n[7] as u128);
        return lo.to_string();
    }
    // For truly huge U256 values (rare in practice), return hex-prefixed string
    format!("0x{}", bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>())
}

// ── Compatibility alias ───────────────────────────────────────────────────────

/// Alias kept for code that still references the old name.
pub type RpcClient = EthClient;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_decimal_zero() {
        assert_eq!(hex_to_decimal_string("0x"), "0");
        assert_eq!(hex_to_decimal_string("0x0"), "0");
        assert_eq!(hex_to_decimal_string("0x00"), "0");
    }

    #[test]
    fn hex_to_decimal_small() {
        assert_eq!(hex_to_decimal_string("0x1"), "1");
        assert_eq!(hex_to_decimal_string("0xff"), "255");
        assert_eq!(hex_to_decimal_string("0x3b9aca00"), "1000000000");
    }

    #[test]
    fn hex_to_decimal_u256_weth_price() {
        // 1 ETH = 1e18 wei
        assert_eq!(
            hex_to_decimal_string("0xde0b6b3a7640000"),
            "1000000000000000000"
        );
    }

    #[test]
    fn eth_client_new() {
        let client = EthClient::new("https://eth-mainnet.g.alchemy.com/v2/key", 1);
        assert_eq!(client.chain_id, 1);
        assert!(client.rpc_url.contains("alchemy"));
    }

    #[test]
    fn multicall_empty() {
        // Verify empty multicall returns empty vec synchronously-ish via block_on
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let client = EthClient::new("http://unused", 1);
        let result = rt.block_on(client.multicall(vec![])).unwrap();
        assert!(result.is_empty());
    }
}
