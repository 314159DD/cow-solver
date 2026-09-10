//! Settlement ABI encoding for the CoW Protocol GPv2Settlement contract.
//!
//! Encodes the `settle()` function call from our Solution struct into
//! raw calldata that can be simulated via `eth_call` against the
//! settlement contract. This is exactly what the CoW driver does
//! before accepting a solution.
//!
//! Settlement contract: `0x9008D19f58AAbD9eD0D60971565AA8510560ab41`
//!
//! ```solidity
//! function settle(
//!     IERC20[] calldata tokens,
//!     uint256[] calldata clearingPrices,
//!     GPv2Trade.Data[] calldata trades,
//!     GPv2Interaction.Data[][3] calldata interactions
//! ) external;
//! ```

use std::collections::HashMap;

use tracing::{debug, warn};

use crate::models::order::{Order, OrderKind};
use crate::models::solution::{Interaction, Solution, Trade};

/// The settlement contract address (same on all EVM chains).
pub const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

/// settle() function selector: keccak256("settle(address[],uint256[],(uint256,uint256,address,uint256,uint256,uint32,bytes32,uint256,uint256,uint256,bytes)[],(address,uint256,bytes)[][3])")
/// The actual selector from the GPv2Settlement contract:
const SETTLE_SELECTOR: [u8; 4] = [0x13, 0xd7, 0x9a, 0x2b];

/// Result of settlement encoding.
pub struct EncodedSettlement {
    /// Full calldata (selector + ABI-encoded params)
    pub calldata: Vec<u8>,
    /// Hex-encoded calldata string (0x-prefixed)
    pub calldata_hex: String,
    /// Number of tokens in the settlement
    pub token_count: usize,
    /// Number of trades in the settlement
    pub trade_count: usize,
    /// Number of interactions in the settlement
    pub interaction_count: usize,
}

