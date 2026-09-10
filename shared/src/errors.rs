use thiserror::Error;

#[derive(Debug, Error)]
pub enum SolverError {
    #[error("RPC error: {0}")]
    Rpc(#[from] anyhow::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Arithmetic overflow in {context}")]
    Overflow { context: &'static str },

    #[error("No profitable solution found")]
    NoProfitableSolution,

    #[error("EBBO violation for order {order_uid}")]
    EbboViolation { order_uid: String },

    #[error("Invalid token decimals for {token}: expected ≤18, got {decimals}")]
    InvalidDecimals { token: String, decimals: u8 },
}
