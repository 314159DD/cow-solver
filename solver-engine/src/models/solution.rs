use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::token::TokenAddress;

// ── Interactions ──────────────────────────────────────────────────────────────

/// A custom on-chain interaction (DEX swap calldata, token approval, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomInteraction {
    /// Whether the settlement contract can skip executing this externally
    #[serde(default)]
    pub internalize: bool,
    /// Contract address to call
    pub target: String,
    /// ETH value to send (decimal string, usually "0")
    pub value: String,
    /// ABI-encoded calldata (hex string with 0x prefix)
    pub call_data: String,
}

/// A liquidity interaction (instructs the driver to route through a known pool)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiquidityInteraction {
    /// Whether the settlement contract can internalize this
    #[serde(default)]
    pub internalize: bool,
    /// Pool id from the auction's liquidity array
    pub id: String,
    pub input_token: TokenAddress,
    pub output_token: TokenAddress,
    /// Amount of input_token consumed (decimal string)
    pub input_amount: String,
    /// Amount of output_token produced (decimal string)
    pub output_amount: String,
}

/// An on-chain interaction tagged by kind
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Interaction {
    Custom(CustomInteraction),
    Liquidity(LiquidityInteraction),
}

// ── Trades ────────────────────────────────────────────────────────────────────

/// Settlement of a user order
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FulfillmentTrade {
    /// Order UID being settled (hex string)
    pub order: String,
    /// Amount executed (sell_amount for sell orders, buy_amount for buy orders; decimal string)
    pub executed_amount: String,
    /// Fee taken from the order's fee budget (decimal string)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fee: String,
}

/// A trade entry in the solution (only fulfillment for Sprint 1–2; jit added in Sprint 3)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Trade {
    Fulfillment(FulfillmentTrade),
}

impl Trade {
    pub fn fulfillment(order_uid: impl Into<String>, executed_amount: impl Into<String>) -> Self {
        Trade::Fulfillment(FulfillmentTrade {
            order: order_uid.into(),
            executed_amount: executed_amount.into(),
            fee: String::new(),
        })
    }
}

// ── Score ─────────────────────────────────────────────────────────────────────

/// How the solver's score is expressed
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Score {
    /// Absolute score — sum of (surplus + fees) * reference_price (decimal string, wei)
    Solver { score: String },
    /// Probabilistic score — solver's own estimate of success probability [0.0, 1.0]
    RiskAdjusted {
        #[serde(rename = "successProbability")]
        success_probability: f64,
    },
}

// ── Solution / Response ───────────────────────────────────────────────────────

/// A candidate solution for one or more orders.
///
/// JSON format must match the CoW Protocol solver API exactly:
/// - `preInteractions`: executed BEFORE settlement (approvals, etc.)
/// - `interactions`: executed DURING settlement (swaps)
/// - `postInteractions`: executed AFTER settlement (cleanup)
/// - `gas`: estimated gas cost (optional but recommended)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Solution {
    /// Monotonically increasing ID within this response (0-indexed)
    pub id: u64,
    /// Uniform clearing prices: token address → price in reference currency (decimal string).
    /// Serialized with keys in sorted order for deterministic JSON output.
    #[serde(serialize_with = "serialize_prices_sorted")]
    pub prices: HashMap<TokenAddress, String>,
    /// Order executions
    pub trades: Vec<Trade>,
    /// Pre-settlement interactions (approvals, setup)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_interactions: Vec<Interaction>,
    /// Intra-settlement interactions (DEX swaps, liquidity routes)
    pub interactions: Vec<Interaction>,
    /// Post-settlement interactions (cleanup)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_interactions: Vec<Interaction>,
    /// Estimated gas cost for this solution (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas: Option<u64>,
    /// Estimated solution score (optional — omit to let driver compute)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<Score>,
}

impl Solution {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            prices: HashMap::new(),
            trades: vec![],
            pre_interactions: vec![],
            interactions: vec![],
            post_interactions: vec![],
            gas: None,
            score: None,
        }
    }
}

