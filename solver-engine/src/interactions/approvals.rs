//! ERC-20 approval management for the settlement contract.
//!
//! Before the settlement contract can move tokens through DEXs, it needs
//! ERC-20 approvals. The settlement contract must approve DEX routers to
//! spend tokens on its behalf before each swap interaction.
//!
//! This module provides:
//! - Calldata encoding for `allowance()` checks (for RPC use)
//! - `approve(spender, type(uint256).max)` interaction encoding
//! - Approval prepending logic: approvals go before swap interactions

use crate::models::solution::{CustomInteraction, Interaction};

/// The CoW Protocol settlement contract (same on all EVM chains).
pub const SETTLEMENT_CONTRACT: &str = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41";

/// type(uint256).max — single approval, never needs renewal.
pub const APPROVAL_AMOUNT_MAX: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639935";

// ── Calldata encoding ─────────────────────────────────────────────────────────

/// Encode `allowance(address owner, address spender)` calldata for eth_call.
///
/// Returns 0x-prefixed hex string (136 chars = 4 selector + 2×32 param bytes).
pub fn encode_allowance_check(owner: &str, spender: &str) -> String {
    // selector = keccak256("allowance(address,address)")[0..4] = 0xdd62ed3e
    let selector = "dd62ed3e";
    format!("0x{selector}{}{}", pad_address(owner), pad_address(spender))
}

/// Encode `approve(address spender, uint256 amount)` as a [`CustomInteraction`].
///
/// Uses `type(uint256).max` so the approval never expires.
/// The interaction targets the ERC-20 `token` contract directly.
pub fn encode_approval(token: &str, spender: &str) -> Interaction {
    // selector = keccak256("approve(address,uint256)")[0..4] = 0x095ea7b3
    let selector = "095ea7b3";
    let max_uint256 = "f".repeat(64); // type(uint256).max
    let calldata = format!("0x{selector}{}{max_uint256}", pad_address(spender));

    Interaction::Custom(CustomInteraction {
        internalize: false,
        target: token.to_string(),
        value: "0".to_string(),
        call_data: calldata,
    })
}

/// Decode an `allowance()` RPC response (hex-encoded 32-byte word → u128).
///
/// Values that exceed u128::MAX are clamped to u128::MAX (treated as unlimited).
/// Returns None only for malformed (non-hex) responses.
pub fn decode_allowance(hex_response: &str) -> Option<u128> {
    let hex = hex_response.trim_start_matches("0x");
    // 32-byte word → 64 hex chars. If value exceeds u128 range, clamp.
    let relevant = if hex.len() > 32 {
        &hex[hex.len() - 32..] // take least-significant 16 bytes
    } else {
        hex
    };
    u128::from_str_radix(relevant, 16).ok()
}

/// Return `true` if a new approval is needed.
pub fn needs_approval(current_allowance: u128, required_amount: u128) -> bool {
    current_allowance < required_amount
}

// ── Solution integration ───────────────────────────────────────────────────────

/// Describes a single token approval requirement.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// ERC-20 token contract address
    pub token: String,
    /// Address being approved (e.g. a DEX router or pool)
    pub spender: String,
    /// Minimum required allowance; approval is skipped if current >= this
    pub required_amount: u128,
    /// Current allowance from on-chain state (0 = unknown → always approve)
    pub current_allowance: u128,
}

/// Prepend required approval interactions before swap interactions.
///
/// Approvals with sufficient current allowance are skipped.
/// The resulting list is: `[approvals..., original_interactions...]`.
pub fn prepend_approvals(
    interactions: Vec<Interaction>,
    requests: &[ApprovalRequest],
) -> Vec<Interaction> {
    let mut result: Vec<Interaction> = requests
        .iter()
        .filter(|r| needs_approval(r.current_allowance, r.required_amount))
        .map(|r| encode_approval(&r.token, &r.spender))
        .collect();

    result.extend(interactions);
    result
}

/// Extract (token, spender) pairs from a list of interactions for approval checking.
///
/// For `LiquidityInteraction`, the `id` field is the pool/router address that will
/// receive the token transfer — this is what needs to be approved.
/// `CustomInteraction`s are excluded; callers must manage approvals for those manually.
pub fn extract_approval_targets(interactions: &[Interaction]) -> Vec<(String, String)> {
    interactions
        .iter()
        .filter_map(|i| match i {
            Interaction::Liquidity(li) => {
                Some((li.input_token.clone(), li.id.clone()))
            }
            Interaction::Custom(_) => None,
        })
        .collect()
}

// ── Internal helpers ───────────────────────────────────────────────────────────

