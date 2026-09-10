//! Balancer V2 swap calldata encoding for the CoW Protocol settlement.
//!
//! All Balancer V2 swaps go through the Vault contract. For single-hop swaps,
//! use `Vault.swap()` (selector `0x52bbbe29`). For multi-hop, use `Vault.batchSwap()`.
//!
//! The calldata encoding here is used by the solver's assembler to produce
//! `Interaction` objects for the settlement transaction.

use crate::models::solution::{CustomInteraction, Interaction};

/// Balancer V2 Vault address (same on all chains)
pub const BALANCER_VAULT: &str = "0xBA12222222228d8Ba445958a75a0704d566BF2C8";

/// `swap(SingleSwap,FundManagement,uint256,uint256)` selector
const SWAP_SELECTOR: &str = "52bbbe29";

/// `batchSwap(uint8,BatchSwapStep[],address[],FundManagement,int256[],uint256)` selector
const BATCH_SWAP_SELECTOR: &str = "945bcec9";

// ── ABI helpers ──────────────────────────────────────────────────────────────

fn encode_uint256(value: u128) -> String {
    format!("{:064x}", value)
}

fn encode_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("{:0>64}", clean)
}

// ── Single swap encoding ────────────────────────────────────────────────────

/// Encode a single Balancer V2 swap via `Vault.swap()`.
///
/// Uses `GIVEN_IN` kind (exact tokenIn amount).
/// The interaction targets the Vault contract directly.
///
/// # Arguments
/// - `pool_id` — Balancer pool ID (32 bytes hex)
/// - `token_in` — address of the token being sold
/// - `token_out` — address of the token being bought
/// - `amount_in` — exact amount of token_in to sell
/// - `min_amount_out` — minimum amount of token_out to receive (slippage protection)
/// - `sender` — address sending token_in (settlement contract)
/// - `recipient` — address receiving token_out (settlement contract)
pub fn encode_single_swap(
    pool_id: &str,
    token_in: &str,
    token_out: &str,
    amount_in: u128,
    min_amount_out: u128,
    sender: &str,
    recipient: &str,
) -> Interaction {
    // SingleSwap struct:
    //   bytes32 poolId
    //   uint8   kind (0 = GIVEN_IN)
    //   address assetIn
    //   address assetOut
    //   uint256 amount
    //   bytes   userData (offset to dynamic data)
    let pool_id_clean = pool_id.trim_start_matches("0x");
    let pool_id_padded = format!("{:0>64}", pool_id_clean);
    let kind = "0".repeat(64); // GIVEN_IN = 0
    let asset_in = encode_address(token_in);
    let asset_out = encode_address(token_out);
    let amount = encode_uint256(amount_in);
    // userData offset: 0xc0 = 192 bytes from start of SingleSwap struct
    let user_data_offset = format!("{:0>64x}", 0xc0u64);

    // FundManagement struct:
    //   address sender
    //   bool    fromInternalBalance (false)
    //   address recipient
    //   bool    toInternalBalance (false)
    let sender_padded = encode_address(sender);
    let from_internal = "0".repeat(64);
    let recipient_padded = encode_address(recipient);
    let to_internal = "0".repeat(64);

    // limit = min_amount_out
    let limit = encode_uint256(min_amount_out);
    // deadline = far future
    let deadline = format!("{:0>64x}", u64::MAX);

    // userData bytes: empty (length = 0)
    let user_data_len = "0".repeat(64);

    let call_data = format!(
        "0x{SWAP_SELECTOR}{pool_id_padded}{kind}{asset_in}{asset_out}{amount}{user_data_offset}{sender_padded}{from_internal}{recipient_padded}{to_internal}{limit}{deadline}{user_data_len}"
    );

    Interaction::Custom(CustomInteraction {
        internalize: false,
        target: BALANCER_VAULT.to_string(),
        value: "0".to_string(),
        call_data,
    })
}

/// Encode a Balancer V2 swap via `Vault.swap()` with the settlement contract
/// as both sender and recipient.
pub fn encode_settlement_swap(
    pool_id: &str,
    token_in: &str,
    token_out: &str,
    amount_in: u128,
    min_amount_out: u128,
) -> Interaction {
    const SETTLEMENT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";
    encode_single_swap(pool_id, token_in, token_out, amount_in, min_amount_out, SETTLEMENT, SETTLEMENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_swap_has_correct_selector() {
        let interaction = encode_single_swap(
            "0x36bf227d6bac96e2ab1ebb5492ecec69c691943f000200000000000000000316",
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            1_000_000_000_000_000_000u128,
            0,
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, BALANCER_VAULT);
            assert!(c.call_data.starts_with("0x52bbbe29"));
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn settlement_swap_targets_vault() {
        let interaction = encode_settlement_swap(
            "0x36bf227d6bac96e2ab1ebb5492ecec69c691943f000200000000000000000316",
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            1_000_000_000_000_000_000u128,
            900_000u128,
        );
        if let Interaction::Custom(c) = &interaction {
            assert_eq!(c.target, BALANCER_VAULT);
        } else {
            panic!("Expected Custom interaction");
        }
    }

    #[test]
    fn encode_address_pads_correctly() {
        let padded = encode_address("0xBA12222222228d8Ba445958a75a0704d566BF2C8");
        assert_eq!(padded.len(), 64);
        assert!(padded.starts_with("000000000000000000000000"));
    }
}
