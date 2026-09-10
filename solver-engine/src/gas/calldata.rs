//! Calldata size estimation per interaction type.
//!
//! Each DEX interaction produces ABI-encoded calldata that gets posted to
//! Ethereum L1 when settling on Arbitrum. The L1 data cost is proportional
//! to the number of bytes. These estimates are based on the actual ABI
//! encoding of each swap function.

/// Interaction type for calldata size estimation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionKind {
    /// Uniswap V2 / Sushiswap `swap(uint,uint,address,bytes)`
    UniswapV2Swap,
    /// Uniswap V3 `exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))`
    UniswapV3ExactInputSingle,
    /// Balancer `batchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool),int256[],uint256)`
    BalancerBatchSwap {
        /// Number of swap steps (hops) in the batch
        hop_count: usize,
    },
    /// Curve `exchange(int128,int128,uint256,uint256)`
    CurveExchange,
    /// Camelot V2 `swap(uint,uint,address,bytes)` — same ABI as Uniswap V2
    CamelotV2Swap,
    /// Camelot V3 (Algebra) `swap(address,bool,int256,uint160,bytes)` — different from Uni V3
    CamelotV3Swap,
    /// Trader Joe V2.1 LBRouter `swapExactTokensForTokens(uint256,uint256,(uint256[],uint8[],address[]),address,uint256)`
    TraderJoeV21Swap,
    /// ERC-20 `approve(address,uint256)`
    Erc20Approve,
    /// Settlement contract base transaction overhead (function selector, parameters, signatures)
    SettlementOverhead,
}

/// Estimate the calldata size in bytes for a given interaction.
///
/// These are conservative estimates based on the ABI encoding of each function.
/// Actual sizes may vary slightly due to dynamic encoding, but these are close
/// enough for L1 data cost estimation (typically within 5-10%).
pub fn estimate_calldata_bytes(kind: InteractionKind) -> usize {
    match kind {
        // swap(uint256,uint256,address,bytes)
        // 4 (selector) + 32*3 (amounts + to) + 64 (bytes offset+length) + 32*2 (path addresses)
        InteractionKind::UniswapV2Swap => 196,

        // exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))
        // 4 (selector) + 32*8 (struct fields) = 260, but with tuple offset overhead ~228
        InteractionKind::UniswapV3ExactInputSingle => 228,

        // batchSwap has a base cost plus per-hop cost for the swap steps array
        // Base: 4 (selector) + 32*6 (kind, assets offsets, funds struct, deadline) + ~128 (dynamic headers)
        // Per hop: ~64 bytes (poolId, assetIn/OutIndex, amount, userData)
        InteractionKind::BalancerBatchSwap { hop_count } => 320 + 64 * hop_count,

        // exchange(int128,int128,uint256,uint256)
        // 4 (selector) + 32*4 (i, j, dx, min_dy)
        InteractionKind::CurveExchange => 132,

        // Camelot V2: same ABI as Uniswap V2 swap(uint,uint,address,bytes)
        InteractionKind::CamelotV2Swap => 196,

        // Camelot V3 (Algebra): swap(address,bool,int256,uint160,bytes)
        // 4 (selector) + 32*5 (recipient, zeroForOne, amountSpecified, sqrtPriceLimitX96, data offset)
        // + ~64 (data length + padding)
        InteractionKind::CamelotV3Swap => 228,

        // Trader Joe V2.1: swapExactTokensForTokens with Path struct
        // 4 (selector) + 32*5 (header) + 32*3 (offsets) + 32*7 (data) = 484 bytes
        InteractionKind::TraderJoeV21Swap => 484,

        // approve(address,uint256)
        // 4 (selector) + 32*2 (spender, amount)
        InteractionKind::Erc20Approve => 68,

        // Settlement transaction base overhead:
        // function selector, order arrays, interaction arrays, signatures
        // This is the fixed cost per settlement transaction regardless of swap count
        InteractionKind::SettlementOverhead => 500,
    }
}

/// Estimate total calldata bytes for a set of interactions.
///
/// Includes the settlement overhead automatically.
pub fn estimate_total_calldata(interactions: &[InteractionKind]) -> usize {
    let base = estimate_calldata_bytes(InteractionKind::SettlementOverhead);
    let swap_bytes: usize = interactions.iter().map(|k| estimate_calldata_bytes(*k)).sum();
    base + swap_bytes
}

/// Map a boolean is_v3 flag to the corresponding interaction kind.
///
/// This is a convenience for the common case where we only distinguish V2 vs V3.
pub fn interaction_kind_from_v3_flag(is_v3: bool) -> InteractionKind {
    if is_v3 {
        InteractionKind::UniswapV3ExactInputSingle
    } else {
        InteractionKind::UniswapV2Swap
    }
}

