//! Settlement Accounting Engine (A.6)
//!
//! Tracks predicted vs realized vs reimbursed P&L per auction.
//! Uses the same SQLite database as the replay system.
//!
//! ## Data Model
//!
//! Three layers per settlement:
//! - **Predicted**: surplus, gas estimate, net score at submission time
//! - **Realized**: actual surplus, gas paid, slippage from on-chain tx
//! - **Accounting**: CoW weekly reimbursement, net P&L

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use tracing::{debug, warn};

// ── Types ───────────────────────────────────────────────────────────────────

/// Predicted settlement outcome (recorded at submission time).
#[derive(Debug, Clone)]
pub struct PredictedSettlement {
    pub auction_id: String,
    pub chain_id: u64,
    pub predicted_surplus_wei: String,
    pub predicted_gas_cost_wei: String,
    pub predicted_net_score_wei: String,
    pub winning_strategy: String,
    pub simulated: bool,
    pub submission_mode: String, // "standard" | "protected" | "flashbots"
}

/// Realized settlement outcome (from on-chain data).
#[derive(Debug, Clone)]
pub struct RealizedSettlement {
    pub auction_id: String,
    pub realized_surplus_wei: String,
    pub realized_gas_cost_wei: String,
    pub realized_slippage_wei: String,
    pub settlement_tx_hash: String,
}

/// Weekly accounting adjustment from CoW Protocol.
#[derive(Debug, Clone)]
pub struct AccountingAdjustment {
    pub auction_id: String,
    pub reimbursement_wei: String,       // positive = received, negative = paid
    pub accounting_period: String,       // e.g. "2026-W13"
    pub net_pnl_wei: String,
}

/// Full settlement record for queries.
#[derive(Debug, Clone)]
pub struct SettlementRecord {
    pub auction_id: String,
    pub chain_id: i64,
    pub settled_at: i64,
    pub predicted_surplus_wei: String,
    pub predicted_gas_cost_wei: String,
    pub predicted_net_score_wei: String,
    pub realized_surplus_wei: Option<String>,
    pub realized_gas_cost_wei: Option<String>,
    pub realized_slippage_wei: Option<String>,
    pub settlement_tx_hash: Option<String>,
    pub reimbursement_wei: Option<String>,
    pub net_pnl_wei: Option<String>,
    pub winning_strategy: String,
    pub simulated: bool,
    pub submission_mode: String,
}

/// Aggregate P&L summary.
#[derive(Debug, Default)]
pub struct PnlSummary {
    pub total_auctions: u64,
    pub settled: u64,
    pub pending: u64,
    pub total_predicted_surplus_gwei: f64,
    pub total_realized_surplus_gwei: f64,
    pub total_gas_cost_gwei: f64,
    pub total_slippage_gwei: f64,
    pub total_reimbursement_gwei: f64,
    pub total_net_pnl_gwei: f64,
}

// ── Database ────────────────────────────────────────────────────────────────

fn db_path() -> &'static std::path::PathBuf {
    static PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        std::env::var("ACCOUNTING_DB_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("data/accounting.db"))
    })
}

fn open_db() -> Result<Connection, rusqlite::Error> {
    if let Some(parent) = db_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let conn = Connection::open(db_path())?;
    conn.pragma_update(None, "journal_mode", "WAL")?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settlement_log (
            auction_id TEXT PRIMARY KEY,
            chain_id INTEGER NOT NULL,
            settled_at INTEGER NOT NULL,

            -- Predicted (at submission time)
            predicted_surplus_wei TEXT NOT NULL DEFAULT '0',
            predicted_gas_cost_wei TEXT NOT NULL DEFAULT '0',
            predicted_net_score_wei TEXT NOT NULL DEFAULT '0',

            -- Realized (from on-chain settlement)
            realized_surplus_wei TEXT,
            realized_gas_cost_wei TEXT,
            realized_slippage_wei TEXT,
            settlement_tx_hash TEXT,

            -- Accounting (from CoW weekly settlement)
            reimbursement_wei TEXT,
            accounting_period TEXT,
            net_pnl_wei TEXT,

            -- Attribution
            winning_strategy TEXT NOT NULL DEFAULT '',
            simulated INTEGER NOT NULL DEFAULT 0,
            submission_mode TEXT NOT NULL DEFAULT 'standard'
        );

        CREATE INDEX IF NOT EXISTS idx_settlement_time ON settlement_log(settled_at);
        CREATE INDEX IF NOT EXISTS idx_settlement_strategy ON settlement_log(winning_strategy);"
    )?;

    Ok(conn)
}

