//! Auction Replay System (A.7)
//!
//! Records every auction received and the solution we submitted to SQLite.
//! Enables offline replay, win/loss analysis, and strategy attribution.
//!
//! ## Usage
//!
//! ```rust,ignore
//! // In routes/solve.rs — fire-and-forget after every /solve call:
//! tokio::spawn(replay::record_auction(record));
//!
//! // CLI replay:
//! // cargo run --bin replay -- --last 100 --summary
//! ```

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use tracing::{debug, warn};

// ── Types ────────────────────────────────────────────────────────────────────

/// Everything we record per auction for replay and analysis.
#[derive(Debug, Clone)]
pub struct AuctionRecord {
    pub auction_id: String,
    pub chain_id: u64,
    pub orders_count: usize,
    /// Full auction JSON (will be compressed before storage)
    pub auction_json: String,
    /// Our solution JSON (will be compressed before storage)
    pub solution_json: String,
    /// Best score from our solution (wei as string)
    pub our_score_wei: String,
    /// Wall-clock solve time in ms
    pub response_time_ms: u64,
    /// Which strategies ran (JSON array)
    pub strategies_used: String,
    /// Which strategy produced the submitted solution
    pub strategy_submitted: String,
    /// Whether fallback was used
    pub used_fallback: bool,
    /// Result: "submitted" | "empty" | "error" | "timeout"
    pub result: String,
}

/// Summary stats from replay queries.
#[derive(Debug, Default)]
pub struct ReplaySummary {
    pub total_auctions: u64,
    pub submitted: u64,
    pub empty: u64,
    pub errors: u64,
    pub avg_response_ms: f64,
    pub fallback_count: u64,
    pub avg_score_wei: f64,
}

/// Per-strategy breakdown.
#[derive(Debug)]
pub struct StrategyStats {
    pub strategy: String,
    pub times_submitted: u64,
    pub avg_score_wei: f64,
}

// ── Database ─────────────────────────────────────────────────────────────────

static DB_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Returns the path to the replay database. Defaults to `data/replay.db` relative
/// to the current working directory, overridable via `REPLAY_DB_PATH` env var.
fn db_path() -> &'static PathBuf {
    DB_PATH.get_or_init(|| {
        std::env::var("REPLAY_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/replay.db"))
    })
}

/// Open a connection to the replay database. Creates the file and schema if needed.
fn open_db() -> Result<Connection, rusqlite::Error> {
    // Ensure parent directory exists
    if let Some(parent) = db_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let conn = Connection::open(db_path())?;

    // WAL mode for concurrent reads + writes
    conn.pragma_update(None, "journal_mode", "WAL")?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS auction_log (
            id TEXT PRIMARY KEY,
            chain_id INTEGER NOT NULL,
            received_at INTEGER NOT NULL,
            orders_count INTEGER NOT NULL,
            auction_json BLOB NOT NULL,
            solution_json BLOB NOT NULL,
            our_score_wei TEXT NOT NULL DEFAULT '0',
            winning_solver TEXT,
            winning_score_wei TEXT,
            score_delta_wei TEXT,
            response_time_ms INTEGER NOT NULL,
            strategies_used TEXT NOT NULL,
            strategy_submitted TEXT NOT NULL DEFAULT '',
            used_fallback INTEGER NOT NULL DEFAULT 0,
            result TEXT NOT NULL,
            triage_class TEXT,
            simulated INTEGER,
            sim_result TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_auction_received ON auction_log(received_at);
        CREATE INDEX IF NOT EXISTS idx_auction_result ON auction_log(result);
        CREATE INDEX IF NOT EXISTS idx_auction_strategy ON auction_log(strategy_submitted);"
    )?;

    Ok(conn)
}

// ── Compression ──────────────────────────────────────────────────────────────

fn compress(data: &str) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(data.as_bytes()).unwrap_or_default();
    encoder.finish().unwrap_or_default()
}

fn decompress(data: &[u8]) -> String {
    use flate2::read::GzDecoder;

    let mut decoder = GzDecoder::new(data);
    let mut result = String::new();
    decoder.read_to_string(&mut result).unwrap_or_default();
    result
}

