use crate::models::solution::{CustomInteraction, Interaction};

// ── Uniswap V3 Router ─────────────────────────────────────────────────────────

/// Uniswap V3 SwapRouter address (mainnet)
pub const V3_ROUTER: &str = "0xE592427A0AEce92De3Edee1F18E0157C05861564";
/// Uniswap V3 SwapRouter address (Arbitrum — same contract, deployed at same address)
pub const ARBITRUM_V3_ROUTER: &str = "0xE592427A0AEce92De3Edee1F18E0157C05861564";

/// Returns the correct V3 router address for the given chain.
pub fn router_for_chain(chain_id: u64) -> &'static str {
    match chain_id {
        42161 => ARBITRUM_V3_ROUTER,
        _ => V3_ROUTER,
    }
}

/// `exactInputSingle((tokenIn,tokenOut,fee,recipient,deadline,amountIn,amountOutMinimum,sqrtPriceLimitX96))`
/// Function selector: keccak256("exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))")
const EXACT_INPUT_SINGLE_SELECTOR: &str = "414bf389";

/// `exactInput((bytes path, address recipient, uint256 deadline, uint256 amountIn, uint256 amountOutMinimum))`
const EXACT_INPUT_SELECTOR: &str = "c04b8d59";

// ── ABI helpers ───────────────────────────────────────────────────────────────

fn encode_uint256(value: u128) -> String {
    format!("{:064x}", value)
}

fn encode_uint24(value: u32) -> String {
    format!("{:064x}", value & 0xFF_FFFF)
}

fn encode_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

/// Encode a V3 path as bytes: [token_in (20 bytes)] ++ [fee (3 bytes)] ++ [token_out (20 bytes)]
/// Returns the raw hex (no 0x prefix) of the 43-byte path.
fn encode_path_bytes(token_in: &str, fee: u32, token_out: &str) -> String {
    let t_in = token_in.trim_start_matches("0x").to_lowercase();
    let fee_hex = format!("{:06x}", fee & 0xFF_FFFF);
    let t_out = token_out.trim_start_matches("0x").to_lowercase();
    format!("{t_in}{fee_hex}{t_out}")
}

