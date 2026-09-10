use serde::{Deserialize, Serialize};

use super::token::TokenAddress;

/// Order kind: sell a fixed amount or buy a fixed amount
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderKind {
    Sell,
    Buy,
}

/// Order class: market orders fill at market price, limit orders fill at or better than limit
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OrderClass {
    #[default]
    Market,
    Limit,
    Liquidity,
}

/// A single trade intent from a CoW Protocol user
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    /// Unique order UID (hex string)
    pub uid: String,
    /// Token the user is selling
    pub sell_token: TokenAddress,
    /// Token the user is buying
    pub buy_token: TokenAddress,
    /// Amount of sell_token the user offers (U256 as decimal string)
    pub sell_amount: String,
    /// Minimum amount of buy_token the user accepts (U256 as decimal string)
    pub buy_amount: String,
    /// Maximum fee the user allows (U256 as decimal string)
    #[serde(default)]
    pub fee_amount: String,
    /// Whether this is a sell or buy order
    pub kind: OrderKind,
    /// Whether partial fill is allowed
    #[serde(default)]
    pub partially_fillable: bool,
    /// Order class (market, limit, or liquidity)
    #[serde(default)]
    pub class: OrderClass,
    /// Where to source sell tokens from ("erc20", "internal", "external")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sell_token_balance: Option<String>,
    /// Where to send buy tokens ("erc20", "internal")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buy_token_balance: Option<String>,
    /// Signing scheme used ("eip712", "ethsign", "presign", "eip1271")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_scheme: Option<String>,
    /// Order signature bytes (hex string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Receiver of buy tokens (defaults to owner)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    /// App data hash (hex string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_data: Option<String>,
    /// Order validity deadline (Unix timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<u64>,
}

impl Order {
    /// Parse sell_amount as u128 (panics if overflow — use only in tests)
    pub fn sell_amount_u128(&self) -> u128 {
        self.sell_amount.parse().expect("valid sell_amount")
    }

    /// Parse buy_amount as u128 (panics if overflow — use only in tests)
    pub fn buy_amount_u128(&self) -> u128 {
        self.buy_amount.parse().expect("valid buy_amount")
    }
}
