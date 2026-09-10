use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    liquidity::Liquidity,
    order::Order,
    token::{TokenAddress, TokenInfo},
};

/// Token metadata map: token address → TokenInfo
pub type TokenMap = HashMap<TokenAddress, TokenInfo>;

/// A batch auction instance from the CoW driver (POST /solve payload)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuctionInstance {
    /// Auction identifier (driver may send as string or number)
    #[serde(deserialize_with = "deserialize_id_flexible")]
    pub id: u64,
    /// Token metadata for all tokens in this auction
    #[serde(default)]
    pub tokens: TokenMap,
    /// Orders to be settled in this batch
    #[serde(default)]
    pub orders: Vec<Order>,
    /// Available liquidity sources (unknown types silently skipped)
    #[serde(default, deserialize_with = "super::liquidity::deserialize_liquidity_vec")]
    pub liquidity: Vec<Liquidity>,
    /// Effective gas price at time of auction (decimal string, wei)
    #[serde(default)]
    pub effective_gas_price: String,
    /// Deadline by which the solver must respond (ISO 8601 string or Unix timestamp string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    /// Chain ID this auction is on
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<u64>,
    /// Block number at time of auction creation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<u64>,
}

/// Deserialize auction id from either a JSON number or a quoted decimal string.
/// The CoW driver sends `"id": "1"` (string), but some test fixtures use `"id": 1` (number).
fn deserialize_id_flexible<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::de::{self, Unexpected};

    struct IdVisitor;

    impl<'de> de::Visitor<'de> for IdVisitor {
        type Value = u64;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a u64 or a string containing a decimal u64")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::invalid_value(Unexpected::Signed(v), &self))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<u64, E> {
            v.parse()
                .map_err(|_| E::invalid_value(Unexpected::Str(v), &self))
        }
    }

    d.deserialize_any(IdVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_minimal_auction() {
        let json = r#"{"id": 1, "orders": []}"#;
        let auction: AuctionInstance = serde_json::from_str(json).unwrap();
        assert_eq!(auction.id, 1);
        assert!(auction.orders.is_empty());
    }

    #[test]
    fn deserialize_id_as_string() {
        let json = r#"{"id": "42", "orders": []}"#;
        let auction: AuctionInstance = serde_json::from_str(json).unwrap();
        assert_eq!(auction.id, 42);
    }

    #[test]
    fn deserialize_full_sample() {
        let json = include_str!("../../../data/sample_auction.json");
        let auction: AuctionInstance = serde_json::from_str(json).expect("sample must deserialize");
        assert_eq!(auction.id, 1);
        assert_eq!(auction.tokens.len(), 3);
        assert_eq!(auction.orders.len(), 2);
        assert_eq!(auction.liquidity.len(), 3);
        assert!(!auction.effective_gas_price.is_empty());
    }

    #[test]
    fn deserialize_token_info() {
        let json = r#"{
            "id": "1",
            "tokens": {
                "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2": {
                    "decimals": 18,
                    "symbol": "WETH",
                    "referencePrice": "1000000000000000000",
                    "availableBalance": "10000000000000000000",
                    "trusted": true
                }
            },
            "orders": []
        }"#;
        let auction: AuctionInstance = serde_json::from_str(json).unwrap();
        let weth = auction
            .tokens
            .get("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")
            .unwrap();
        assert_eq!(weth.decimals, Some(18));
        assert_eq!(weth.symbol.as_deref(), Some("WETH"));
        assert!(weth.trusted);
    }

    #[test]
    fn deserialize_token_info_null_decimals() {
        // CoW sends null decimals for unknown/untrusted tokens — must not crash
        let json = r#"{
            "id": "1",
            "tokens": {
                "0xunknown": {
                    "decimals": null,
                    "symbol": null,
                    "referencePrice": null,
                    "availableBalance": "0",
                    "trusted": false
                }
            },
            "orders": []
        }"#;
        let auction: AuctionInstance = serde_json::from_str(json).unwrap();
        let token = auction.tokens.get("0xunknown").unwrap();
        assert_eq!(token.decimals, None);
        assert_eq!(token.decimals_or_default(), 18);
    }

    #[test]
    fn deserialize_constant_product_liquidity() {
        let json = r#"{
            "id": "1",
            "orders": [],
            "liquidity": [{
                "kind": "constantProduct",
                "tokens": {
                    "0xtoken0": { "balance": "1000000" },
                    "0xtoken1": { "balance": "2000000" }
                },
                "fee": "0.003",
                "id": "pool-0",
                "address": "0xpool",
                "gasEstimate": "110000"
            }]
        }"#;
        let auction: AuctionInstance = serde_json::from_str(json).unwrap();
        assert_eq!(auction.liquidity.len(), 1);
        match &auction.liquidity[0] {
            Liquidity::ConstantProduct(p) => {
                assert_eq!(p.fee, "0.003");
                assert_eq!(p.tokens.len(), 2);
            }
            other => panic!("expected ConstantProduct, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_concentrated_liquidity() {
        let json = r#"{
            "id": "1",
            "orders": [],
            "liquidity": [{
                "kind": "concentratedLiquidity",
                "tokens": ["0xtoken0", "0xtoken1"],
                "fee": "0.0005",
                "id": "univ3-0",
                "address": "0xpool",
                "sqrtPrice": "1855498850490838808812",
                "liquidity": "20457398620580",
                "tick": 201861,
                "gasEstimate": "130000"
            }]
        }"#;
        let auction: AuctionInstance = serde_json::from_str(json).unwrap();
        match &auction.liquidity[0] {
            Liquidity::ConcentratedLiquidity(p) => {
                assert_eq!(p.tick, 201861);
                assert_eq!(p.sqrt_price, "1855498850490838808812");
            }
            other => panic!("expected ConcentratedLiquidity, got {:?}", other),
        }
    }

    #[test]
    fn malformed_missing_required_field() {
        // `id` is required — missing it should fail
        let json = r#"{"orders": []}"#;
        assert!(serde_json::from_str::<AuctionInstance>(json).is_err());
    }

    #[test]
    fn malformed_bad_id_string() {
        let json = r#"{"id": "not-a-number", "orders": []}"#;
        assert!(serde_json::from_str::<AuctionInstance>(json).is_err());
    }
}
