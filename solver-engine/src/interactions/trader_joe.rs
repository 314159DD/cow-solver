//! Trader Joe V2.1 (Liquidity Book) swap calldata encoding.
//!
//! Encodes interactions for the LBRouter V2.1 on Arbitrum.
//! Router: 0xb4315e873dBcf96Ffd0acd8EA43f689D8c20fB30

use crate::models::solution::{CustomInteraction, Interaction};

// ── Router address ──────────────────────────────────────────────────────────

/// Trader Joe V2.1 LBRouter on Arbitrum
pub const TRADER_JOE_V21_ROUTER: &str = "0xb4315e873dBcf96Ffd0acd8EA43f689D8c20fB30";

/// `swapExactTokensForTokens(uint256,uint256,(uint256[],uint8[],address[]),address,uint256)`
///
/// The LBRouter V2.1 uses a `Path` struct containing:
/// - pairBinSteps: uint256[] — bin step for each hop
/// - versions: uint8[] — ILBRouter.Version (V1=0, V2=1, V2_1=2)
/// - tokenPath: address[] — sequence of tokens including intermediaries
///
/// selector = keccak256("swapExactTokensForTokens(uint256,uint256,(uint256[],uint8[],address[]),address,uint256)")[:4]
const SWAP_EXACT_TOKENS_SELECTOR: &str = "b3bc7021";

/// `swapTokensForExactTokens(uint256,uint256,(uint256[],uint8[],address[]),address,uint256)`
const _SWAP_TOKENS_FOR_EXACT_SELECTOR: &str = "20c3d5cb";

// ── ABI helpers ──────────────────────────────────────────────────────────────

fn encode_uint256(value: u128) -> String {
    format!("{:064x}", value)
}

fn encode_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Encode a `swapExactTokensForTokens` call on the Trader Joe V2.1 LBRouter
/// for a single-hop swap.
///
/// The Path struct is ABI-encoded as a tuple of dynamic arrays:
/// (uint256[] pairBinSteps, uint8[] versions, address[] tokenPath)
///
/// # Arguments
/// - `router` — LBRouter V2.1 address
/// - `amount_in` — exact sell amount
/// - `amount_out_min` — minimum buy amount
/// - `bin_step` — bin step for this pair
/// - `token_in` / `token_out` — the swap path tokens
/// - `recipient` — receives output tokens
/// - `deadline` — Unix timestamp
#[allow(clippy::too_many_arguments)]
pub fn encode_router_swap(
    router: &str,
    amount_in: u128,
    amount_out_min: u128,
    bin_step: u32,
    token_in: &str,
    token_out: &str,
    recipient: &str,
    deadline: u64,
) -> Interaction {
    // swapExactTokensForTokens(
    //   uint256 amountIn,                      [0]
    //   uint256 amountOutMin,                  [1]
    //   (uint256[], uint8[], address[]) path,  [2] → offset
    //   address to,                            [3]
    //   uint256 deadline                       [4]
    // )
    //
    // The Path tuple is a dynamic type, so word [2] is an offset to the Path data.
    // Path data layout:
    //   [P+0] offset to pairBinSteps array
    //   [P+1] offset to versions array
    //   [P+2] offset to tokenPath array
    //   [P+3] pairBinSteps.length = 1
    //   [P+4] pairBinSteps[0] = bin_step
    //   [P+5] versions.length = 1
    //   [P+6] versions[0] = 2 (V2_1)
    //   [P+7] tokenPath.length = 2
    //   [P+8] tokenPath[0] = token_in
    //   [P+9] tokenPath[1] = token_out
    //
    // Static header: 5 words (amountIn, amountOutMin, pathOffset, to, deadline)
    // pathOffset = 5 * 32 = 0xa0

    let path_offset = encode_uint256(0xa0); // 5 * 32

    // Path tuple: 3 offsets + 3 arrays
    // offsets: pairBinSteps at 3*32=0x60, versions at 0x60+2*32=0xc0, tokenPath at 0xc0+2*32=0x120
    // Wait — let's compute properly:
    // Tuple head: 3 words (offsets for 3 dynamic arrays)
    // pairBinSteps: offset from tuple start = 3*32 = 96 = 0x60
    // versions: offset = 96 + 32 + 32 = 160 = 0xa0 (length word + 1 element)
    // tokenPath: offset = 160 + 32 + 32 = 224 = 0xe0 (length word + 1 element, padded)
    let pair_bin_steps_offset = encode_uint256(0x60);
    let versions_offset = encode_uint256(0xa0);
    let token_path_offset = encode_uint256(0xe0);

    // pairBinSteps: [1, bin_step]
    let bin_steps_len = encode_uint256(1);
    let bin_step_val = encode_uint256(bin_step as u128);

    // versions: [1, 2] (V2_1 = 2)
    let versions_len = encode_uint256(1);
    let version_v21 = encode_uint256(2); // ILBRouter.Version.V2_1

    // tokenPath: [2, token_in, token_out]
    let token_path_len = encode_uint256(2);

    let call_data = format!(
        "0x{selector}{amount_in}{amount_out_min}{path_offset}{to}{deadline}\
         {pair_bin_steps_offset}{versions_offset}{token_path_offset}\
         {bin_steps_len}{bin_step_val}\
         {versions_len}{version_v21}\
         {token_path_len}{token_in}{token_out}",
        selector = SWAP_EXACT_TOKENS_SELECTOR,
        amount_in = encode_uint256(amount_in),
        amount_out_min = encode_uint256(amount_out_min),
        path_offset = path_offset,
        to = encode_address(recipient),
        deadline = encode_uint256(deadline as u128),
        pair_bin_steps_offset = pair_bin_steps_offset,
        versions_offset = versions_offset,
        token_path_offset = token_path_offset,
        bin_steps_len = bin_steps_len,
        bin_step_val = bin_step_val,
        versions_len = versions_len,
        version_v21 = version_v21,
        token_path_len = token_path_len,
        token_in = encode_address(token_in),
        token_out = encode_address(token_out),
    );

    Interaction::Custom(CustomInteraction {
        internalize: false,
        target: router.to_string(),
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
            TRADER_JOE_V21_ROUTER,
            1_000_000,
            900_000,
            15,
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1", // WETH
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831", // USDC
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41", // Settlement
            9_999_999_999,
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, TRADER_JOE_V21_ROUTER);
            assert!(c.call_data.starts_with("0xb3bc7021"));
            // Should have selector (4B) + 5 header words + 3 offset words + 7 data words
            // = 4 + 15 * 32 = 484 bytes = 968 hex chars + "0x"
            assert_eq!(c.call_data.len(), 2 + 8 + 15 * 64);
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn router_swap_value_is_zero() {
        let interaction = encode_router_swap(
            TRADER_JOE_V21_ROUTER,
            1_000,
            900,
            20,
            "0xAAA",
            "0xBBB",
            "0xCCC",
            12345,
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.value, "0");
            assert!(!c.internalize);
        } else {
            panic!("Expected Custom interaction");
        }
    }
}