/// Left-pad an Ethereum address (with or without 0x prefix) to a 32-byte hex word.
fn pad_address(addr: &str) -> String {
    let addr = addr.trim_start_matches("0x");
    format!("{:0>64}", addr)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::solution::LiquidityInteraction;

    #[test]
    fn allowance_check_selector_and_length() {
        let calldata = encode_allowance_check(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
        );
        // 0x + 4 selector bytes (8 hex) + 2×32 param bytes (128 hex) = 136 chars total
        assert_eq!(calldata.len(), 2 + 8 + 128, "calldata must be 136 chars");
        assert!(calldata.starts_with("0xdd62ed3e"), "selector must be dd62ed3e");
    }

    #[test]
    fn approve_calldata_selector_and_max_amount() {
        let token = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
        let spender = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D";
        let interaction = encode_approval(token, spender);

        if let Interaction::Custom(ci) = interaction {
            assert_eq!(ci.target, token);
            assert_eq!(ci.value, "0");
            assert!(ci.call_data.starts_with("0x095ea7b3"), "selector must be 095ea7b3");
            // Last 64 hex chars = type(uint256).max
            assert!(
                ci.call_data.ends_with(&"f".repeat(64)),
                "amount must be type(uint256).max"
            );
            // Total: 0x + 4 selector + 32 spender + 32 amount = 2 + 8 + 64 + 64 = 138 chars
            assert_eq!(ci.call_data.len(), 138);
        } else {
            panic!("expected Custom interaction");
        }
    }

    #[test]
    fn needs_approval_logic() {
        assert!(!needs_approval(1_000, 500));
        assert!(!needs_approval(500, 500));
        assert!(needs_approval(499, 500));
        assert!(needs_approval(0, 1));
    }

    #[test]
    fn decode_allowance_zero() {
        assert_eq!(decode_allowance(&format!("0x{}", "0".repeat(64))), Some(0));
    }

    #[test]
    fn decode_allowance_small_value() {
        // 100 in hex = 64, padded to 64 chars
        let hex = format!("0x{:0>64}", "64");
        assert_eq!(decode_allowance(&hex), Some(100));
    }

    #[test]
    fn decode_allowance_max_u128_clamped() {
        // type(uint256).max — clamped to low 16 bytes = u128::MAX
        let hex = format!("0x{}", "f".repeat(64));
        let result = decode_allowance(&hex);
        assert_eq!(result, Some(u128::MAX));
    }

    #[test]
    fn prepend_approvals_skips_when_sufficient() {
        let swap = Interaction::Liquidity(LiquidityInteraction {
            internalize: false,
            id: "pool".into(),
            input_token: "0xa".into(),
            output_token: "0xb".into(),
            input_amount: "1000".into(),
            output_amount: "2000".into(),
        });
        let requests = vec![ApprovalRequest {
            token: "0xa".into(),
            spender: "pool".into(),
            required_amount: 1000,
            current_allowance: 1000, // exactly sufficient
        }];
        let result = prepend_approvals(vec![swap], &requests);
        assert_eq!(result.len(), 1, "no approval needed");
        assert!(matches!(result[0], Interaction::Liquidity(_)));
    }

    #[test]
    fn prepend_approvals_inserts_before_swaps() {
        let swap = Interaction::Liquidity(LiquidityInteraction {
            internalize: false,
            id: "pool".into(),
            input_token: "0xa".into(),
            output_token: "0xb".into(),
            input_amount: "1000".into(),
            output_amount: "2000".into(),
        });
        let requests = vec![ApprovalRequest {
            token: "0xa".into(),
            spender: "pool".into(),
            required_amount: 1000,
            current_allowance: 0,
        }];
        let result = prepend_approvals(vec![swap], &requests);
        assert_eq!(result.len(), 2);
        assert!(matches!(result[0], Interaction::Custom(_)), "approval must be first");
        assert!(matches!(result[1], Interaction::Liquidity(_)), "swap must be second");
    }

    #[test]
    fn extract_approval_targets_from_liquidity_interactions() {
        let interactions = vec![
            Interaction::Liquidity(LiquidityInteraction {
                internalize: false,
                id: "0xpool1".into(),
                input_token: "0xweth".into(),
                output_token: "0xusdc".into(),
                input_amount: "1000".into(),
                output_amount: "2000".into(),
            }),
            Interaction::Liquidity(LiquidityInteraction {
                internalize: false,
                id: "0xpool2".into(),
                input_token: "0xusdc".into(),
                output_token: "0xdai".into(),
                input_amount: "500".into(),
                output_amount: "499".into(),
            }),
        ];
        let targets = extract_approval_targets(&interactions);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0], ("0xweth".into(), "0xpool1".into()));
        assert_eq!(targets[1], ("0xusdc".into(), "0xpool2".into()));
    }

    #[test]
    fn pad_address_is_64_hex_chars() {
        let padded = pad_address("0xdeadbeef");
        assert_eq!(padded.len(), 64);
        assert!(padded.ends_with("deadbeef"));
        assert!(padded.starts_with("000000000000000000000000000000000000000000000000000000"));
    }
}