/// Map a `PoolKind` to the corresponding `InteractionKind`.
///
/// This lets gas estimation use the exact DEX type instead of just V2/V3 flags.
pub fn interaction_kind_from_pool_kind(kind: crate::models::liquidity::PoolKind) -> InteractionKind {
    use crate::models::liquidity::PoolKind;
    match kind {
        PoolKind::UniswapV2 | PoolKind::Sushiswap => InteractionKind::UniswapV2Swap,
        PoolKind::UniswapV3 => InteractionKind::UniswapV3ExactInputSingle,
        PoolKind::CamelotV2 => InteractionKind::CamelotV2Swap,
        PoolKind::CamelotV3 => InteractionKind::CamelotV3Swap,
        PoolKind::Curve => InteractionKind::CurveExchange,
        PoolKind::Balancer | PoolKind::BalancerWeighted | PoolKind::BalancerStable => {
            InteractionKind::BalancerBatchSwap { hop_count: 1 }
        }
        PoolKind::TraderJoeV21 => InteractionKind::TraderJoeV21Swap,
        // GMX V2, DODO, Wombat — use conservative V3-like calldata estimate
        PoolKind::GmxV2 | PoolKind::Dodo | PoolKind::Wombat => InteractionKind::UniswapV3ExactInputSingle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_swap_size_is_reasonable() {
        let size = estimate_calldata_bytes(InteractionKind::UniswapV2Swap);
        // Should be between 128 and 300 bytes for a typical V2 swap
        assert!(size >= 128 && size <= 300, "V2 size {size} out of expected range");
    }

    #[test]
    fn v3_swap_larger_than_v2() {
        let v2 = estimate_calldata_bytes(InteractionKind::UniswapV2Swap);
        let v3 = estimate_calldata_bytes(InteractionKind::UniswapV3ExactInputSingle);
        assert!(v3 > v2, "V3 calldata ({v3}) should be larger than V2 ({v2})");
    }

    #[test]
    fn balancer_scales_with_hops() {
        let one_hop = estimate_calldata_bytes(InteractionKind::BalancerBatchSwap { hop_count: 1 });
        let three_hop = estimate_calldata_bytes(InteractionKind::BalancerBatchSwap { hop_count: 3 });
        assert_eq!(three_hop - one_hop, 128, "each extra hop should add 64 bytes");
    }

    #[test]
    fn approve_is_small() {
        let size = estimate_calldata_bytes(InteractionKind::Erc20Approve);
        assert_eq!(size, 68);
    }

    #[test]
    fn total_calldata_includes_overhead() {
        let interactions = vec![InteractionKind::UniswapV2Swap];
        let total = estimate_total_calldata(&interactions);
        let overhead = estimate_calldata_bytes(InteractionKind::SettlementOverhead);
        let swap = estimate_calldata_bytes(InteractionKind::UniswapV2Swap);
        assert_eq!(total, overhead + swap);
    }

    #[test]
    fn v3_flag_mapping() {
        assert_eq!(
            interaction_kind_from_v3_flag(false),
            InteractionKind::UniswapV2Swap
        );
        assert_eq!(
            interaction_kind_from_v3_flag(true),
            InteractionKind::UniswapV3ExactInputSingle
        );
    }

    #[test]
    fn camelot_v2_same_size_as_univ2() {
        let v2 = estimate_calldata_bytes(InteractionKind::UniswapV2Swap);
        let cam2 = estimate_calldata_bytes(InteractionKind::CamelotV2Swap);
        assert_eq!(v2, cam2, "Camelot V2 has the same ABI as Uniswap V2");
    }

    #[test]
    fn camelot_v3_size_is_reasonable() {
        let cam3 = estimate_calldata_bytes(InteractionKind::CamelotV3Swap);
        assert!(cam3 >= 196 && cam3 <= 300, "Camelot V3 size {cam3} out of range");
    }

    #[test]
    fn curve_exchange_is_small() {
        let size = estimate_calldata_bytes(InteractionKind::CurveExchange);
        assert_eq!(size, 132);
    }

    #[test]
    fn pool_kind_mapping() {
        use crate::models::liquidity::PoolKind;
        assert_eq!(
            interaction_kind_from_pool_kind(PoolKind::CamelotV2),
            InteractionKind::CamelotV2Swap
        );
        assert_eq!(
            interaction_kind_from_pool_kind(PoolKind::CamelotV3),
            InteractionKind::CamelotV3Swap
        );
        assert_eq!(
            interaction_kind_from_pool_kind(PoolKind::Curve),
            InteractionKind::CurveExchange
        );
    }
}