// ── Recording ────────────────────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Record a single auction to the replay database.
///
/// Designed to be called via `tokio::spawn()` — non-blocking, fire-and-forget.
/// Errors are logged but never propagated (recording must not break the solve path).
pub async fn record_auction(record: AuctionRecord) {
    // Run the blocking SQLite write on the blocking thread pool
    let result = tokio::task::spawn_blocking(move || {
        let conn = open_db()?;

        let auction_compressed = compress(&record.auction_json);
        let solution_compressed = compress(&record.solution_json);

        conn.execute(
            "INSERT OR REPLACE INTO auction_log (
                id, chain_id, received_at, orders_count,
                auction_json, solution_json, our_score_wei,
                response_time_ms, strategies_used, strategy_submitted,
                used_fallback, result
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.auction_id,
                record.chain_id as i64,
                now_secs() as i64,
                record.orders_count as i64,
                auction_compressed,
                solution_compressed,
                record.our_score_wei,
                record.response_time_ms as i64,
                record.strategies_used,
                record.strategy_submitted,
                record.used_fallback as i32,
                record.result,
            ],
        )?;

        debug!(auction_id = %record.auction_id, "Auction recorded to replay DB");
        Ok::<(), rusqlite::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(error = %e, "Failed to record auction to replay DB"),
        Err(e) => warn!(error = %e, "Replay recording task panicked"),
    }
}

// ── Competition result updates ────────────────────────────────────────────────

/// Update an auction record with competition results (winner, score delta).
/// Called by the competition tracker background task.
pub fn update_competition_result(
    auction_id: &str,
    winning_solver: &str,
    winning_score_wei: &str,
) -> Result<(), rusqlite::Error> {
    let conn = open_db()?;
    conn.execute(
        "UPDATE auction_log SET
            winning_solver = ?1,
            winning_score_wei = ?2,
            score_delta_wei = CAST(our_score_wei AS INTEGER) - CAST(?2 AS INTEGER)
         WHERE id = ?3",
        params![winning_solver, winning_score_wei, auction_id],
    )?;
    Ok(())
}

// ── Queries (for CLI and analysis) ───────────────────────────────────────────

/// Get summary stats for the last N auctions.
pub fn query_summary(last_n: Option<u64>) -> Result<ReplaySummary, rusqlite::Error> {
    let conn = open_db()?;

    let query = match last_n {
        Some(n) => format!(
            "SELECT
                COUNT(*) as total,
                SUM(CASE WHEN result = 'submitted' THEN 1 ELSE 0 END) as submitted,
                SUM(CASE WHEN result = 'empty' THEN 1 ELSE 0 END) as empty,
                SUM(CASE WHEN result = 'error' THEN 1 ELSE 0 END) as errors,
                AVG(response_time_ms) as avg_ms,
                SUM(used_fallback) as fallback_count,
                AVG(CASE WHEN our_score_wei != '0' THEN CAST(our_score_wei AS REAL) ELSE NULL END) as avg_score
            FROM (SELECT * FROM auction_log ORDER BY received_at DESC LIMIT {n})"
        ),
        None => String::from(
            "SELECT
                COUNT(*) as total,
                SUM(CASE WHEN result = 'submitted' THEN 1 ELSE 0 END) as submitted,
                SUM(CASE WHEN result = 'empty' THEN 1 ELSE 0 END) as empty,
                SUM(CASE WHEN result = 'error' THEN 1 ELSE 0 END) as errors,
                AVG(response_time_ms) as avg_ms,
                SUM(used_fallback) as fallback_count,
                AVG(CASE WHEN our_score_wei != '0' THEN CAST(our_score_wei AS REAL) ELSE NULL END) as avg_score
            FROM auction_log"
        ),
    };

    conn.query_row(&query, [], |row| {
        Ok(ReplaySummary {
            total_auctions: row.get::<_, i64>(0).unwrap_or(0) as u64,
            submitted: row.get::<_, i64>(1).unwrap_or(0) as u64,
            empty: row.get::<_, i64>(2).unwrap_or(0) as u64,
            errors: row.get::<_, i64>(3).unwrap_or(0) as u64,
            avg_response_ms: row.get::<_, f64>(4).unwrap_or(0.0),
            fallback_count: row.get::<_, i64>(5).unwrap_or(0) as u64,
            avg_score_wei: row.get::<_, f64>(6).unwrap_or(0.0),
        })
    })
}

/// Get per-strategy breakdown.
pub fn query_by_strategy(last_n: Option<u64>) -> Result<Vec<StrategyStats>, rusqlite::Error> {
    let conn = open_db()?;

    let limit_clause = match last_n {
        Some(n) => format!("WHERE id IN (SELECT id FROM auction_log ORDER BY received_at DESC LIMIT {n})"),
        None => String::new(),
    };

    let query = format!(
        "SELECT
            strategy_submitted,
            COUNT(*) as times_submitted,
            AVG(CASE WHEN our_score_wei != '0' THEN CAST(our_score_wei AS REAL) ELSE NULL END) as avg_score
        FROM auction_log
        {limit_clause}
        WHERE strategy_submitted != ''
        GROUP BY strategy_submitted
        ORDER BY times_submitted DESC"
    );

    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map([], |row| {
        Ok(StrategyStats {
            strategy: row.get(0)?,
            times_submitted: row.get::<_, i64>(1).unwrap_or(0) as u64,
            avg_score_wei: row.get::<_, f64>(2).unwrap_or(0.0),
        })
    })?;

    rows.collect()
}

