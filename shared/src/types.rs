//! Common type aliases used across the solver.

/// Token address (lowercase hex string with 0x prefix)
pub type Address = String;

/// Raw token amount as a decimal string (avoids U256 serde issues)
pub type AmountStr = String;

/// Transaction hash (hex string with 0x prefix)
pub type TxHash = String;

/// Block number
pub type BlockNumber = u64;

/// Chain ID
pub type ChainId = u64;