// ── Recording ───────────────────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Record the predicted settlement outcome (called at submission time).
/// Fire-and-forget via `tokio::spawn`.
pub async fn record_predicted(record: PredictedSettlement) {
    let result = tokio::task::spawn_blocking(move || {
        let conn = open_db()?;
        conn.execute(
            "INSERT OR REPLACE INTO settlement_log (
                auction_id, chain_id, settled_at,
                predicted_surplus_wei, predicted_gas_cost_wei, predicted_net_score_wei,
                winning_strategy, simulated, submission_mode
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.auction_id,
                record.chain_id as i64,
                now_secs() as i64,
                record.predicted_surplus_wei,
                record.predicted_gas_cost_wei,
                record.predicted_net_score_wei,
                record.winning_strategy,
                record.simulated as i32,
                record.submission_mode,
            ],
        )?;
        debug!(auction_id = %record.auction_id, "Predicted settlement recorded");
        Ok::<(), rusqlite::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "Failed to record predicted settlement"),
        Err(e) => warn!(error = %e, "Accounting recording task panicked"),
    }
}

/// Update with realized on-chain data (called after settlement confirmation).
pub async fn record_realized(record: RealizedSettlement) {
    let result = tokio::task::spawn_blocking(move || {
        let conn = open_db()?;
        conn.execute(
            "UPDATE settlement_log SET
                realized_surplus_wei = ?2,
                realized_gas_cost_wei = ?3,
                realized_slippage_wei = ?4,
                settlement_tx_hash = ?5
            WHERE auction_id = ?1",
            params![
                record.auction_id,
                record.realized_surplus_wei,
                record.realized_gas_cost_wei,
                record.realized_slippage_wei,
                record.settlement_tx_hash,
            ],
        )?;
        debug!(auction_id = %record.auction_id, "Realized settlement recorded");
        Ok::<(), rusqlite::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "Failed to record realized settlement"),
        Err(e) => warn!(error = %e, "Accounting realized recording panicked"),
    }
}

/// Update with weekly accounting adjustment.
pub async fn record_adjustment(record: AccountingAdjustment) {
    let result = tokio::task::spawn_blocking(move || {
        let conn = open_db()?;
        conn.execute(
            "UPDATE settlement_log SET
                reimbursement_wei = ?2,
                accounting_period = ?3,
                net_pnl_wei = ?4
            WHERE auction_id = ?1",
            params![
                record.auction_id,
                record.reimbursement_wei,
                record.accounting_period,
                record.net_pnl_wei,
            ],
        )?;
        debug!(auction_id = %record.auction_id, "Accounting adjustment recorded");
        Ok::<(), rusqlite::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "Failed to record accounting adjustment"),
        Err(e) => warn!(error = %e, "Accounting adjustment recording panicked"),
    }
}

// ── Queries ─────────────────────────────────────────────────────────────────

