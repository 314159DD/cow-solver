//! Interaction internalization — skip on-chain DEX execution for trusted tokens.
//!
//! The CoW settlement contract holds token buffers from positive slippage.
//! When both input and output tokens are trusted AND the contract has enough
//! balance of the output token, the driver can "internalize" the swap —
//! it moves tokens from its own balance instead of executing the AMM on-chain.
//!
//! This saves ~100-150K gas per internalized interaction.

use crate::models::auction::TokenMap;
use crate::models::solution::Interaction;

/// Check if an interaction is eligible for internalization.
///
/// Requirements:
/// 1. Input token must be `trusted: true` in the auction token map
/// 2. Output token must be `trusted: true`
/// 3. Output token's `available_balance` >= interaction's output_amount
pub fn should_internalize(
    input_token: &str,
    output_token: &str,
    output_amount: u128,
    tokens: &TokenMap,
) -> bool {
    let input_lower = input_token.to_lowercase();
    let output_lower = output_token.to_lowercase();

    // Both tokens must be trusted
    let input_info = tokens.iter()
        .find(|(k, _)| k.to_lowercase() == input_lower)
        .map(|(_, v)| v);
    let output_info = tokens.iter()
        .find(|(k, _)| k.to_lowercase() == output_lower)
        .map(|(_, v)| v);

    let input_trusted = input_info.map(|t| t.trusted).unwrap_or(false);
    let output_trusted = output_info.map(|t| t.trusted).unwrap_or(false);

    if !input_trusted || !output_trusted {
        return false;
    }

    // Output token must have sufficient available_balance in the settlement contract
    let available: u128 = output_info
        .and_then(|t| t.available_balance.as_ref())
        .and_then(|b| b.parse().ok())
        .unwrap_or(0);

    available >= output_amount
}

/// Apply internalization to a slice of interactions in-place.
///
/// Returns the count of interactions that were internalized.
pub fn try_internalize_interactions(
    interactions: &mut [Interaction],
    tokens: &TokenMap,
) -> usize {
    let mut count = 0;
    for interaction in interactions.iter_mut() {
        if let Interaction::Liquidity(li) = interaction {
            let output_amount: u128 = li.output_amount.parse().unwrap_or(0);
            if output_amount > 0
                && should_internalize(&li.input_token, &li.output_token, output_amount, tokens)
            {
                li.internalize = true;
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::solution::{Interaction, LiquidityInteraction};
    use crate::models::token::TokenInfo;
    use std::collections::HashMap;

    fn make_tokens(trusted_in: bool, trusted_out: bool, balance: &str) -> TokenMap {
        let mut tokens = HashMap::new();
        tokens.insert("0xweth".to_string(), TokenInfo {
            decimals: Some(18),
            symbol: Some("WETH".to_string()),
            reference_price: Some("1000000000000000000".to_string()),
            available_balance: Some("5000000000000000000".to_string()), // 5 ETH
            trusted: trusted_in,
        });
        tokens.insert("0xusdc".to_string(), TokenInfo {
            decimals: Some(6),
            symbol: Some("USDC".to_string()),
            reference_price: Some("500000000000000".to_string()),
            available_balance: Some(balance.to_string()),
            trusted: trusted_out,
        });
        tokens
    }

    fn make_interaction(input: &str, output: &str, out_amount: &str) -> Interaction {
        Interaction::Liquidity(LiquidityInteraction {
            internalize: false,
            id: "pool1".to_string(),
            input_token: input.to_string(),
            output_token: output.to_string(),
            input_amount: "1000000000000000000".to_string(),
            output_amount: out_amount.to_string(),
        })
    }

    #[test]
    fn both_trusted_sufficient_balance() {
        let tokens = make_tokens(true, true, "10000000"); // 10 USDC
        assert!(should_internalize("0xweth", "0xusdc", 5_000_000, &tokens));
    }

    #[test]
    fn input_not_trusted() {
        let tokens = make_tokens(false, true, "10000000");
        assert!(!should_internalize("0xweth", "0xusdc", 5_000_000, &tokens));
    }

    #[test]
    fn output_not_trusted() {
        let tokens = make_tokens(true, false, "10000000");
        assert!(!should_internalize("0xweth", "0xusdc", 5_000_000, &tokens));
    }

    #[test]
    fn insufficient_balance() {
        let tokens = make_tokens(true, true, "1000000"); // 1 USDC — less than 5M needed
        assert!(!should_internalize("0xweth", "0xusdc", 5_000_000, &tokens));
    }

    #[test]
    fn try_internalize_sets_flag() {
        let tokens = make_tokens(true, true, "10000000");
        let mut interactions = vec![make_interaction("0xweth", "0xusdc", "5000000")];
        let count = try_internalize_interactions(&mut interactions, &tokens);
        assert_eq!(count, 1);
        if let Interaction::Liquidity(li) = &interactions[0] {
            assert!(li.internalize);
        } else {
            panic!("expected liquidity interaction");
        }
    }

    #[test]
    fn try_internalize_skips_untrusted() {
        let tokens = make_tokens(true, false, "10000000");
        let mut interactions = vec![make_interaction("0xweth", "0xusdc", "5000000")];
        let count = try_internalize_interactions(&mut interactions, &tokens);
        assert_eq!(count, 0);
        if let Interaction::Liquidity(li) = &interactions[0] {
            assert!(!li.internalize);
        }
    }
}