/// Encode a Solution into settlement calldata for eth_call simulation.
///
/// `orders` provides the original order data needed for trade encoding
/// (sell/buy tokens, amounts, signatures, validity).
///
/// Returns None if the solution has no trades or prices.
pub fn encode_settlement(solution: &Solution, orders: &[Order]) -> Option<EncodedSettlement> {
    if solution.trades.is_empty() || solution.prices.is_empty() {
        return None;
    }

    // Step 1: Collect all unique tokens from prices map (sorted for determinism)
    let mut tokens: Vec<String> = solution.prices.keys().cloned().collect();
    tokens.sort();

    // Build token → index map
    let token_index: HashMap<&str, usize> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();

    // Step 2: Collect clearing prices in token order
    let clearing_prices: Vec<[u8; 32]> = tokens
        .iter()
        .map(|t| {
            let price_str = solution.prices.get(t).cloned().unwrap_or_default();
            let val = price_str.parse::<u128>().unwrap_or(0);
            u256_bytes(val as u128)
        })
        .collect();

    // Step 3: Encode trades
    // GPv2Trade.Data struct:
    //   uint256 sellTokenIndex;
    //   uint256 buyTokenIndex;
    //   address receiver;
    //   uint256 sellAmount;
    //   uint256 buyAmount;
    //   uint32  validTo;
    //   bytes32 appData;
    //   uint256 feeAmount;
    //   uint256 flags;
    //   uint256 executedAmount;
    //   bytes   signature;
    // Build order lookup: uid → Order
    let order_map: HashMap<&str, &Order> = orders.iter()
        .map(|o| (o.uid.as_str(), o))
        .collect();

    let mut trade_data: Vec<Vec<u8>> = Vec::new();
    for trade in &solution.trades {
        let Trade::Fulfillment(f) = trade;
        let executed_amount: u128 = f.executed_amount.parse().unwrap_or(0);

        if let Some(order) = order_map.get(f.order.as_str()) {
            let sell_idx = token_index.get(order.sell_token.as_str()).copied().unwrap_or(0);
            let buy_idx = token_index.get(order.buy_token.as_str()).copied().unwrap_or(0);
            let trade_bytes = encode_trade(order, sell_idx, buy_idx, executed_amount);
            trade_data.push(trade_bytes);
        } else {
            warn!(order_uid = %f.order, "Trade references unknown order — using placeholder");
            let trade_bytes = encode_trade_placeholder(&f.order, executed_amount);
            trade_data.push(trade_bytes);
        }
    }

    // Step 4: Encode interactions
    // interactions is a fixed-size array of 3 dynamic arrays:
    //   [0] = pre-interactions (before settlement)
    //   [1] = intra-interactions (during settlement — our swap calls)
    //   [2] = post-interactions (after settlement)
    let mut pre_interactions: Vec<Vec<u8>> = Vec::new();
    let mut intra_interactions: Vec<Vec<u8>> = Vec::new();
    let mut post_interactions: Vec<Vec<u8>> = Vec::new();

    for interaction in &solution.interactions {
        match interaction {
            Interaction::Custom(ci) => {
                let encoded = encode_interaction(&ci.target, &ci.call_data, &ci.value);
                intra_interactions.push(encoded);
            }
            Interaction::Liquidity(_) => {
                // Liquidity interactions are logical — skip for calldata encoding
            }
        }
    }

    let interaction_count = pre_interactions.len() + intra_interactions.len() + post_interactions.len();

    // Step 5: ABI-encode the full settle() call
    let mut calldata = Vec::new();
    calldata.extend_from_slice(&SETTLE_SELECTOR);

    // ABI encoding uses dynamic offsets for dynamic types.
    // settle() has 4 params, all dynamic: address[], uint256[], Trade[], Interaction[][3]
    // Head: 4 offsets × 32 bytes = 128 bytes
    let head_size = 4 * 32;

    // We'll build the tail sections and compute offsets
    let tokens_encoded = encode_address_array(&tokens);
    let prices_encoded = encode_u256_array(&clearing_prices);

    // Encode trades as dynamic array of tuples
    let trades_encoded = encode_trade_array(&trade_data);

    // Offset pointers (relative to start of params, not including selector)
    let offset_tokens = head_size;
    let offset_prices = offset_tokens + tokens_encoded.len();
    let offset_trades = offset_prices + prices_encoded.len();
    let offset_interactions = offset_trades + trades_encoded.len();

    // Encode the 3-element fixed array of interaction arrays
    let interactions_encoded = encode_interaction_arrays(
        &pre_interactions,
        &intra_interactions,
        &post_interactions,
    );

    // Write offsets
    calldata.extend_from_slice(&u256_bytes(offset_tokens as u128));
    calldata.extend_from_slice(&u256_bytes(offset_prices as u128));
    calldata.extend_from_slice(&u256_bytes(offset_trades as u128));
    calldata.extend_from_slice(&u256_bytes(offset_interactions as u128));

    // Write data sections
    calldata.extend_from_slice(&tokens_encoded);
    calldata.extend_from_slice(&prices_encoded);
    calldata.extend_from_slice(&trades_encoded);
    calldata.extend_from_slice(&interactions_encoded);

    let calldata_hex = format!("0x{}", hex::encode(&calldata));

    debug!(
        tokens = tokens.len(),
        trades = solution.trades.len(),
        interactions = interaction_count,
        calldata_len = calldata.len(),
        "Encoded settlement calldata"
    );

    Some(EncodedSettlement {
        calldata,
        calldata_hex,
        token_count: tokens.len(),
        trade_count: solution.trades.len(),
        interaction_count,
    })
}