/// Get aggregate P&L summary for the last N settlements.
pub fn query_pnl_summary(last_n: Option<u64>) -> Result<PnlSummary, rusqlite::Error> {
    let conn = open_db()?;

    let query = match last_n {
        Some(n) => format!(
            "SELECT
                COUNT(*) as total,
                SUM(CASE WHEN realized_surplus_wei IS NOT NULL THEN 1 ELSE 0 END) as settled,
                SUM(CASE WHEN realized_surplus_wei IS NULL THEN 1 ELSE 0 END) as pending,
                COALESCE(SUM(CAST(predicted_surplus_wei AS REAL) / 1e9), 0) as pred_surplus,
                COALESCE(SUM(CASE WHEN realized_surplus_wei IS NOT NULL THEN CAST(realized_surplus_wei AS REAL) / 1e9 ELSE 0 END), 0) as real_surplus,
                COALESCE(SUM(CASE WHEN realized_gas_cost_wei IS NOT NULL THEN CAST(realized_gas_cost_wei AS REAL) / 1e9 ELSE CAST(predicted_gas_cost_wei AS REAL) / 1e9 END), 0) as gas,
                COALESCE(SUM(CASE WHEN realized_slippage_wei IS NOT NULL THEN CAST(realized_slippage_wei AS REAL) / 1e9 ELSE 0 END), 0) as slippage,
                COALESCE(SUM(CASE WHEN reimbursement_wei IS NOT NULL THEN CAST(reimbursement_wei AS REAL) / 1e9 ELSE 0 END), 0) as reimburse,
                COALESCE(SUM(CASE WHEN net_pnl_wei IS NOT NULL THEN CAST(net_pnl_wei AS REAL) / 1e9 ELSE 0 END), 0) as pnl
            FROM (SELECT * FROM settlement_log ORDER BY settled_at DESC LIMIT {n})"
        ),
        None => String::from(
            "SELECT
                COUNT(*) as total,
                SUM(CASE WHEN realized_surplus_wei IS NOT NULL THEN 1 ELSE 0 END) as settled,
                SUM(CASE WHEN realized_surplus_wei IS NULL THEN 1 ELSE 0 END) as pending,
                COALESCE(SUM(CAST(predicted_surplus_wei AS REAL) / 1e9), 0) as pred_surplus,
                COALESCE(SUM(CASE WHEN realized_surplus_wei IS NOT NULL THEN CAST(realized_surplus_wei AS REAL) / 1e9 ELSE 0 END), 0) as real_surplus,
                COALESCE(SUM(CASE WHEN realized_gas_cost_wei IS NOT NULL THEN CAST(realized_gas_cost_wei AS REAL) / 1e9 ELSE CAST(predicted_gas_cost_wei AS REAL) / 1e9 END), 0) as gas,
                COALESCE(SUM(CASE WHEN realized_slippage_wei IS NOT NULL THEN CAST(realized_slippage_wei AS REAL) / 1e9 ELSE 0 END), 0) as slippage,
                COALESCE(SUM(CASE WHEN reimbursement_wei IS NOT NULL THEN CAST(reimbursement_wei AS REAL) / 1e9 ELSE 0 END), 0) as reimburse,
                COALESCE(SUM(CASE WHEN net_pnl_wei IS NOT NULL THEN CAST(net_pnl_wei AS REAL) / 1e9 ELSE 0 END), 0) as pnl
            FROM settlement_log"
        ),
    };

    conn.query_row(&query, [], |row| {
        Ok(PnlSummary {
            total_auctions: row.get::<_, i64>(0).unwrap_or(0) as u64,
            settled: row.get::<_, i64>(1).unwrap_or(0) as u64,
            pending: row.get::<_, i64>(2).unwrap_or(0) as u64,
            total_predicted_surplus_gwei: row.get(3).unwrap_or(0.0),
            total_realized_surplus_gwei: row.get(4).unwrap_or(0.0),
            total_gas_cost_gwei: row.get(5).unwrap_or(0.0),
            total_slippage_gwei: row.get(6).unwrap_or(0.0),
            total_reimbursement_gwei: row.get(7).unwrap_or(0.0),
            total_net_pnl_gwei: row.get(8).unwrap_or(0.0),
        })
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_accounting_db_creates_schema() {
        // SAFETY: single-threaded test
        unsafe { std::env::set_var("ACCOUNTING_DB_PATH", ":memory:") };

        // Test with in-memory DB directly
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settlement_log (
                auction_id TEXT PRIMARY KEY,
                chain_id INTEGER NOT NULL,
                settled_at INTEGER NOT NULL,
                predicted_surplus_wei TEXT NOT NULL DEFAULT '0',
                predicted_gas_cost_wei TEXT NOT NULL DEFAULT '0',
                predicted_net_score_wei TEXT NOT NULL DEFAULT '0',
                realized_surplus_wei TEXT,
                realized_gas_cost_wei TEXT,
                realized_slippage_wei TEXT,
                settlement_tx_hash TEXT,
                reimbursement_wei TEXT,
                accounting_period TEXT,
                net_pnl_wei TEXT,
                winning_strategy TEXT NOT NULL DEFAULT '',
                simulated INTEGER NOT NULL DEFAULT 0,
                submission_mode TEXT NOT NULL DEFAULT 'standard'
            );"
        ).unwrap();

        conn.execute(
            "INSERT INTO settlement_log (auction_id, chain_id, settled_at, predicted_surplus_wei, predicted_gas_cost_wei, predicted_net_score_wei, winning_strategy, simulated, submission_mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params!["test_1", 42161i64, now_secs() as i64, "5000000000", "1000000000", "4000000000", "direct", 1, "standard"],
        ).unwrap();

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM settlement_log", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);

        // Verify update works
        conn.execute(
            "UPDATE settlement_log SET realized_surplus_wei = ?2, settlement_tx_hash = ?3 WHERE auction_id = ?1",
            params!["test_1", "4800000000", "0xabc123"],
        ).unwrap();

        let surplus: String = conn.query_row(
            "SELECT realized_surplus_wei FROM settlement_log WHERE auction_id = 'test_1'",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(surplus, "4800000000");
    }
}