/// Top-level response returned by POST /solve
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveResponse {
    pub solutions: Vec<Solution>,
}

impl SolveResponse {
    pub fn empty() -> Self {
        Self { solutions: vec![] }
    }
}

// ── Deterministic serialization helpers ──────────────────────────────────────

/// Serialize a `HashMap<String, String>` with keys in sorted (lexicographic) order.
///
/// Used on `Solution::prices` to ensure the same auction always produces
/// byte-identical JSON output regardless of HashMap internal ordering.
fn serialize_prices_sorted<S>(
    map: &HashMap<TokenAddress, String>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeMap;
    let mut sorted: Vec<(&TokenAddress, &String)> = map.iter().collect();
    sorted.sort_by_key(|(k, _)| k.as_str());
    let mut ser = serializer.serialize_map(Some(sorted.len()))?;
    for (k, v) in sorted {
        ser.serialize_entry(k, v)?;
    }
    ser.end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_response_serializes() {
        let r = SolveResponse::empty();
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"solutions":[]}"#);
    }

    #[test]
    fn solution_round_trip() {
        let mut sol = Solution::new(0);
        sol.prices.insert(
            "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".into(),
            "1000000000000000000".into(),
        );
        sol.prices.insert(
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".into(),
            "266264879219137".into(),
        );
        sol.trades.push(Trade::fulfillment(
            "0x2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a0000000000000001",
            "1000000000000000000",
        ));
        sol.interactions.push(Interaction::Custom(CustomInteraction {
            internalize: false,
            target: "0x7a250d5630b4cf539739df2c5dacb4c659f2488d".into(),
            value: "0".into(),
            call_data: "0xdeadbeef".into(),
        }));
        sol.score = Some(Score::Solver {
            score: "5000000000000000".into(),
        });

        let response = SolveResponse { solutions: vec![sol] };
        let json = serde_json::to_string(&response).unwrap();
        let back: SolveResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(back.solutions.len(), 1);
        assert_eq!(back.solutions[0].id, 0);
        assert_eq!(back.solutions[0].prices.len(), 2);
        assert_eq!(back.solutions[0].trades.len(), 1);
        assert_eq!(back.solutions[0].interactions.len(), 1);
        match &back.solutions[0].score {
            Some(Score::Solver { score }) => assert_eq!(score, "5000000000000000"),
            other => panic!("expected Solver score, got {:?}", other),
        }
    }

    #[test]
    fn score_solver_serializes() {
        let s = Score::Solver {
            score: "42000000".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""kind":"solver""#));
        assert!(json.contains(r#""score":"42000000""#));
    }

    #[test]
    fn score_risk_adjusted_serializes() {
        let s = Score::RiskAdjusted {
            success_probability: 0.95,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""kind":"riskAdjusted""#));
        assert!(json.contains("0.95"));
    }

    #[test]
    fn trade_fulfillment_tagged() {
        let t = Trade::fulfillment("0xuid", "1000");
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(r#""kind":"fulfillment""#));
        assert!(json.contains(r#""order":"0xuid""#));
    }

    #[test]
    fn interaction_custom_tagged() {
        let i = Interaction::Custom(CustomInteraction {
            internalize: false,
            target: "0xaddr".into(),
            value: "0".into(),
            call_data: "0xdata".into(),
        });
        let json = serde_json::to_string(&i).unwrap();
        assert!(json.contains(r#""kind":"custom""#));
        assert!(json.contains(r#""target":"0xaddr""#));
    }

    #[test]
    fn interaction_liquidity_tagged() {
        let i = Interaction::Liquidity(LiquidityInteraction {
            internalize: false,
            id: "pool-0".into(),
            input_token: "0xtoken0".into(),
            output_token: "0xtoken1".into(),
            input_amount: "1000000".into(),
            output_amount: "2000000".into(),
        });
        let json = serde_json::to_string(&i).unwrap();
        assert!(json.contains(r#""kind":"liquidity""#));
        assert!(json.contains(r#""id":"pool-0""#));
    }
}
