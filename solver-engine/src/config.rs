use std::env;
use thiserror::Error;
use tracing::info;

use crate::chain_config::{self, ChainConfig, ChainConfigError};

#[derive(Debug, Clone)]
pub struct Config {
    /// Ethereum JSON-RPC endpoint
    pub rpc_url: String,
    /// HTTP server listen port (default: 8000)
    pub solver_port: u16,
    /// Target chain ID (1=mainnet, 42161=Arbitrum)
    pub chain_id: u64,
    /// Tracing log level (default: "info")
    pub log_level: String,
    /// Max solve time in milliseconds (default: 25000)
    pub max_solve_time_ms: u64,
    /// CoW Protocol Driver URL (optional)
    pub driver_url: Option<String>,
    /// Chain-specific configuration loaded from TOML (optional — gracefully
    /// degrades to hardcoded defaults if the config file is not found).
    pub chain: Option<ChainConfig>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Missing required environment variable: {0}")]
    MissingVar(String),
    #[error("Invalid value for {var}: {reason}")]
    InvalidValue { var: String, reason: String },
    #[error("Chain config error: {0}")]
    ChainConfig(#[from] ChainConfigError),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let rpc_url = env::var("RPC_URL").map_err(|_| ConfigError::MissingVar("RPC_URL".into()))?;

        let solver_port = env::var("SOLVER_PORT")
            .unwrap_or_else(|_| "8000".into())
            .parse::<u16>()
            .map_err(|e| ConfigError::InvalidValue {
                var: "SOLVER_PORT".into(),
                reason: e.to_string(),
            })?;

        let chain_id = env::var("CHAIN_ID")
            .unwrap_or_else(|_| "1".into())
            .parse::<u64>()
            .map_err(|e| ConfigError::InvalidValue {
                var: "CHAIN_ID".into(),
                reason: e.to_string(),
            })?;

        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());

        let max_solve_time_ms = env::var("MAX_SOLVE_TIME_MS")
            .unwrap_or_else(|_| "25000".into())
            .parse::<u64>()
            .map_err(|e| ConfigError::InvalidValue {
                var: "MAX_SOLVE_TIME_MS".into(),
                reason: e.to_string(),
            })?;

        let driver_url = env::var("DRIVER_URL").ok().filter(|v| !v.is_empty());

        // Attempt to load chain-specific TOML config.
        // This is optional — the solver falls back to hardcoded constants if
        // the config file doesn't exist.
        let chain = match chain_config::load_chain_config(chain_id) {
            Ok(cc) => {
                info!(
                    chain_id = cc.chain_id,
                    chain_name = %cc.chain_name,
                    tokens = cc.tokens.len(),
                    curve_pools = cc.curve_pools.len(),
                    "Loaded chain config from TOML"
                );
                Some(cc)
            }
            Err(e) => {
                tracing::warn!(
                    chain_id,
                    error = %e,
                    "Could not load chain TOML config — using hardcoded defaults"
                );
                None
            }
        };

        Ok(Self {
            rpc_url,
            solver_port,
            chain_id,
            log_level,
            max_solve_time_ms,
            driver_url,
            chain,
        })
    }

    /// Returns rpc_url with the API key (last path segment) redacted.
    pub fn rpc_url_redacted(&self) -> String {
        // Redact everything after the last '/' — that's typically the API key.
        // Falls back to showing only the first 20 chars for non-standard URLs.
        if let Some(idx) = self.rpc_url.rfind('/') {
            let prefix = &self.rpc_url[..idx];
            if prefix.contains("://") {
                return format!("{}/***", prefix);
            }
        }
        let visible = self.rpc_url.chars().take(20).collect::<String>();
        format!("{}***", visible)
    }
}
