use crate::models::solution::{CustomInteraction, Interaction};

// ── Camelot V2 Router ────────────────────────────────────────────────────────

/// Camelot V2 Router address on Arbitrum
pub const CAMELOT_V2_ROUTER: &str = "0xc873fEcbd354f5A56E00E710B90EF4201db2448d";

/// `swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256,uint256,address[],address,address,uint256)`
/// Camelot uses a referrer parameter and supports fee-on-transfer tokens.
/// selector = keccak256("swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256,uint256,address[],address,address,uint256)")[:4]
const SWAP_EXACT_TOKENS_SELECTOR: &str = "5c11d795";

/// `swap(uint256,uint256,address,bytes)` — called directly on the pair contract
const PAIR_SWAP_SELECTOR: &str = "022c0d9f";

// ── ABI helpers ──────────────────────────────────────────────────────────────

fn encode_uint256(value: u128) -> String {
    format!("{:064x}", value)
}

fn encode_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Encode a `swapExactTokensForTokensSupportingFeeOnTransferTokens` call on the
/// Camelot V2 Router.
///
/// Camelot V2 router has an extra `referrer` parameter compared to Uniswap V2.
/// The referrer is set to the zero address (no referral).
///
/// # Arguments
/// - `router` — Camelot V2 router address
/// - `amount_in` — exact sell amount
/// - `amount_out_min` — minimum buy amount
/// - `token_in` / `token_out` — the swap path tokens
/// - `recipient` — receives output tokens
/// - `deadline` — Unix timestamp
#[allow(clippy::too_many_arguments)]
pub fn encode_router_swap(
    router: &str,
    amount_in: u128,
    amount_out_min: u128,
    token_in: &str,
    token_out: &str,
    recipient: &str,
    deadline: u64,
) -> Interaction {
    // swapExactTokensForTokensSupportingFeeOnTransferTokens(
    //   uint256 amountIn,
    //   uint256 amountOutMin,
    //   address[] calldata path,   -- offset
    //   address to,
    //   address referrer,          -- zero address (no referral)
    //   uint256 deadline
    // )
    //
    // ABI layout:
    //   [0] amountIn
    //   [1] amountOutMin
    //   [2] offset to path   = 0xc0 (6 * 32 = 192)
    //   [3] to (recipient)
    //   [4] referrer (zero address)
    //   [5] deadline
    //   [6] path.length = 2
    //   [7] path[0] = token_in
    //   [8] path[1] = token_out

    let path_offset = encode_uint256(0xc0); // 6 * 32
    let referrer = encode_uint256(0); // zero address
    let path_len = encode_uint256(2);

    let call_data = format!(
        "0x{}{}{}{}{}{}{}{}{}{}",
        SWAP_EXACT_TOKENS_SELECTOR,
        encode_uint256(amount_in),
        encode_uint256(amount_out_min),
        path_offset,
        encode_address(recipient),
        referrer,
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

/// Encode a direct `swap(amount0Out, amount1Out, to, data)` on a Camelot V2 pair.
///
/// Camelot V2 pairs use the same low-level swap interface as Uniswap V2 pairs.
/// The caller must have already transferred `amount_in` to the pair.
pub fn encode_pair_swap(
    pair_address: &str,
    zero_for_one: bool,
    amount_out: u128,
    recipient: &str,
) -> Interaction {
    let (amount0_out, amount1_out) = if zero_for_one {
        (0u128, amount_out)
    } else {
        (amount_out, 0u128)
    };

    let data_offset = encode_uint256(0x80); // 4 * 32
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_swap_has_correct_selector() {
        let interaction = encode_router_swap(
            CAMELOT_V2_ROUTER,
            1_000_000,
            900_000,
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
            9_999_999_999,
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, CAMELOT_V2_ROUTER);
            assert!(c.call_data.starts_with("0x5c11d795"));
            // selector(4B) + 9 words × 32B = 4 + 288 = 292 bytes = 586 hex chars + "0x"
            assert_eq!(c.call_data.len(), 2 + 8 + 9 * 64);
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn pair_swap_zero_for_one() {
        let interaction = encode_pair_swap("0xPair", true, 500_000, "0xRecipient");
        if let Interaction::Custom(c) = &interaction {
            assert!(c.call_data.starts_with("0x022c0d9f"));
            // amount0Out = 0 → first word after selector all zeros
            assert!(c.call_data[10..74].chars().all(|c| c == '0'));
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn pair_swap_one_for_zero() {
        let interaction = encode_pair_swap("0xPair", false, 500_000, "0xRecipient");
        if let Interaction::Custom(c) = &interaction {
            // amount1Out = 0 → second word after selector all zeros
            assert!(c.call_data[74..138].chars().all(|c| c == '0'));
        } else {
            panic!("Expected Custom interaction");
        }
    }
}
