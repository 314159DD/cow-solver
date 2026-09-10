use crate::models::solution::{CustomInteraction, Interaction};

// ── GMX V2 Exchange Router ──────────────────────────────────────────────────

/// GMX V2 Exchange Router on Arbitrum
pub const GMX_V2_EXCHANGE_ROUTER: &str = "0x7C68C7866A64FA2160F78EEaE12217FFbf871fa8";

/// GMX V2 Swap Router (handles the actual swap execution)
pub const GMX_V2_ROUTER: &str = "0x7C68C7866A64FA2160F78EEaE12217FFbf871fa8";

/// `createOrder(...)` — the primary method for creating swap orders on GMX V2.
/// For spot swaps, we use the multicall approach: sendWnt + createOrder.
/// selector = "createOrder((address,address,address,address,address,address[],address[],uint256,uint256,uint256,uint256,uint256,uint256,uint8,uint8,bool,bool,bytes32))"
const CREATE_ORDER_SELECTOR: &str = "3f6e8a37";

/// `multicall(bytes[])` — batches multiple calls (sendWnt + createOrder)
const MULTICALL_SELECTOR: &str = "ac9650d8";

/// `sendWnt(address,uint256)` — send wrapped native token to receiver
const SEND_WNT_SELECTOR: &str = "53e7e7d8";

// ── ABI helpers ─────────────────────────────────────────────────────────────

fn encode_uint256(value: u128) -> String {
    format!("{:064x}", value)
}

fn encode_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Encode a GMX V2 swap via the Exchange Router.
///
/// GMX V2 swaps use createOrder with orderType = MarketSwap (0).
/// The swap path is [market_address] for single-market swaps.
///
/// # Arguments
/// - `market_address` — the GMX V2 market to swap through
/// - `token_in` — input token address
/// - `amount_in` — exact sell amount
/// - `amount_out_min` — minimum buy amount
/// - `recipient` — receives output tokens (settlement contract)
#[allow(clippy::too_many_arguments)]
pub fn encode_gmx_v2_swap(
    market_address: &str,
    token_in: &str,
    amount_in: u128,
    amount_out_min: u128,
    recipient: &str,
) -> Interaction {
    // GMX V2 swap via ExchangeRouter.createOrder
    //
    // CreateOrderParams struct (simplified for market swap):
    //   receiver: address           — who gets the output
    //   callbackContract: address   — zero (no callback)
    //   uiFeeReceiver: address      — zero (no UI fee)
    //   market: address             — the market to swap in
    //   initialCollateralToken: address — token_in
    //   swapPath: address[]         — [market_address]
    //   sizeDeltaUsd: uint256       — 0 (spot swap, not perp)
    //   initialCollateralDeltaAmount: uint256 — amount_in
    //   triggerPrice: uint256       — 0
    //   acceptablePrice: uint256    — max uint256 (market order)
    //   executionFee: uint256       — 0 (paid separately)
    //   callbackGasLimit: uint256   — 0
    //   minOutputAmount: uint256    — amount_out_min
    //   orderType: uint8            — 0 (MarketSwap)
    //   decreasePositionSwapType: uint8 — 0
    //   isLong: bool                — false
    //   shouldUnwrapNativeToken: bool — false
    //   referralCode: bytes32       — 0

    // For simplicity, encode as a single createOrder call.
    // The settlement contract will have pre-approved tokens.

    // Encode the struct as flat ABI params
    let zero = encode_uint256(0);
    let max_u256 = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string();

    // Offset to the CreateOrderParams struct data
    let struct_offset = encode_uint256(0x20); // 1 word

    // Encode the swapPath array (offset + length + element)
    // swapPath is dynamic, so we need its offset within the struct
    // The struct has 13 fixed fields before swapPath, then swapPath offset
    // For a simplified encoding, we pack everything linearly

    let call_data = format!(
        "0x{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}{}", // 21 params + selector
        CREATE_ORDER_SELECTOR,
        struct_offset,
        // Begin struct fields:
        encode_address(recipient),              // receiver
        zero,                                   // callbackContract (zero)
        zero,                                   // uiFeeReceiver (zero)
        encode_address(market_address),         // market
        encode_address(token_in),               // initialCollateralToken
        encode_uint256(0x0240),                 // offset to swapPath array
        encode_uint256(amount_in),              // initialCollateralDeltaAmount
        zero,                                   // sizeDeltaUsd (0 for spot swap)
        zero,                                   // triggerPrice
        max_u256,                               // acceptablePrice (max = market order)
        zero,                                   // executionFee
        zero,                                   // callbackGasLimit
        encode_uint256(amount_out_min),         // minOutputAmount
        zero,                                   // orderType = 0 (MarketSwap)
        zero,                                   // decreasePositionSwapType = 0
        zero,                                   // isLong = false
        zero,                                   // shouldUnwrapNativeToken = false
        zero,                                   // referralCode = 0
        // swapPath array:
        encode_uint256(1),                      // swapPath.length = 1
        encode_address(market_address),         // swapPath[0] = market
    );

    Interaction::Custom(CustomInteraction {
        internalize: false,
        target: GMX_V2_EXCHANGE_ROUTER.to_string(),
        value: "0".to_string(),
        call_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_has_correct_selector() {
        let interaction = encode_gmx_v2_swap(
            "0x70d95587d40A2cda56C5e14bBbF65707D79e44e6", // ETH/USD market
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1", // WETH
            1_000_000_000_000_000_000,                      // 1 ETH
            3_400_000_000,                                   // 3400 USDC min
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41", // settlement
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, GMX_V2_EXCHANGE_ROUTER);
            assert!(c.call_data.starts_with("0x3f6e8a37"));
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn swap_target_is_exchange_router() {
        let interaction = encode_gmx_v2_swap(
            "0x70d95587d40A2cda56C5e14bBbF65707D79e44e6",
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831", // USDC
            3_500_000_000,
            900_000_000_000_000_000, // min 0.9 ETH
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, GMX_V2_EXCHANGE_ROUTER);
            assert_eq!(c.value, "0");
        } else {
            panic!("Expected Custom interaction");
        }
    }
}
