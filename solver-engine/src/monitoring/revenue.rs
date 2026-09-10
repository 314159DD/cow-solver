//! Revenue tracking for the CoW solver.
//!
//! Maintains a daily ledger of auctions won, surplus earned, gas spent, and
//! net P&L. Persisted as JSON so metrics survive solver restarts.
//!
//! # Usage
//!
//! ```rust,ignore
//! use solver_engine::monitoring::revenue::RevenueTracker;
//!
//! let mut tracker = RevenueTracker::load_or_new();
//! tracker.record_win(surplus_wei, gas_wei);
//! tracker.save().unwrap();
//! tracker.log_daily_summary();
//! ```
//!
//! # Persistence
//!
//! Stored at `REVENUE_FILE` (env var) or `./data/revenue.json` by default.
//! The file is a JSON array of `DailyRevenue` records, one per UTC day.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::{env, fs};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Revenue summary for one UTC day (format: "YYYY-MM-DD").
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DailyRevenue {
    /// UTC date string (YYYY-MM-DD)
    pub date: String,
    /// Number of auctions won (non-empty solution returned)
    pub auctions_won: u64,
    /// Total surplus earned across all wins (wei, as decimal string to avoid precision loss)
    pub surplus_wei: String,
    /// Total gas spent across all interactions (wei, as decimal string)
    pub gas_wei: String,
    /// Net P&L = surplus - gas (wei, signed, as decimal string)
    pub net_pnl_wei: String,

    // Internal accumulators (i128 avoids overflow for reasonable daily volumes)
    #[serde(skip)]
    pub surplus_raw: i128,
    #[serde(skip)]
    pub gas_raw: i128,
}

impl DailyRevenue {
    pub fn new(date: impl Into<String>) -> Self {
        Self {
            date: date.into(),
            ..Default::default()
        }
    }

    /// Record a winning solution.
    pub fn record_win(&mut self, surplus_wei: u128, gas_wei: u128) {
        self.auctions_won += 1;
        self.surplus_raw += surplus_wei as i128;
        self.gas_raw += gas_wei as i128;
        self.refresh_strings();
    }

    fn refresh_strings(&mut self) {
        self.surplus_wei = self.surplus_raw.to_string();
        self.gas_wei = self.gas_raw.to_string();
        let net = self.surplus_raw - self.gas_raw;
        self.net_pnl_wei = net.to_string();
    }

    /// Net P&L in ETH (float, for display only — don't use for on-chain math).
    pub fn net_pnl_eth(&self) -> f64 {
        let net: i128 = self.net_pnl_wei.parse().unwrap_or(0);
        net as f64 / 1e18
    }

    /// True if this day is profitable.
    pub fn is_profitable(&self) -> bool {
        !self.net_pnl_wei.starts_with('-') && self.net_pnl_wei != "0"
    }
}

/// Persistent revenue tracker.
pub struct RevenueTracker {
    /// Daily records, keyed by YYYY-MM-DD.
    days: BTreeMap<String, DailyRevenue>,
    /// Path to the JSON persistence file.
    file_path: PathBuf,
}

impl RevenueTracker {
    /// Load from disk, or create a fresh tracker if the file doesn't exist.
    pub fn load_or_new() -> Self {
        let file_path = revenue_file_path();
        let mut tracker = Self {
            days: BTreeMap::new(),
            file_path: file_path.clone(),
        };

        if file_path.exists() {
            match fs::read_to_string(&file_path) {
                Ok(contents) => match serde_json::from_str::<Vec<DailyRevenue>>(&contents) {
                    Ok(records) => {
                        for mut record in records {
                            // Restore raw accumulators from string fields
                            record.surplus_raw = record.surplus_wei.parse().unwrap_or(0);
                            record.gas_raw = record.gas_wei.parse().unwrap_or(0);
                            tracker.days.insert(record.date.clone(), record);
                        }
                        info!(
                            file = %file_path.display(),
                            days_loaded = tracker.days.len(),
                            "Revenue history loaded from disk"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, file = %file_path.display(), "Failed to parse revenue file — starting fresh");
                    }
                },
                Err(e) => {
                    warn!(error = %e, "Failed to read revenue file — starting fresh");
                }
            }
        } else {
            info!(file = %file_path.display(), "No revenue file found — starting fresh tracker");
        }

        tracker
    }

    /// Record a winning auction for today.
    pub fn record_win(&mut self, surplus_wei: u128, gas_wei: u128) {
        let today = today_date();
        let entry = self.days.entry(today.clone()).or_insert_with(|| DailyRevenue::new(&today));
        entry.record_win(surplus_wei, gas_wei);
    }

    /// Get today's revenue summary.
    pub fn today(&self) -> Option<&DailyRevenue> {
        self.days.get(&today_date())
    }

    /// Total lifetime net P&L in wei.
    pub fn lifetime_net_pnl_wei(&self) -> i128 {
        self.days.values().map(|d| {
            d.net_pnl_wei.parse::<i128>().unwrap_or(0)
        }).sum()
    }

