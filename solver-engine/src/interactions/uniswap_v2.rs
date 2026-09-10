use crate::models::solution::{CustomInteraction, Interaction};

// ── Uniswap V2 Router ─────────────────────────────────────────────────────────

/// Uniswap V2 Router address (mainnet)
pub const V2_ROUTER: &str = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D";
/// Uniswap Universal Router on Arbitrum (V2-compatible)
pub const ARBITRUM_V2_ROUTER: &str = "0x4752ba5dbc23f44d87826276bf6fd6b1c372ad24";

/// Returns the correct V2 router address for the given chain.
pub fn router_for_chain(chain_id: u64) -> &'static str {
    match chain_id {
        42161 => ARBITRUM_V2_ROUTER,
        _ => V2_ROUTER,
    }
}

/// `swapExactTokensForTokens(uint256,uint256,address[],address,uint256)` selector
const SWAP_EXACT_TOKENS_FOR_TOKENS: &str = "38ed1739";

/// `swap(uint256,uint256,address,bytes)` — called directly on a V2 pair
const PAIR_SWAP_SELECTOR: &str = "022c0d9f";

// ── ABI helpers ───────────────────────────────────────────────────────────────

/// Encode a uint256 as a 32-byte big-endian ABI word.
fn encode_uint256(value: u128) -> String {
    format!("{:064x}", value)
}

/// Encode an Ethereum address as a 32-byte ABI word (left-padded).
fn encode_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Encode a `swapExactTokensForTokens` call on the Uniswap V2 Router.
///
/// This is the standard high-level router call: the router handles token
/// transfers and calls `swap` on the pair internally.
///
/// # Arguments
/// - `router` — V2 router address (use `V2_ROUTER` for mainnet)
/// - `amount_in` — exact sell amount
/// - `amount_out_min` — minimum buy amount (slippage protection)
/// - `token_in` / `token_out` — the two tokens in the path
/// - `recipient` — address that receives buy tokens
/// - `deadline` — Unix timestamp after which the tx reverts
pub fn encode_router_swap(
    router: &str,
    amount_in: u128,
    amount_out_min: u128,
    token_in: &str,
    token_out: &str,
    recipient: &str,
    deadline: u64,
) -> Interaction {
    // swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin,
    //   address[] calldata path, address to, uint256 deadline)
    //
    // ABI encoding:
    //   [0]  amountIn       (uint256)
    //   [1]  amountOutMin   (uint256)
    //   [2]  offset to path (uint256) = 0xa0 (5 words before path data)
    //   [3]  to             (address padded)
    //   [4]  deadline       (uint256)
    //   [5]  path.length    (uint256) = 2
    //   [6]  path[0]        (address padded)
    //   [7]  path[1]        (address padded)

    let path_offset = encode_uint256(0xa0); // 5 * 32 = 160 = 0xa0
    let path_len = encode_uint256(2);

    let call_data = format!(
        "0x{}{}{}{}{}{}{}{}{}",
        SWAP_EXACT_TOKENS_FOR_TOKENS,
        encode_uint256(amount_in),
        encode_uint256(amount_out_min),
        path_offset,
        encode_address(recipient),
        encode_uint256(deadline as u128),
        path_len,
        encode_address(token_in),
        encode_address(token_out),
    );

    Interaction::Custom(CustomInteraction {
        internalize: false,
        target: router.to_string(),
        value: "0".to_string(),
        call_data,
    })
}

/// Encode a direct `swap(amount0Out, amount1Out, to, data)` call on a V2 pair.
///
/// Used when calling the pair directly (e.g. through the settlement contract).
/// The caller must have already transferred `amount_in` to the pair.
///
/// - `zero_for_one` — true if selling token0 (receiving token1)
/// - `amount_out` — the amount to receive
/// - `recipient` — address that receives the output tokens
pub fn encode_pair_swap(
    pair_address: &str,
    zero_for_one: bool,
    amount_out: u128,
    recipient: &str,
) -> Interaction {
    // swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data)
    let (amount0_out, amount1_out) = if zero_for_one {
        (0u128, amount_out)
    } else {
        (amount_out, 0u128)
    };

    // Empty bytes data (offset + length both 0x80, length = 0)
    let data_offset = encode_uint256(0x80); // 4 * 32 = 128 = 0x80
    let data_len = encode_uint256(0);

    let call_data = format!(
        "0x{}{}{}{}{}{}",
        PAIR_SWAP_SELECTOR,
        encode_uint256(amount0_out),
        encode_uint256(amount1_out),
        encode_address(recipient),
        data_offset,
        data_len,
    );

    Interaction::Custom(CustomInteraction {
        internalize: false,
        target: pair_address.to_string(),
        value: "0".to_string(),
        call_data,
    })
}

/// Legacy compatibility shim — same as `encode_router_swap` with `V2_ROUTER`
/// and a default deadline (30 minutes from a hardcoded block — use in tests only).
pub fn encode_swap(
    pool_address: &str,
    token_in: &str,
    token_out: &str,
    amount_in: u128,
    amount_out_min: u128,
    recipient: &str,
) -> Interaction {
    encode_router_swap(
        pool_address,
        amount_in,
        amount_out_min,
        token_in,
        token_out,
        recipient,
        u64::MAX, // no expiry
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_swap_has_correct_selector() {
        let interaction = encode_router_swap(
            V2_ROUTER,
            1_000_000,
            900_000,
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
            9_999_999_999,
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, V2_ROUTER);
            assert!(c.call_data.starts_with("0x38ed1739"));
            // 4-byte selector + 8 × 32-byte words = 4 + 256 = 260 bytes = 522 hex chars + "0x"
            assert_eq!(c.call_data.len(), 2 + 8 + 8 * 64);
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn pair_swap_zero_for_one() {
        let interaction = encode_pair_swap(
            "0xPair",
            true,
            500_000,
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
        );
        if let Interaction::Custom(c) = &interaction {
            assert!(c.call_data.starts_with("0x022c0d9f"));
            // amount0Out = 0 → first word all zeros
            assert!(c.call_data[10..74].chars().all(|c| c == '0'));
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn encode_uint256_pads_correctly() {
        assert_eq!(encode_uint256(1), "0000000000000000000000000000000000000000000000000000000000000001");
        assert_eq!(encode_uint256(0), "0000000000000000000000000000000000000000000000000000000000000000");
    }

    #[test]
    fn encode_address_pads_correctly() {
        let addr = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
        let encoded = encode_address(addr);
        assert_eq!(encoded.len(), 64);
        assert!(encoded.starts_with("000000000000000000000000"));
    }
}