/// Simulate a settlement via eth_call against the RPC.
///
/// This is the gold standard validation: if eth_call succeeds, the
/// settlement will succeed on-chain. If it reverts, so will the real tx.
///
/// Returns (success, gas_used, revert_reason).
pub async fn simulate_settlement(
    calldata_hex: &str,
    rpc_url: &str,
    block: Option<u64>,
) -> (bool, u64, Option<String>) {
    let client = reqwest::Client::new();

    let block_param = block
        .map(|b| format!("0x{:x}", b))
        .unwrap_or_else(|| "latest".to_string());

    // First: eth_call to check if it reverts
    let call_body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{
            "to": SETTLEMENT_CONTRACT,
            "data": calldata_hex,
            "from": SETTLEMENT_CONTRACT, // Simulate as if settlement is calling itself
        }, block_param],
        "id": 1
    });

    match client
        .post(rpc_url)
        .json(&call_body)
        .timeout(std::time::Duration::from_millis(3000))
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(err) = body.get("error") {
                    let reason = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("eth_call reverted")
                        .to_string();
                    return (false, 0, Some(reason));
                }
                // Success — try to estimate gas
                let gas_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "eth_estimateGas",
                    "params": [{
                        "to": SETTLEMENT_CONTRACT,
                        "data": calldata_hex,
                        "from": SETTLEMENT_CONTRACT,
                    }],
                    "id": 2
                });

                let gas_used = match client
                    .post(rpc_url)
                    .json(&gas_body)
                    .timeout(std::time::Duration::from_millis(2000))
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            body.get("result")
                                .and_then(|r| r.as_str())
                                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                                .unwrap_or(200_000)
                        } else {
                            200_000
                        }
                    }
                    Err(_) => 200_000, // Default gas estimate
                };

                return (true, gas_used, None);
            }
            (false, 0, Some("Failed to parse RPC response".to_string()))
        }
        Err(e) => (false, 0, Some(format!("RPC error: {}", e))),
    }
}

// ── ABI encoding helpers ────────────────────────────────────────────────────

/// Encode a u128 value as a 32-byte big-endian U256.
fn u256_bytes(val: u128) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[16..32].copy_from_slice(&val.to_be_bytes());
    bytes
}

/// Encode an address (20 bytes, left-padded to 32).
fn encode_address(addr: &str) -> [u8; 32] {
    let addr = addr.trim_start_matches("0x");
    let mut bytes = [0u8; 32];
    if let Ok(decoded) = hex::decode(addr) {
        let start = 32 - decoded.len().min(20);
        bytes[start..start + decoded.len().min(20)].copy_from_slice(&decoded[..decoded.len().min(20)]);
    }
    bytes
}

/// Encode an array of addresses: [length, addr0, addr1, ...]
fn encode_address_array(addrs: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u256_bytes(addrs.len() as u128));
    for addr in addrs {
        out.extend_from_slice(&encode_address(addr));
    }
    out
}

/// Encode an array of U256 values: [length, val0, val1, ...]
fn encode_u256_array(vals: &[[u8; 32]]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u256_bytes(vals.len() as u128));
    for val in vals {
        out.extend_from_slice(val);
    }
    out
}

/// Encode an empty dynamic array: just the length (0).
fn encode_empty_dynamic_array() -> Vec<u8> {
    u256_bytes(0).to_vec()
}

/// Encode a single interaction: (address target, uint256 value, bytes calldata)
fn encode_interaction(target: &str, calldata: &str, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&encode_address(target));
    let val: u128 = value.parse().unwrap_or(0);
    out.extend_from_slice(&u256_bytes(val));
    // calldata offset (relative to start of this tuple) = 96 bytes (3 × 32)
    out.extend_from_slice(&u256_bytes(96));
    // calldata: length + padded data
    let cd = calldata.trim_start_matches("0x");
    let cd_bytes = hex::decode(cd).unwrap_or_default();
    out.extend_from_slice(&u256_bytes(cd_bytes.len() as u128));
    out.extend_from_slice(&cd_bytes);
    // Pad to 32-byte boundary
    let padding = (32 - cd_bytes.len() % 32) % 32;
    out.extend(std::iter::repeat(0u8).take(padding));
    out
}

/// Encode the 3-element fixed array of interaction arrays.
fn encode_interaction_arrays(
    pre: &[Vec<u8>],
    intra: &[Vec<u8>],
    post: &[Vec<u8>],
) -> Vec<u8> {
    let mut out = Vec::new();
    // 3 offsets for the 3 sub-arrays
    let head_size = 3 * 32;

    let pre_encoded = encode_interaction_list(pre);
    let intra_encoded = encode_interaction_list(intra);

    let offset_pre = head_size;
    let offset_intra = offset_pre + pre_encoded.len();
    let offset_post = offset_intra + intra_encoded.len();

    out.extend_from_slice(&u256_bytes(offset_pre as u128));
    out.extend_from_slice(&u256_bytes(offset_intra as u128));
    out.extend_from_slice(&u256_bytes(offset_post as u128));

    out.extend_from_slice(&pre_encoded);
    out.extend_from_slice(&intra_encoded);
    out.extend_from_slice(&encode_interaction_list(post));

    out
}