    /// Total lifetime wins.
    pub fn lifetime_wins(&self) -> u64 {
        self.days.values().map(|d| d.auctions_won).sum()
    }

    /// Total number of profitable days.
    pub fn profitable_days(&self) -> usize {
        self.days.values().filter(|d| d.is_profitable()).count()
    }

    /// Save all records to disk as JSON.
    pub fn save(&self) -> anyhow::Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let records: Vec<&DailyRevenue> = self.days.values().collect();
        let json = serde_json::to_string_pretty(&records)?;
        fs::write(&self.file_path, json)?;
        Ok(())
    }

    /// Log a daily summary via tracing (INFO level).
    ///
    /// Called at midnight UTC by the background monitoring task.
    pub fn log_daily_summary(&self) {
        let today = today_date();
        let yesterday = self.days.values().rev()
            .find(|d| d.date != today);

        let lifetime_pnl = self.lifetime_net_pnl_wei();
        let lifetime_wins = self.lifetime_wins();
        let profitable_days = self.profitable_days();
        let total_days = self.days.len();

        if let Some(day) = yesterday {
            info!(
                summary = "daily",
                date = %day.date,
                auctions_won = day.auctions_won,
                surplus_eth = day.surplus_raw as f64 / 1e18,
                gas_eth = day.gas_raw as f64 / 1e18,
                net_pnl_eth = day.net_pnl_eth(),
                profitable = day.is_profitable(),
                lifetime_wins,
                lifetime_net_pnl_eth = lifetime_pnl as f64 / 1e18,
                profitable_days,
                total_days,
                "Daily revenue summary"
            );
        } else {
            info!(
                summary = "daily",
                date = %today,
                message = "No completed auction days yet",
                lifetime_wins,
                lifetime_net_pnl_eth = lifetime_pnl as f64 / 1e18,
                "Daily revenue summary"
            );
        }
    }

    /// Returns a snapshot of all daily records (sorted by date).
    pub fn all_days(&self) -> Vec<&DailyRevenue> {
        self.days.values().collect()
    }
}

// ─── Utilities ────────────────────────────────────────────────────────────────

fn today_date() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    unix_secs_to_date(secs)
}

/// Convert Unix seconds to "YYYY-MM-DD" (UTC).
fn unix_secs_to_date(secs: u64) -> String {
    // Simple proleptic Gregorian calendar conversion (no external deps)
    let days = secs / 86400;
    let (y, m, d) = days_to_ymd(days as i64);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Convert days-since-epoch (proleptic Gregorian) to (year, month, day).
fn days_to_ymd(mut days: i64) -> (i32, u32, u32) {
    // Algorithm: https://howardhinnant.github.io/date_algorithms.html
    days += 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn revenue_file_path() -> PathBuf {
    env::var("REVENUE_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/revenue.json"))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_win_updates_fields() {
        let mut day = DailyRevenue::new("2026-03-24");
        day.record_win(1_500_000_000_000_000_000, 100_000_000_000_000_000);
        assert_eq!(day.auctions_won, 1);
        assert_eq!(day.surplus_raw, 1_500_000_000_000_000_000);
        assert_eq!(day.gas_raw, 100_000_000_000_000_000);
        let net: i128 = day.net_pnl_wei.parse().unwrap();
        assert_eq!(net, 1_400_000_000_000_000_000);
        assert!(day.is_profitable());
    }

    #[test]
    fn net_pnl_negative_when_gas_exceeds_surplus() {
        let mut day = DailyRevenue::new("2026-03-24");
        day.record_win(50_000_000_000_000_000, 100_000_000_000_000_000);
        let net: i128 = day.net_pnl_wei.parse().unwrap();
        assert_eq!(net, -50_000_000_000_000_000);
        assert!(!day.is_profitable());
    }

    #[test]
    fn cumulative_across_multiple_wins() {
        let mut day = DailyRevenue::new("2026-03-24");
        day.record_win(1_000_000_000_000_000_000, 50_000_000_000_000_000);
        day.record_win(2_000_000_000_000_000_000, 80_000_000_000_000_000);
        assert_eq!(day.auctions_won, 2);
        assert_eq!(day.surplus_raw, 3_000_000_000_000_000_000);
        assert_eq!(day.gas_raw, 130_000_000_000_000_000);
    }

    #[test]
    fn unix_secs_to_date_known_value() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        assert_eq!(unix_secs_to_date(1704067200), "2024-01-01");
        // 2026-03-24 00:00:00 UTC = 1774310400
        assert_eq!(unix_secs_to_date(1774310400), "2026-03-24");
    }

    #[test]
    fn tracker_lifetime_stats() {
        let mut tracker = RevenueTracker {
            days: Default::default(),
            file_path: PathBuf::from("/tmp/test-revenue.json"),
        };
        tracker.record_win(1_000_000_000_000_000_000, 100_000_000_000_000_000);
        assert_eq!(tracker.lifetime_wins(), 1);
        assert!(tracker.lifetime_net_pnl_wei() > 0);
    }
}