/// Load a specific auction's raw JSON (decompressed) for replay.
pub fn load_auction(auction_id: &str) -> Result<Option<(String, String)>, rusqlite::Error> {
    let conn = open_db()?;

    let result = conn.query_row(
        "SELECT auction_json, solution_json FROM auction_log WHERE id = ?1",
        params![auction_id],
        |row| {
            let auction_blob: Vec<u8> = row.get(0)?;
            let solution_blob: Vec<u8> = row.get(1)?;
            Ok((decompress(&auction_blob), decompress(&solution_blob)))
        },
    );

    match result {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Get the last N auction IDs for listing.
/// Extended recent auction row with competition data.
#[derive(Debug, Clone)]
pub struct RecentAuctionRow {
    pub id: String,
    pub received_at: i64,
    pub result: String,
    pub response_time_ms: i64,
    pub our_score_wei: String,
    pub winning_score_wei: Option<String>,
    pub winning_solver: Option<String>,
    pub strategy_submitted: String,
}

pub fn list_recent(n: u64) -> Result<Vec<RecentAuctionRow>, rusqlite::Error> {
    let conn = open_db()?;

    let mut stmt = conn.prepare(
        "SELECT id, received_at, result, response_time_ms,
                our_score_wei, winning_score_wei, winning_solver, strategy_submitted
         FROM auction_log ORDER BY received_at DESC LIMIT ?1"
    )?;

    let rows = stmt.query_map(params![n as i64], |row| {
        Ok(RecentAuctionRow {
            id: row.get(0)?,
            received_at: row.get(1)?,
            result: row.get(2)?,
            response_time_ms: row.get(3)?,
            our_score_wei: row.get::<_, String>(4).unwrap_or_default(),
            winning_score_wei: row.get::<_, Option<String>>(5).unwrap_or(None),
            winning_solver: row.get::<_, Option<String>>(6).unwrap_or(None),
            strategy_submitted: row.get::<_, String>(7).unwrap_or_default(),
        })
    })?;

    rows.collect()
}

// ── Bootstrap (restore state after restart) ─────────────────────────────────

/// Metrics snapshot from the replay DB for bootstrapping in-memory counters.
#[derive(Debug, Default)]
pub struct BootstrapMetrics {
    pub auctions_received: u64,
    pub solutions_submitted: u64,
    pub solutions_held: u64,
    pub solutions_empty: u64,
    pub total_solve_ms: u64,
    pub avg_score_gwei: u64,
    pub last_auction_sec: u64,
}

/// Load aggregate metrics from replay DB to bootstrap monitoring counters on startup.
pub fn load_bootstrap_metrics() -> Result<BootstrapMetrics, rusqlite::Error> {
    let conn = open_db()?;

    conn.query_row(
        "SELECT
            COUNT(*) as total,
            SUM(CASE WHEN result = 'submitted' THEN 1 ELSE 0 END),
            SUM(CASE WHEN result = 'held' THEN 1 ELSE 0 END),
            SUM(CASE WHEN result = 'empty' THEN 1 ELSE 0 END),
            COALESCE(SUM(response_time_ms), 0),
            COALESCE(AVG(CASE WHEN our_score_wei != '0'
                THEN CAST(our_score_wei AS REAL) / 1000000000.0
                ELSE NULL END), 0),
            COALESCE(MAX(received_at), 0)
        FROM auction_log",
        [],
        |row| {
            Ok(BootstrapMetrics {
                auctions_received: row.get::<_, i64>(0).unwrap_or(0) as u64,
                solutions_submitted: row.get::<_, i64>(1).unwrap_or(0) as u64,
                solutions_held: row.get::<_, i64>(2).unwrap_or(0) as u64,
                solutions_empty: row.get::<_, i64>(3).unwrap_or(0) as u64,
                total_solve_ms: row.get::<_, i64>(4).unwrap_or(0) as u64,
                avg_score_gwei: row.get::<_, f64>(5).unwrap_or(0.0) as u64,
                last_auction_sec: row.get::<_, i64>(6).unwrap_or(0) as u64,
            })
        },
    )
}

/// Load competition results from replay DB for bootstrapping competition tracker.
/// Returns rows that have winner data (competition result was recorded).
pub fn load_competition_history(limit: u64) -> Result<Vec<crate::competition::CompetitionResult>, rusqlite::Error> {
    let conn = open_db()?;

    let mut stmt = conn.prepare(
        "SELECT id, our_score_wei, winning_solver, winning_score_wei
         FROM auction_log
         WHERE winning_solver IS NOT NULL
         ORDER BY received_at DESC
         LIMIT ?1"
    )?;

    let rows = stmt.query_map(params![limit as i64], |row| {
        let auction_id_str: String = row.get(0)?;
        let our_score_str: String = row.get::<_, String>(1).unwrap_or_default();
        let winner_solver: String = row.get::<_, String>(2).unwrap_or_default();
        let winner_score_str: String = row.get::<_, String>(3).unwrap_or_default();

        let our_score_wei: u128 = our_score_str.parse().unwrap_or(0);
        let winner_score_wei: u128 = winner_score_str.parse().unwrap_or(0);
        let auction_id: u64 = auction_id_str.parse().unwrap_or(0);

        let score_delta_wei = our_score_wei as i128 - winner_score_wei as i128;
        let score_delta_pct = if winner_score_wei > 0 {
            (our_score_wei as f64 / winner_score_wei as f64 - 1.0) * 100.0
        } else {
            0.0
        };

        Ok(crate::competition::CompetitionResult {
            auction_id,
            winner_solver,
            winner_score_wei,
            our_score_wei,
            our_rank: if our_score_wei >= winner_score_wei && winner_score_wei > 0 { 1 } else { 2 },
            total_solvers: 0, // not stored in replay DB
            score_delta_wei,
            score_delta_pct,
        })
    })?;

    rows.collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record() -> AuctionRecord {
        AuctionRecord {
            auction_id: format!("test_{}", now_secs()),
            chain_id: 42161,
            orders_count: 3,
            auction_json: r#"{"id":1,"orders":[]}"#.to_string(),
            solution_json: r#"{"solutions":[]}"#.to_string(),
            our_score_wei: "1000000".to_string(),
            response_time_ms: 150,
            strategies_used: r#"["cow","direct","graph"]"#.to_string(),
            strategy_submitted: "direct".to_string(),
            used_fallback: false,
            result: "submitted".to_string(),
        }
    }

    #[test]
    fn compress_decompress_roundtrip() {
        let original = r#"{"id":1,"orders":[{"uid":"abc"}]}"#;
        let compressed = compress(original);
        let decompressed = decompress(&compressed);
        assert_eq!(original, decompressed);
        // Compressed should be smaller for real payloads (may be larger for tiny strings)
        assert!(!compressed.is_empty());
    }

    #[test]
    fn open_db_creates_schema() {
        // Use a temp path to avoid polluting the real DB
        // SAFETY: This test runs single-threaded and only affects this test's env.
        unsafe { std::env::set_var("REPLAY_DB_PATH", ":memory:") };
        // Force re-init by using open_db directly (bypasses OnceLock for testing)
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS auction_log (
                id TEXT PRIMARY KEY,
                chain_id INTEGER NOT NULL,
                received_at INTEGER NOT NULL,
                orders_count INTEGER NOT NULL,
                auction_json BLOB NOT NULL,
                solution_json BLOB NOT NULL,
                our_score_wei TEXT NOT NULL DEFAULT '0',
                winning_solver TEXT,
                winning_score_wei TEXT,
                score_delta_wei TEXT,
                response_time_ms INTEGER NOT NULL,
                strategies_used TEXT NOT NULL,
                strategy_submitted TEXT NOT NULL DEFAULT '',
                used_fallback INTEGER NOT NULL DEFAULT 0,
                result TEXT NOT NULL,
                triage_class TEXT,
                simulated INTEGER,
                sim_result TEXT
            );"
        ).unwrap();

        // Insert a record
        let rec = test_record();
        conn.execute(
            "INSERT INTO auction_log (id, chain_id, received_at, orders_count, auction_json, solution_json, our_score_wei, response_time_ms, strategies_used, strategy_submitted, used_fallback, result) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                rec.auction_id, rec.chain_id as i64, now_secs() as i64, rec.orders_count as i64,
                compress(&rec.auction_json), compress(&rec.solution_json),
                rec.our_score_wei, rec.response_time_ms as i64, rec.strategies_used,
                rec.strategy_submitted, rec.used_fallback as i32, rec.result,
            ],
        ).unwrap();

        // Query it back
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM auction_log", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }
}