/// Encode a list of interactions as a dynamic array.
fn encode_interaction_list(interactions: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u256_bytes(interactions.len() as u128));

    if interactions.is_empty() {
        return out;
    }

    // Offsets for each interaction (relative to start of data section)
    let offsets_size = interactions.len() * 32;
    let mut current_offset = offsets_size;

    for interaction in interactions {
        out.extend_from_slice(&u256_bytes(current_offset as u128));
        current_offset += interaction.len();
    }

    for interaction in interactions {
        out.extend_from_slice(interaction);
    }

    out
}

/// Encode a trade array: [length, offset0, offset1, ..., trade0, trade1, ...]
fn encode_trade_array(trades: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u256_bytes(trades.len() as u128));

    if trades.is_empty() {
        return out;
    }

    // Each trade is a dynamic tuple → need offsets
    let offsets_size = trades.len() * 32;
    let mut current_offset = offsets_size;
    for trade in trades {
        out.extend_from_slice(&u256_bytes(current_offset as u128));
        current_offset += trade.len();
    }
    for trade in trades {
        out.extend_from_slice(trade);
    }
    out
}

/// Encode a real trade tuple from order data.
///
/// GPv2Trade.Data:
///   uint256 sellTokenIndex, uint256 buyTokenIndex, address receiver,
///   uint256 sellAmount, uint256 buyAmount, uint32 validTo,
///   bytes32 appData, uint256 feeAmount, uint256 flags,
///   uint256 executedAmount, bytes signature
fn encode_trade(order: &Order, sell_token_idx: usize, buy_token_idx: usize, executed_amount: u128) -> Vec<u8> {
    let mut out = Vec::new();

    // sellTokenIndex
    out.extend_from_slice(&u256_bytes(sell_token_idx as u128));
    // buyTokenIndex
    out.extend_from_slice(&u256_bytes(buy_token_idx as u128));
    // receiver (zero = order owner receives tokens)
    let receiver = order.receiver.as_deref().unwrap_or("0x0000000000000000000000000000000000000000");
    out.extend_from_slice(&encode_address(receiver));
    // sellAmount
    let sell_amount: u128 = order.sell_amount.parse().unwrap_or(0);
    out.extend_from_slice(&u256_bytes(sell_amount));
    // buyAmount
    let buy_amount: u128 = order.buy_amount.parse().unwrap_or(0);
    out.extend_from_slice(&u256_bytes(buy_amount));
    // validTo (u32, padded to u256)
    let valid_to = order.valid_to.unwrap_or(u32::MAX as u64) as u128;
    out.extend_from_slice(&u256_bytes(valid_to));
    // appData (bytes32)
    let app_data = order.app_data.as_deref().unwrap_or("0x0000000000000000000000000000000000000000000000000000000000000000");
    let app_data_hex = app_data.trim_start_matches("0x");
    let mut app_data_bytes = [0u8; 32];
    if let Ok(decoded) = hex::decode(app_data_hex) {
        let len = decoded.len().min(32);
        app_data_bytes[..len].copy_from_slice(&decoded[..len]);
    }
    out.extend_from_slice(&app_data_bytes);
    // feeAmount
    let fee_amount: u128 = order.fee_amount.parse().unwrap_or(0);
    out.extend_from_slice(&u256_bytes(fee_amount));
    // flags: encodes kind (bit 0), partially_fillable (bit 1), sell_token_balance (bits 2-3), buy_token_balance (bit 4), signing_scheme (bits 5-6)
    let kind_flag: u128 = match order.kind { OrderKind::Sell => 0, OrderKind::Buy => 1 };
    let partial_flag: u128 = if order.partially_fillable { 2 } else { 0 };
    let signing_flag: u128 = match order.signing_scheme.as_deref() {
        Some("ethsign") => 1 << 5,
        Some("presign") => 2 << 5,
        Some("eip1271") => 3 << 5,
        _ => 0, // eip712 = 0
    };
    let flags = kind_flag | partial_flag | signing_flag;
    out.extend_from_slice(&u256_bytes(flags));
    // executedAmount
    out.extend_from_slice(&u256_bytes(executed_amount));
    // signature: dynamic bytes field
    // Offset to signature data = 11 * 32 = 352 bytes from start of tuple
    out.extend_from_slice(&u256_bytes(11 * 32));
    // Signature data: length + bytes
    let sig_hex = order.signature.as_deref().unwrap_or("0x").trim_start_matches("0x");
    let sig_bytes = hex::decode(sig_hex).unwrap_or_default();
    out.extend_from_slice(&u256_bytes(sig_bytes.len() as u128));
    out.extend_from_slice(&sig_bytes);
    // Pad signature to 32-byte boundary
    let padding = (32 - sig_bytes.len() % 32) % 32;
    out.extend(std::iter::repeat(0u8).take(padding));

    out
}

