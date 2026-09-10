use serde::{Deserialize, Serialize};

/// Token address (checksummed hex string)
pub type TokenAddress = String;

/// Token metadata (internal representation for pool math)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub address: TokenAddress,
    pub decimals: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Per-token metadata from the CoW driver auction payload
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    /// Token decimals — CoW sends null for unknown tokens; we default to 18.
    #[serde(default)]
    pub decimals: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Reference price in ETH (decimal string, may be absent for untrusted tokens)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_price: Option<String>,
    /// Token balance available in the settlement contract (decimal string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_balance: Option<String>,
    /// Whether this token is trusted for surplus calculations
    #[serde(default)]
    pub trusted: bool,
}

impl TokenInfo {
    /// Returns token decimals, defaulting to 18 if CoW sent null.
    pub fn decimals_or_default(&self) -> u8 {
        self.decimals.unwrap_or(18)
    }
}

impl Token {
    pub fn new(address: impl Into<String>, decimals: u8) -> Self {
        Self {
            address: address.into(),
            decimals,
            symbol: None,
            name: None,
        }
    }

    /// Scale a raw U256 amount to a human-readable f64 (for logging only — never use for math)
    pub fn to_display_amount(&self, raw: u128) -> f64 {
        raw as f64 / 10f64.powi(self.decimals as i32)
    }
}