/// ABI-encode a `bytes` value: offset (32B) + length (32B) + padded data.
fn abi_encode_bytes(hex_data: &str) -> String {
    let byte_len = hex_data.len() / 2;
    let padded_len = byte_len.div_ceil(32) * 32 * 2; // pad to 32-byte boundary
    let padded = format!("{:0<width$}", hex_data, width = padded_len);
    format!(
        "{}{}{}",
        encode_uint256(byte_len as u128),
        padded,
        ""
    )
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Encode an `exactInputSingle` call on the Uniswap V3 SwapRouter.
///
/// This is the standard single-hop swap. The ExactInputSingleParams struct
/// is ABI-encoded as a tuple (all fields concatenated as 32-byte words).
///
/// # Arguments
/// - `router` — V3 router address (use `V3_ROUTER` for mainnet)
/// - `token_in` / `token_out` — the two tokens
/// - `fee` — pool fee tier (e.g. 3000 for 0.3%)
/// - `recipient` — receives output tokens
/// - `deadline` — Unix timestamp
/// - `amount_in` — exact input amount
/// - `amount_out_min` — minimum output (slippage protection)
/// - `sqrt_price_limit_x96` — 0 means no price limit
#[allow(clippy::too_many_arguments)]
pub fn encode_exact_input_single(
    router: &str,
    token_in: &str,
    token_out: &str,
    fee: u32,
    recipient: &str,
    deadline: u64,
    amount_in: u128,
    amount_out_min: u128,
    sqrt_price_limit_x96: u128,
) -> Interaction {
    // ExactInputSingleParams struct ABI encoding (each field = 32 bytes):
    // tokenIn, tokenOut, fee (uint24), recipient, deadline, amountIn,
    // amountOutMinimum, sqrtPriceLimitX96
    let call_data = format!(
        "0x{}{}{}{}{}{}{}{}{}",
        EXACT_INPUT_SINGLE_SELECTOR,
        encode_address(token_in),
        encode_address(token_out),
        encode_uint24(fee),
        encode_address(recipient),
        encode_uint256(deadline as u128),
        encode_uint256(amount_in),
        encode_uint256(amount_out_min),
        encode_uint256(sqrt_price_limit_x96),
    );

    Interaction::Custom(CustomInteraction {
        internalize: false,
        target: router.to_string(),
        value: "0".to_string(),
        call_data,
    })
}

/// Encode an `exactInput` (multi-hop) call on the Uniswap V3 SwapRouter.
///
/// For a single-hop swap, the path is: token_in ++ fee ++ token_out (43 bytes).
/// This function encodes a single-hop path.
#[allow(clippy::too_many_arguments)]
pub fn encode_exact_input(
    router: &str,
    token_in: &str,
    fee: u32,
    token_out: &str,
    recipient: &str,
    deadline: u64,
    amount_in: u128,
    amount_out_min: u128,
) -> Interaction {
    // ExactInputParams:
    // bytes path        — dynamic, offset = 0xa0 (5 words)
    // address recipient
    // uint256 deadline
    // uint256 amountIn
    // uint256 amountOutMinimum
    // bytes data (path) — 43 bytes = token_in(20) + fee(3) + token_out(20)

    let path_hex = encode_path_bytes(token_in, fee, token_out);
    let path_offset = encode_uint256(0xa0); // 5 * 32

    let call_data = format!(
        "0x{}{}{}{}{}{}{}",
        EXACT_INPUT_SELECTOR,
        path_offset,
        encode_address(recipient),
        encode_uint256(deadline as u128),
        encode_uint256(amount_in),
        encode_uint256(amount_out_min),
        abi_encode_bytes(&path_hex),
    );

    Interaction::Custom(CustomInteraction {
        internalize: false,
        target: router.to_string(),
        value: "0".to_string(),
        call_data,
    })
}

/// Legacy compatibility shim.
#[allow(clippy::too_many_arguments)]
pub fn encode_swap(
    router_address: &str,
    token_in: &str,
    token_out: &str,
    fee: u32,
    amount_in: u128,
    amount_out_min: u128,
    recipient: &str,
    sqrt_price_limit_x96: u128,
) -> Interaction {
    encode_exact_input_single(
        router_address,
        token_in,
        token_out,
        fee,
        recipient,
        u64::MAX,
        amount_in,
        amount_out_min,
        sqrt_price_limit_x96,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_input_single_has_correct_selector() {
        let interaction = encode_exact_input_single(
            V3_ROUTER,
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            3_000,
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
            9_999_999_999,
            1_000_000_000_000_000_000,
            900_000_000,
            0,
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, V3_ROUTER);
            assert!(c.call_data.starts_with("0x414bf389"));
            // selector(4B) + 8 params × 32B = 4 + 256 = 260 bytes = 522 hex chars + 2 for "0x"
            assert_eq!(c.call_data.len(), 2 + 8 + 8 * 64);
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn exact_input_single_zero_price_limit() {
        let interaction = encode_exact_input_single(
            V3_ROUTER,
            "0xaaaa",
            "0xbbbb",
            500,
            "0xcccc",
            0,
            1000,
            900,
            0,
        );
        if let Interaction::Custom(c) = &interaction {
            // sqrtPriceLimitX96 = 0 → last 64 hex chars all zeros
            let data = &c.call_data[2..];
            let last_word = &data[data.len() - 64..];
            assert!(last_word.chars().all(|c| c == '0'));
        } else {
            panic!();
        }
    }

    #[test]
    fn encode_path_bytes_correct_length() {
        let path = encode_path_bytes("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", 3000, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        // 20 bytes + 3 bytes + 20 bytes = 43 bytes = 86 hex chars
        assert_eq!(path.len(), 86);
    }

    #[test]
    fn encode_uint24_stays_within_3_bytes() {
        // uint24 max = 16_777_215
        let enc = encode_uint24(16_777_215);
        // Should be 64 hex chars, last 6 chars = "ffffff"
        assert_eq!(&enc[58..], "ffffff");
        // No bits above bit 23 set
        let enc2 = encode_uint24(0xFF_FF_FF_FF);
        assert_eq!(&enc2[58..], "ffffff");
    }
}