/// Encode a placeholder trade tuple (fallback when order data is missing).
fn encode_trade_placeholder(order_uid: &str, executed_amount: u128) -> Vec<u8> {
    // Minimal trade encoding — just enough for the settlement contract
    // to not revert on ABI decoding. The actual trade validation
    // requires signatures which we don't have in simulation.
    let mut out = Vec::new();
    // sellTokenIndex, buyTokenIndex = 0 (placeholder)
    out.extend_from_slice(&u256_bytes(0));
    out.extend_from_slice(&u256_bytes(0));
    // receiver = zero address
    out.extend_from_slice(&[0u8; 32]);
    // sellAmount, buyAmount = 0
    out.extend_from_slice(&u256_bytes(0));
    out.extend_from_slice(&u256_bytes(0));
    // validTo = max
    out.extend_from_slice(&u256_bytes(u32::MAX as u128));
    // appData = 0
    out.extend_from_slice(&[0u8; 32]);
    // feeAmount = 0
    out.extend_from_slice(&u256_bytes(0));
    // flags = 0
    out.extend_from_slice(&u256_bytes(0));
    // executedAmount
    out.extend_from_slice(&u256_bytes(executed_amount));
    // signature offset + empty signature
    out.extend_from_slice(&u256_bytes(11 * 32)); // offset to signature
    out.extend_from_slice(&u256_bytes(0)); // signature length
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::solution::{CustomInteraction, Solution};

    #[test]
    fn encode_settlement_empty_solution_returns_none() {
        let sol = Solution::new(0);
        assert!(encode_settlement(&sol, &[]).is_none());
    }

    #[test]
    fn u256_bytes_encodes_correctly() {
        let bytes = u256_bytes(1);
        assert_eq!(bytes[31], 1);
        assert_eq!(bytes[0], 0);

        let bytes = u256_bytes(256);
        assert_eq!(bytes[30], 1);
        assert_eq!(bytes[31], 0);
    }

    #[test]
    fn encode_address_pads_correctly() {
        let addr = encode_address("0x9008D19f58AAbD9eD0D60971565AA8510560ab41");
        // Address should be in bytes 12-31 (left-padded with zeros)
        assert_eq!(addr[0..12], [0u8; 12]);
        assert_ne!(addr[12..32], [0u8; 20]);
    }

    #[test]
    fn encode_settlement_with_interactions() {
        let mut sol = Solution::new(1);
        sol.prices.insert("0xtoken_a".into(), "1000000".into());
        sol.prices.insert("0xtoken_b".into(), "2000000".into());
        sol.trades.push(crate::models::solution::Trade::Fulfillment(
            crate::models::solution::FulfillmentTrade {
                order: "order1".into(),
                executed_amount: "1000".into(),
                fee: String::new(),
            },
        ));
        sol.interactions.push(Interaction::Custom(CustomInteraction {
            internalize: false,
            target: "0x1234567890abcdef1234567890abcdef12345678".into(),
            call_data: "0xabcdef01".into(),
            value: "0".into(),
        }));

        let encoded = encode_settlement(&sol, &[]);
        assert!(encoded.is_some());
        let encoded = encoded.unwrap();
        assert!(encoded.calldata_hex.starts_with("0x"));
        assert_eq!(encoded.token_count, 2);
        assert_eq!(encoded.trade_count, 1);
        assert_eq!(encoded.interaction_count, 1);
    }
}
