//! SushiSwap swap calldata encoding.
//!
//! SushiSwap is a Uniswap V2 fork with identical router ABI.
//! We reuse the Uniswap V2 interaction encoding with SushiSwap router addresses.

use crate::interactions::uniswap_v2;
use crate::models::solution::Interaction;

// ── Router addresses ────────────────────────────────────────────────────────

/// SushiSwap Router on Ethereum mainnet
pub const MAINNET_ROUTER: &str = "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F";

/// SushiSwap Router on Arbitrum
pub const ARBITRUM_ROUTER: &str = "0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506";

/// Return the SushiSwap router for a given chain.
pub fn router_for_chain(chain_id: u64) -> &'static str {
    match chain_id {
        42161 => ARBITRUM_ROUTER,
        _ => MAINNET_ROUTER,
    }
}

// ── Swap encoding ───────────────────────────────────────────────────────────

/// Encode a SushiSwap `swapExactTokensForTokens` call.
///
/// Identical to Uniswap V2 router encoding but targets the SushiSwap router.
pub fn encode_router_swap(
    router: &str,
    amount_in: u128,
    amount_out_min: u128,
    token_in: &str,
    token_out: &str,
    recipient: &str,
    deadline: u64,
) -> Interaction {
    uniswap_v2::encode_router_swap(
        router,
        amount_in,
        amount_out_min,
        token_in,
        token_out,
        recipient,
        deadline,
    )
}

/// Encode a direct pair swap on a SushiSwap pair contract.
///
/// Identical to Uniswap V2 pair swap since SushiSwap uses the same pair ABI.
pub fn encode_pair_swap(
    pair_address: &str,
    zero_for_one: bool,
    amount_out: u128,
    recipient: &str,
) -> Interaction {
    uniswap_v2::encode_pair_swap(pair_address, zero_for_one, amount_out, recipient)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::solution::Interaction;

    #[test]
    fn router_swap_has_correct_selector() {
        let interaction = encode_router_swap(
            ARBITRUM_ROUTER,
            1_000_000,
            900_000,
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
            9_999_999_999,
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, ARBITRUM_ROUTER);
            // Same selector as Uniswap V2: swapExactTokensForTokens
            assert!(c.call_data.starts_with("0x38ed1739"));
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn router_for_chain_returns_correct_addresses() {
        assert_eq!(router_for_chain(1), MAINNET_ROUTER);
        assert_eq!(router_for_chain(42161), ARBITRUM_ROUTER);
    }
}
