//! Command Center Dashboard
//!
//! Serves a self-contained HTML dashboard at GET /dashboard and a JSON API
//! at GET /api/stats for the frontend to poll.
//!
//! ## Auth
//! Set `DASHBOARD_TOKEN` env var. Access via `?token=<value>`.
//! If unset, dashboard is open (fine for localhost / VPN).

use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::Query,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{accounting, attribution, freshness, monitoring, pool_indexer, replay, simulation, submission, triage};

// ── Auth ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TokenParam {
    token: Option<String>,
}

pub fn check_auth(params: &TokenParam) -> bool {
    match std::env::var("DASHBOARD_TOKEN") {
        Ok(expected) if !expected.is_empty() => {
            params.token.as_deref() == Some(expected.as_str())
        }
        _ => true, // no token set = open access
    }
}

// ── JSON API ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DashboardData {
    pub timestamp: u64,
    pub uptime_seconds: u64,
    pub overview: Overview,
    pub performance: PerformanceStats,
    pub strategies: Vec<StrategyRow>,
    pub triage: TriageStats,
    pub simulation: SimStats,
    pub freshness: FreshnessStats,
    pub submission: SubmissionStats,
    pub indexer: IndexerStats,
    pub rpc: RpcStats,
    pub recent_auctions: Vec<RecentAuction>,
    pub pnl: Option<PnlStats>,
    pub replay_summary: Option<ReplaySummaryData>,
    pub competitiveness: crate::competition::CompetitivenessStats,
    /// Score anatomy: per-auction breakdown of our score vs winner
    pub score_anatomy: Vec<crate::competition::ScoreAnatomyEntry>,
    /// Auto-diagnosis summary
    pub diagnosis: String,
    /// ETH price in USD cents (e.g. 270000 = $2700.00). 0 if unknown.
    pub eth_price_usd_cents: u64,
}

#[derive(Serialize)]
pub struct PerformanceStats {
    // Histogram bucket counts (upper bound ms) — fine-grained around 200-500ms
    pub hist_50: u64,
    pub hist_100: u64,
    pub hist_150: u64,
    pub hist_200: u64,
    pub hist_250: u64,
    pub hist_300: u64,
    pub hist_350: u64,
    pub hist_400: u64,
    pub hist_500: u64,
    pub hist_750: u64,
    pub hist_1000: u64,
    pub hist_2500: u64,
    pub hist_5000: u64,
    pub hist_10000: u64,
    pub hist_25000: u64,
    pub hist_over: u64,
    // Percentiles (approximate, ms)
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    // Score range (in wei, not gwei — supports sub-gwei scores)
    pub score_min_gwei: u64,
    pub score_max_gwei: u64,
    // Throughput
    pub auctions_per_hour: u64,
}

#[derive(Serialize)]
pub struct Overview {
    pub auctions_received: u64,
    pub solutions_submitted: u64,
    pub solutions_held: u64,
    pub solutions_empty: u64,
    pub win_rate_pct: f64,
    pub avg_solve_ms: u64,
    pub avg_score_gwei: u64,
    pub last_auction_secs_ago: u64,
    pub fallback_used: u64,
}

#[derive(Serialize)]
pub struct StrategyRow {
    pub name: String,
    pub submitted: u64,
    pub generated: u64,
    pub sim_passed: u64,
    pub sim_failed: u64,
    pub policy_approved: u64,
    pub won: u64,
    pub submit_rate: f64,
    pub win_rate: f64,
    pub sim_pass_rate: f64,
}

#[derive(Serialize)]
pub struct TriageStats {
    pub profitable: u64,
    pub marginal: u64,
    pub unwinnable: u64,
    pub skip: u64,
}

#[derive(Serialize)]
pub struct SimStats {
    pub total: u64,
    pub passed: u64,
    pub reverted: u64,
    pub timeout: u64,
    pub skipped: u64,
}

#[derive(Serialize)]
pub struct FreshnessStats {
    pub total: u64,
    pub fresh: u64,
    pub acceptable: u64,
    pub stale: u64,
    pub avg_confidence: f64,
}

#[derive(Serialize)]
pub struct SubmissionStats {
    pub total: u64,
    pub submit: u64,
    pub hold: u64,
    pub replace: u64,
    pub risk_low: u64,
    pub risk_medium: u64,
    pub risk_high: u64,
}

#[derive(Serialize)]
pub struct IndexerStats {
    pub current_block: u64,
    pub pool_cache_size: usize,
}

#[derive(Serialize)]
pub struct RpcStats {
    pub total_calls: u64,
    pub errors: u64,
    pub error_pct: f64,
    pub avg_latency_ms: u64,
}

#[derive(Serialize)]
pub struct RecentAuction {
    pub id: String,
    pub received_at: i64,
    pub result: String,
    pub response_time_ms: i64,
    pub our_score_gwei: f64,
    pub winner_score_gwei: Option<f64>,
    pub winner_solver: Option<String>,
    pub delta_pct: Option<f64>,
    pub strategy: String,
}

#[derive(Serialize)]
pub struct PnlStats {
    pub total_auctions: u64,
    pub settled: u64,
    pub pending: u64,
    pub predicted_surplus_gwei: f64,
    pub realized_surplus_gwei: f64,
    pub gas_cost_gwei: f64,
    pub net_pnl_gwei: f64,
}

#[derive(Serialize)]
pub struct ReplaySummaryData {
    pub total_auctions: u64,
    pub submitted: u64,
    pub empty: u64,
    pub errors: u64,
    pub avg_response_ms: f64,
    pub fallback_count: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn build_dashboard_data() -> DashboardData {
    let m = monitoring::metrics();
    let now = now_secs();

    let auctions = m.auctions_received.load(Ordering::Relaxed);
    let submitted = m.solutions_submitted.load(Ordering::Relaxed);
    let held = m.solutions_held.load(Ordering::Relaxed);
    let empty = m.solutions_empty.load(Ordering::Relaxed);
    let solve_ms = m.total_solve_ms.load(Ordering::Relaxed);
    let solve_n = m.solve_samples.load(Ordering::Relaxed);
    let score_gwei = m.score_sum_gwei.load(Ordering::Relaxed);
    let score_n = m.score_samples.load(Ordering::Relaxed);
    let last_auction = m.last_auction_sec.load(Ordering::Relaxed);
    let rpc_calls = m.rpc_calls.load(Ordering::Relaxed);
    let rpc_errors = m.rpc_errors.load(Ordering::Relaxed);
    let rpc_ms = m.total_rpc_ms.load(Ordering::Relaxed);

    // This is submission rate, NOT win rate. True win rate comes from competition data.
    let submit_rate_pct = if auctions > 0 { submitted as f64 / auctions as f64 * 100.0 } else { 0.0 };
    let avg_solve_ms = if solve_n > 0 { solve_ms / solve_n } else { 0 };
    let avg_score_gwei = if score_n > 0 { score_gwei / score_n } else { 0 };
    let last_auction_secs_ago = if last_auction > 0 { now.saturating_sub(last_auction) } else { 0 };
    let rpc_error_pct = if rpc_calls > 0 { rpc_errors as f64 / rpc_calls as f64 * 100.0 } else { 0.0 };
    let avg_rpc_ms = if rpc_calls > 0 { rpc_ms / rpc_calls } else { 0 };

    // Performance stats
    let hist = monitoring::solve_histogram();
    let (p50, p95, p99) = monitoring::compute_percentiles();
    let (score_min, score_max) = monitoring::score_range();
    let performance = PerformanceStats {
        hist_50:    hist[0],
        hist_100:   hist[1],
        hist_150:   hist[2],
        hist_200:   hist[3],
        hist_250:   hist[4],
        hist_300:   hist[5],
        hist_350:   hist[6],
        hist_400:   hist[7],
        hist_500:   hist[8],
        hist_750:   hist[9],
        hist_1000:  hist[10],
        hist_2500:  hist[11],
        hist_5000:  hist[12],
        hist_10000: hist[13],
        hist_25000: hist[14],
        hist_over:  hist[15],
        p50_ms: p50,
        p95_ms: p95,
        p99_ms: p99,
        score_min_gwei: score_min,
        score_max_gwei: score_max,
        auctions_per_hour: monitoring::auctions_per_hour(),
    };

    // Strategy attribution
    let snapshots = attribution::all_snapshots();
    let strategies: Vec<StrategyRow> = snapshots.into_iter().map(|(name, snap)| {
        let strat_submitted = match name.as_str() {
            "cow" => m.strategy_cow.load(Ordering::Relaxed),
            "direct" => m.strategy_direct.load(Ordering::Relaxed),
            "combined" => m.strategy_combined.load(Ordering::Relaxed),
            "multihop" => m.strategy_multihop.load(Ordering::Relaxed),
            "graph" => m.strategy_graph.load(Ordering::Relaxed),
            "split" => m.strategy_split.load(Ordering::Relaxed),
            "external" => m.strategy_external.load(Ordering::Relaxed),
            "fallback" => m.strategy_fallback.load(Ordering::Relaxed),
            _ => 0,
        };
        StrategyRow {
            name,
            submitted: strat_submitted,
            generated: snap.generated,
            sim_passed: snap.sim_passed,
            sim_failed: snap.sim_failed,
            policy_approved: snap.policy_approved,
            won: snap.won,
            submit_rate: snap.submit_rate(),
            win_rate: snap.win_rate(),
            sim_pass_rate: snap.sim_pass_rate(),
        }
    }).collect();

    // Triage
    let tm = triage::triage_metrics();
    let triage_stats = TriageStats {
        profitable: tm.profitable.load(Ordering::Relaxed),
        marginal: tm.marginal.load(Ordering::Relaxed),
        unwinnable: tm.unwinnable.load(Ordering::Relaxed),
        skip: tm.skip.load(Ordering::Relaxed),
    };

    // Simulation
    let sm = simulation::sim_metrics();
    let sim_stats = SimStats {
        total: sm.total.load(Ordering::Relaxed),
        passed: sm.passed.load(Ordering::Relaxed),
        reverted: sm.reverted.load(Ordering::Relaxed),
        timeout: sm.timeout.load(Ordering::Relaxed),
        skipped: sm.skipped.load(Ordering::Relaxed),
    };

    // Freshness
    let fm = freshness::freshness_metrics();
    let ftotal = fm.total.load(Ordering::Relaxed);
    let fsum = fm.confidence_sum_milli.load(Ordering::Relaxed);
    let freshness_stats = FreshnessStats {
        total: ftotal,
        fresh: fm.fresh_count.load(Ordering::Relaxed),
        acceptable: fm.acceptable_count.load(Ordering::Relaxed),
        stale: fm.stale_count.load(Ordering::Relaxed),
        avg_confidence: if ftotal > 0 { fsum as f64 / ftotal as f64 / 1000.0 } else { 0.0 },
    };

    // Submission
    let sub = submission::submission_metrics();
    let submission_stats = SubmissionStats {
        total: sub.total_decisions.load(Ordering::Relaxed),
        submit: sub.submit_count.load(Ordering::Relaxed),
        hold: sub.hold_count.load(Ordering::Relaxed),
        replace: sub.replace_count.load(Ordering::Relaxed),
        risk_low: sub.risk_low.load(Ordering::Relaxed),
        risk_medium: sub.risk_medium.load(Ordering::Relaxed),
        risk_high: sub.risk_high.load(Ordering::Relaxed),
    };

    // Pool indexer
    let indexer_stats = IndexerStats {
        current_block: pool_indexer::current_block(),
        pool_cache_size: pool_indexer::pool_count(),
    };

    // RPC
    let rpc_stats = RpcStats {
        total_calls: rpc_calls,
        errors: rpc_errors,
        error_pct: rpc_error_pct,
        avg_latency_ms: avg_rpc_ms,
    };

    // Recent auctions from replay DB (blocking, run in current thread for simplicity)
    let recent_auctions = replay::list_recent(500) // enough for ~1h of auctions
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            let our_wei: f64 = row.our_score_wei.parse().unwrap_or(0.0);
            let our_gwei = our_wei / 1_000_000_000.0;
            let winner_gwei = row.winning_score_wei.as_ref()
                .and_then(|s| s.parse::<f64>().ok())
                .map(|w| w / 1_000_000_000.0);
            let delta_pct = winner_gwei.map(|wg| {
                if wg > 0.0 { (our_gwei / wg - 1.0) * 100.0 } else { 0.0 }
            });
            RecentAuction {
                id: row.id,
                received_at: row.received_at,
                result: row.result,
                response_time_ms: row.response_time_ms,
                our_score_gwei: our_gwei,
                winner_score_gwei: winner_gwei,
                winner_solver: row.winning_solver,
                delta_pct,
                strategy: row.strategy_submitted,
            }
        })
        .collect();

    // P&L from accounting DB
    let pnl = accounting::query_pnl_summary(Some(500)).ok().map(|p| PnlStats {
        total_auctions: p.total_auctions,
        settled: p.settled,
        pending: p.pending,
        predicted_surplus_gwei: p.total_predicted_surplus_gwei,
        realized_surplus_gwei: p.total_realized_surplus_gwei,
        gas_cost_gwei: p.total_gas_cost_gwei,
        net_pnl_gwei: p.total_net_pnl_gwei,
    });

    // Replay summary
    let replay_summary = replay::query_summary(Some(1000)).ok().map(|s| ReplaySummaryData {
        total_auctions: s.total_auctions,
        submitted: s.submitted,
        empty: s.empty,
        errors: s.errors,
        avg_response_ms: s.avg_response_ms,
        fallback_count: s.fallback_count,
    });

    DashboardData {
        timestamp: now,
        uptime_seconds: now.saturating_sub(m.start_time_sec),
        overview: Overview {
            auctions_received: auctions,
            solutions_submitted: submitted,
            solutions_held: held,
            solutions_empty: empty,
            win_rate_pct: submit_rate_pct,
            avg_solve_ms,
            avg_score_gwei,
            last_auction_secs_ago,
            fallback_used: m.fallback_used.load(Ordering::Relaxed),
        },
        performance,
        strategies,
        triage: triage_stats,
        simulation: sim_stats,
        freshness: freshness_stats,
        submission: submission_stats,
        indexer: indexer_stats,
        rpc: rpc_stats,
        recent_auctions,
        pnl,
        replay_summary,
        competitiveness: crate::competition::CompetitivenessStats::default(),
        score_anatomy: Vec::new(), // populated async in api_stats
        diagnosis: String::new(),  // populated async in api_stats
        eth_price_usd_cents: monitoring::eth_price_usd_cents(),
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// GET /api/stats — JSON data for the dashboard frontend.
pub async fn api_stats(Query(params): Query<TokenParam>) -> impl IntoResponse {
    if !check_auth(&params) {
        return (StatusCode::UNAUTHORIZED, "Invalid token").into_response();
    }

    // Fetch competition stats async (uses tokio Mutex, can't be in spawn_blocking)
    let comp_stats = crate::competition::get_stats().await;

    let mut data = tokio::task::spawn_blocking(build_dashboard_data)
        .await
        .unwrap_or_else(|_| build_fallback_data());

    data.competitiveness = comp_stats;

    // Score anatomy + diagnosis (async — uses competition results store)
    data.score_anatomy = crate::competition::score_anatomy(15).await;
    data.diagnosis = crate::competition::auto_diagnosis(100).await;

    Json(data).into_response()
}

fn build_fallback_data() -> DashboardData {
    DashboardData {
        timestamp: now_secs(),
        uptime_seconds: 0,
        overview: Overview {
            auctions_received: 0, solutions_submitted: 0, solutions_held: 0, solutions_empty: 0,
            win_rate_pct: 0.0, avg_solve_ms: 0, avg_score_gwei: 0,
            last_auction_secs_ago: 0, fallback_used: 0,
        },
        performance: PerformanceStats {
            hist_50: 0, hist_100: 0, hist_150: 0, hist_200: 0, hist_250: 0,
            hist_300: 0, hist_350: 0, hist_400: 0, hist_500: 0, hist_750: 0,
            hist_1000: 0, hist_2500: 0, hist_5000: 0, hist_10000: 0, hist_25000: 0, hist_over: 0,
            p50_ms: 0, p95_ms: 0, p99_ms: 0,
            score_min_gwei: 0, score_max_gwei: 0, auctions_per_hour: 0,
        },
        strategies: vec![],
        triage: TriageStats { profitable: 0, marginal: 0, unwinnable: 0, skip: 0 },
        simulation: SimStats { total: 0, passed: 0, reverted: 0, timeout: 0, skipped: 0 },
        freshness: FreshnessStats { total: 0, fresh: 0, acceptable: 0, stale: 0, avg_confidence: 0.0 },
        submission: SubmissionStats { total: 0, submit: 0, hold: 0, replace: 0, risk_low: 0, risk_medium: 0, risk_high: 0 },
        indexer: IndexerStats { current_block: 0, pool_cache_size: 0 },
        rpc: RpcStats { total_calls: 0, errors: 0, error_pct: 0.0, avg_latency_ms: 0 },
        recent_auctions: vec![],
        pnl: None,
        replay_summary: None,
        competitiveness: crate::competition::CompetitivenessStats::default(),
        score_anatomy: Vec::new(),
        diagnosis: String::new(),
        eth_price_usd_cents: 0,
    }
}

/// GET /dashboard — self-contained HTML command center.
pub async fn dashboard_page(Query(params): Query<TokenParam>) -> impl IntoResponse {
    if !check_auth(&params) {
        return (StatusCode::UNAUTHORIZED, Html("Unauthorized. Add ?token=YOUR_TOKEN".to_string())).into_response();
    }

    let token_param = params.token
        .map(|t| format!("?token={}", t))
        .unwrap_or_default();

    let html = format!(r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>CoW Solver - Command Center</title>
<style>
:root {{
  --bg: #0b0e14;
  --surface: #131820;
  --surface2: #1a2030;
  --border: #2a3040;
  --text: #e0e4ec;
  --text2: #8892a4;
  --accent: #6c5ce7;
  --green: #00b894;
  --red: #e74c3c;
  --yellow: #fdcb6e;
  --blue: #74b9ff;
  --orange: #e17055;
}}
* {{ margin: 0; padding: 0; box-sizing: border-box; }}
body {{ background: var(--bg); color: var(--text); font-family: 'Inter', -apple-system, BlinkMacSystemFont, sans-serif; font-size: 14px; }}
.container {{ max-width: 1400px; margin: 0 auto; padding: 16px; }}

/* Header */
.header {{ display: flex; align-items: center; justify-content: space-between; padding: 12px 0; border-bottom: 1px solid var(--border); margin-bottom: 20px; }}
.header h1 {{ font-size: 20px; font-weight: 700; letter-spacing: -0.5px; }}
.header h1 span {{ color: var(--accent); }}
.header-right {{ display: flex; align-items: center; gap: 16px; font-size: 12px; color: var(--text2); }}
.status-dot {{ width: 8px; height: 8px; border-radius: 50%; display: inline-block; margin-right: 4px; }}
.status-dot.live {{ background: var(--green); box-shadow: 0 0 6px var(--green); }}
.status-dot.idle {{ background: var(--yellow); }}
.status-dot.dead {{ background: var(--red); }}

/* Cards */
.grid {{ display: grid; gap: 16px; }}
.grid-4 {{ grid-template-columns: repeat(4, 1fr); }}
.grid-3 {{ grid-template-columns: repeat(3, 1fr); }}
.grid-2 {{ grid-template-columns: repeat(2, 1fr); }}
.grid-1 {{ grid-template-columns: 1fr; }}

.card {{ background: var(--surface); border: 1px solid var(--border); border-radius: 10px; padding: 16px; }}
.card h3 {{ font-size: 11px; text-transform: uppercase; letter-spacing: 1px; color: var(--text2); margin-bottom: 12px; }}

/* Big numbers */
.metric {{ text-align: center; }}
.metric .value {{ font-size: 32px; font-weight: 700; font-variant-numeric: tabular-nums; }}
.metric .label {{ font-size: 11px; color: var(--text2); margin-top: 4px; }}
.metric .value.green {{ color: var(--green); }}
.metric .value.red {{ color: var(--red); }}
.metric .value.yellow {{ color: var(--yellow); }}
.metric .value.blue {{ color: var(--blue); }}
.metric .value.accent {{ color: var(--accent); }}

/* Tables */
table {{ width: 100%; border-collapse: collapse; font-size: 13px; }}
th {{ text-align: left; font-size: 10px; text-transform: uppercase; letter-spacing: 0.8px; color: var(--text2); padding: 8px 10px; border-bottom: 1px solid var(--border); }}
td {{ padding: 8px 10px; border-bottom: 1px solid var(--border); font-variant-numeric: tabular-nums; }}
tr:last-child td {{ border-bottom: none; }}
tr:hover td {{ background: var(--surface2); }}

/* Bars */
.bar-container {{ display: flex; gap: 2px; height: 20px; border-radius: 4px; overflow: hidden; }}
.bar-segment {{ height: 100%; min-width: 2px; transition: width 0.3s; }}
.bar-segment.green {{ background: var(--green); }}
.bar-segment.red {{ background: var(--red); }}
.bar-segment.yellow {{ background: var(--yellow); }}
.bar-segment.blue {{ background: var(--blue); }}
.bar-segment.gray {{ background: var(--border); }}

/* Result badges */
.badge {{ display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; }}
.badge.submitted {{ background: rgba(0, 184, 148, 0.15); color: var(--green); }}
.badge.empty {{ background: rgba(253, 203, 110, 0.15); color: var(--yellow); }}
.badge.held {{ background: rgba(116, 185, 255, 0.15); color: var(--blue); }}
.badge.error {{ background: rgba(231, 76, 60, 0.15); color: var(--red); }}

/* Responsive */
@media (max-width: 900px) {{
  .grid-4 {{ grid-template-columns: repeat(2, 1fr); }}
  .grid-3 {{ grid-template-columns: repeat(2, 1fr); }}
}}
@media (max-width: 600px) {{
  .grid-4, .grid-3, .grid-2 {{ grid-template-columns: 1fr; }}
}}

/* Refresh indicator */
.refresh {{ position: fixed; top: 8px; right: 8px; font-size: 10px; color: var(--text2); opacity: 0; transition: opacity 0.3s; }}
.refresh.active {{ opacity: 1; }}
</style>
</head>
<body>
<div class="container">
  <div class="header">
    <h1><span>CoW</span> Solver - Command Center</h1>
    <div class="header-right">
      <span><span class="status-dot" id="statusDot"></span><span id="statusLabel">Loading...</span></span>
      <span id="uptimeLabel">Uptime: --</span>
      <span id="blockLabel">Block: --</span>
      <span id="ethPriceLabel" style="color:var(--text2)"></span>
      <span id="rpcCostLabel" style="color:var(--text2)"></span>
      <select id="timeFilterHeader" onchange="syncTimeFilter(this.value)" style="background:var(--bg);color:var(--text);border:1px solid var(--text2);border-radius:4px;padding:3px 6px;font-size:12px;cursor:pointer">
        <option value="1">1h</option>
        <option value="6" selected>6h</option>
        <option value="24">24h</option>
        <option value="0">All time</option>
      </select>
      <button id="discoverBtn" onclick="triggerDiscovery()" style="
        background: var(--accent); color: white; border: none; border-radius: 6px;
        padding: 4px 12px; font-size: 12px; cursor: pointer; font-weight: 600;
      ">Discover Pools</button>
      <span id="discoveryStatus" style="color:var(--text2);font-size:11px"></span>
    </div>
  </div>

  <!-- Overview KPIs -->
  <div class="grid" style="grid-template-columns: repeat(5, 1fr); margin-bottom: 16px; gap: 16px;">
    <div class="card metric">
      <div class="value accent" id="kpiAuctions">--</div>
      <div class="label">Auctions Received</div>
    </div>
    <div class="card metric">
      <div class="value green" id="kpiSubmitted">--</div>
      <div class="label">Submitted</div>
    </div>
    <div class="card metric">
      <div class="value" id="kpiHeld" style="color:orange">--</div>
      <div class="label">Held</div>
    </div>
    <div class="card metric">
      <div class="value blue" id="kpiAvgSolve">--</div>
      <div class="label">Avg Solve Time</div>
    </div>
    <div class="card metric">
      <div class="value yellow" id="kpiPerHour">--</div>
      <div class="label">Auctions / Hour</div>
    </div>
  </div>

  <!-- Performance Row -->
  <div class="grid grid-3" style="margin-bottom: 16px;">
    <div class="card">
      <h3>Solve Time Percentiles</h3>
      <div style="display:flex;gap:8px;margin-bottom:12px;">
        <div class="metric" style="flex:1;background:var(--surface2);border-radius:8px;padding:10px;">
          <div class="value green" id="perfP50" style="font-size:22px;">--</div>
          <div class="label">p50</div>
        </div>
        <div class="metric" style="flex:1;background:var(--surface2);border-radius:8px;padding:10px;">
          <div class="value yellow" id="perfP95" style="font-size:22px;">--</div>
          <div class="label">p95</div>
        </div>
        <div class="metric" style="flex:1;background:var(--surface2);border-radius:8px;padding:10px;">
          <div class="value red" id="perfP99" style="font-size:22px;">--</div>
          <div class="label">p99</div>
        </div>
      </div>
      <div style="font-size:11px;color:var(--text2);margin-bottom:4px;">Budget: 25 000ms — color turns red above p95 &gt; 10 000ms</div>
    </div>
    <div class="card">
      <h3>Solve Time Distribution</h3>
      <div id="histChart" style="display:flex;flex-direction:column;gap:4px;"></div>
    </div>
    <div class="card">
      <h3>Score Distribution</h3>
      <table>
        <tr><td>Min Score</td><td style="text-align:right;color:var(--text2)" id="scoreMin">-- gwei</td></tr>
        <tr><td>Avg Score</td><td style="text-align:right;color:var(--accent)" id="scoreAvg">-- gwei</td></tr>
        <tr><td>Max Score</td><td style="text-align:right;color:var(--green)" id="scoreMax">-- gwei</td></tr>
      </table>
      <div style="margin-top:12px;">
        <div style="font-size:11px;color:var(--text2);margin-bottom:4px;">Score range visualizer</div>
        <div id="scoreRangeBar" style="height:8px;background:var(--border);border-radius:4px;position:relative;">
          <div id="scoreRangeFill" style="height:100%;border-radius:4px;background:linear-gradient(to right,var(--accent),var(--green));width:0%;"></div>
        </div>
      </div>
    </div>
  </div>

  <!-- Row 2: Triage + Submission + Freshness -->
  <div class="grid grid-3" style="margin-bottom: 16px;">
    <div class="card">
      <h3>Auction Triage</h3>
      <div class="bar-container" id="triageBar"></div>
      <table style="margin-top: 10px;">
        <tr><td>Profitable</td><td style="text-align:right;color:var(--green)" id="triProfit">0</td></tr>
        <tr><td>Marginal</td><td style="text-align:right;color:var(--yellow)" id="triMarginal">0</td></tr>
        <tr><td>Unwinnable</td><td style="text-align:right;color:var(--orange)" id="triUnwin">0</td></tr>
        <tr><td>Skipped</td><td style="text-align:right;color:var(--text2)" id="triSkip">0</td></tr>
      </table>
    </div>
    <div class="card">
      <h3>Submission Policy</h3>
      <div class="bar-container" id="subBar"></div>
      <table style="margin-top: 10px;">
        <tr><td>Submit</td><td style="text-align:right;color:var(--green)" id="subSubmit">0</td></tr>
        <tr><td>Hold</td><td style="text-align:right;color:var(--yellow)" id="subHold">0</td></tr>
        <tr><td>Replace</td><td style="text-align:right;color:var(--blue)" id="subReplace">0</td></tr>
      </table>
      <table style="margin-top: 6px; border-top: 1px solid var(--border); padding-top: 6px;">
        <tr><td>Low Risk</td><td style="text-align:right;color:var(--green)" id="riskLow">0</td></tr>
        <tr><td>Medium Risk</td><td style="text-align:right;color:var(--yellow)" id="riskMed">0</td></tr>
        <tr><td>High Risk</td><td style="text-align:right;color:var(--red)" id="riskHigh">0</td></tr>
      </table>
    </div>
    <div class="card">
      <h3>Data Freshness</h3>
      <div class="metric" style="margin-bottom: 10px;">
        <div class="value" id="freshConf" style="font-size: 28px;">--</div>
        <div class="label">Avg Confidence</div>
      </div>
      <div class="bar-container" id="freshBar"></div>
      <table style="margin-top: 10px;">
        <tr><td>Fresh (>0.8)</td><td style="text-align:right;color:var(--green)" id="freshGood">0</td></tr>
        <tr><td>Acceptable</td><td style="text-align:right;color:var(--yellow)" id="freshOk">0</td></tr>
        <tr><td>Stale (<0.3)</td><td style="text-align:right;color:var(--red)" id="freshBad">0</td></tr>
      </table>
    </div>
  </div>

  <!-- Row 3: Strategy Funnel + RPC + Sim -->
  <div class="grid grid-2" style="margin-bottom: 16px;">
    <div class="card">
      <h3>Strategy Attribution Funnel</h3>
      <table>
        <thead>
          <tr><th>Strategy</th><th>Generated</th><th>Sim Pass</th><th>Submitted</th><th>Won</th><th>Submit %</th><th>Win %</th></tr>
        </thead>
        <tbody id="strategyTable"></tbody>
      </table>
    </div>
    <div class="card">
      <h3>System Health</h3>
      <table>
        <tr><td>Validation <span style="color:var(--text2);font-size:10px">(structural, no EVM fork)</span></td><td style="text-align:right"><span id="simPassed" style="color:var(--green)">0</span> pass / <span id="simReverted" style="color:var(--red)">0</span> revert / <span id="simSkipped" style="color:var(--text2)">0</span> skip</td></tr>
        <tr><td>RPC Calls</td><td style="text-align:right" id="rpcCalls">0</td></tr>
        <tr><td>RPC Errors</td><td style="text-align:right" id="rpcErrors">0</td></tr>
        <tr><td>RPC Error Rate</td><td style="text-align:right" id="rpcErrPct">0%</td></tr>
        <tr><td>Avg RPC Latency</td><td style="text-align:right" id="rpcLatency">0ms</td></tr>
        <tr><td>Pool Cache</td><td style="text-align:right" id="poolCount">0 pools</td></tr>
        <tr><td>Avg Score</td><td style="text-align:right" id="avgScore">0 gwei</td></tr>
        <tr><td>Max Score</td><td style="text-align:right;color:var(--green)" id="maxScoreHealth">0 gwei</td></tr>
        <tr><td>Fallbacks Used</td><td style="text-align:right" id="fallbackCount">0</td></tr>
      </table>
    </div>
  </div>

  <!-- Shadow Competitiveness Panel -->
  <div class="grid grid-1" id="compSection" style="margin-bottom: 16px; display: none;">
    <div class="card">
      <h3>Shadow Competitiveness</h3>
      <div class="grid grid-4" style="gap: 10px;">
        <div class="metric"><div class="value" id="compCompared" style="font-size:22px">0</div><div class="label">Auctions Compared</div></div>
        <div class="metric"><div class="value green" id="compWins" style="font-size:22px">0</div><div class="label">Wins</div></div>
        <div class="metric"><div class="value" id="compWithin10" style="font-size:22px">0</div><div class="label">Within 10%</div></div>
        <div class="metric"><div class="value" id="compWithin50" style="font-size:22px">0</div><div class="label">Within 50%</div></div>
      </div>
      <div class="grid grid-4" style="gap: 10px; margin-top: 10px;">
        <div class="metric"><div class="value" id="compMedianDelta" style="font-size:18px">--</div><div class="label">Median Gap to Winner</div></div>
        <div class="metric"><div class="value" id="compBestRank" style="font-size:18px">--</div><div class="label">Best Rank</div></div>
        <div class="metric"><div class="value" id="compAvgRank" style="font-size:18px">--</div><div class="label">Avg Rank</div></div>
        <div class="metric"><div class="value" id="compTopWinner" style="font-size:14px">--</div><div class="label">Top Competitor</div></div>
      </div>
      <div id="compClosestMiss" style="text-align:center; padding: 8px; color: var(--text2); font-size: 12px; margin-top: 8px; display: none;"></div>
      <div id="scoreValidation" style="text-align:center; padding: 6px; color: var(--text2); font-size: 12px; margin-top: 4px; display: none;">Scoring: --</div>
      <!-- Theoretical Earnings sub-card -->
      <div style="margin-top: 12px; padding: 12px; background: rgba(139,92,246,0.08); border-radius: 8px; border: 1px solid rgba(139,92,246,0.2);">
        <div style="font-size: 13px; font-weight: 600; color: var(--accent); margin-bottom: 8px;">Theoretical Earnings (if we had won)</div>
        <div class="grid grid-4" style="gap: 10px;">
          <div class="metric"><div class="value green" id="compEarningsEth" style="font-size:20px">0 ETH</div><div class="label">Total</div></div>
          <div class="metric"><div class="value" id="compEarningsUsd" style="font-size:20px">$0</div><div class="label">USD Value</div></div>
          <div class="metric"><div class="value" id="compPerWinAvg" style="font-size:18px">--</div><div class="label">Per-Win Avg</div></div>
          <div class="metric"><div class="value" id="compTotalSurplus" style="font-size:18px">--</div><div class="label">Total Surplus Generated</div></div>
        </div>
      </div>
    </div>
  </div>

  <!-- Row 4: P&L (hidden until data exists) -->
  <div class="grid grid-1" id="pnlSection" style="margin-bottom: 16px; display: none;">
    <div class="card">
      <h3>Settlement P&L</h3>
      <div class="grid grid-4" style="gap: 10px;">
        <div class="metric"><div class="value green" id="pnlSurplus" style="font-size:22px">--</div><div class="label">Predicted Surplus (gwei)</div></div>
        <div class="metric"><div class="value" id="pnlRealized" style="font-size:22px">--</div><div class="label">Realized Surplus (gwei)</div></div>
        <div class="metric"><div class="value red" id="pnlGas" style="font-size:22px">--</div><div class="label">Gas Cost (gwei)</div></div>
        <div class="metric"><div class="value" id="pnlNet" style="font-size:22px">--</div><div class="label">Net P&L (gwei)</div></div>
      </div>
    </div>
  </div>

  <!-- Row 5: Recent Auctions -->
  <div class="grid grid-1" style="margin-bottom: 16px;">
    <div class="card">
      <h3 style="display:inline">Recent Auctions</h3>
      <div style="float:right;font-size:12px;color:var(--text2);display:flex;align-items:center;gap:10px">
        <select id="timeFilterTable" onchange="syncTimeFilter(this.value)" style="background:var(--surface);color:var(--text);border:1px solid var(--text2);border-radius:4px;padding:2px 4px;font-size:11px">
          <option value="1">1h</option>
          <option value="6" selected>6h</option>
          <option value="24">24h</option>
          <option value="0">All</option>
        </select>
        <label style="cursor:pointer">
          <input type="checkbox" id="showAllAuctions" onchange="refresh()"> incl. no-settle
          <span title="~80% of CoW batches don't settle. 'No settle' = nobody won, no data to compare." style="cursor:help;border:1px solid var(--text2);border-radius:50%;width:14px;height:14px;display:inline-block;text-align:center;font-size:10px;line-height:14px;margin-left:2px">i</span>
        </label>
      </div>
      <table>
        <thead>
          <tr><th>Auction ID</th><th>Time</th><th>Strategy</th><th>Result</th><th>Solve</th><th>Our Score</th><th>Winner</th><th>Gap</th><th>Earnings</th></tr>
        </thead>
        <tbody id="recentTable"></tbody>
      </table>
      <div id="noAuctions" style="text-align:center; padding: 20px; color: var(--text2); display: none;">
        No auctions received yet. Waiting for CoW driver to send traffic...
      </div>
    </div>
  </div>
</div>

<!-- Score Anatomy -->
<div class="card" id="scoreAnatomyCard" style="display:none">
  <h3>Score Anatomy <span style="font-size:11px;color:var(--text2)">(our score vs winner, per auction)</span></h3>
  <div id="diagnosisBox" style="padding:6px 10px;background:var(--surface);border-radius:4px;font-size:12px;color:var(--text2);margin-bottom:8px"></div>
  <table>
    <thead><tr>
      <th>Auction</th><th>Our Score</th><th>Winner</th><th>Ratio</th><th>Class</th>
    </tr></thead>
    <tbody id="anatomyBody"></tbody>
  </table>
</div>

<div class="refresh" id="refreshIndicator">Refreshing...</div>

<script>
const API_URL = '/api/stats{token_param}';
const TOKEN = new URLSearchParams(window.location.search).get('token') || '';
const REFRESH_MS = 15000;

function fmt(n) {{ return n.toLocaleString(); }}
// Convert gwei amount to USD string using cached ETH price.
// ethPriceCents is from d.eth_price_usd_cents (e.g. 270000 = $2700)
function gweiToUsd(gwei, ethPriceCents) {{
  if (!ethPriceCents || ethPriceCents === 0 || !gwei) return '';
  const ethAmount = gwei / 1e9; // gwei to ETH
  const usd = ethAmount * ethPriceCents / 100; // cents to dollars
  if (usd < 0.01) return ' ($<0.01)';
  if (usd < 1) return ' ($' + usd.toFixed(3) + ')';
  return ' ($' + usd.toFixed(2) + ')';
}}
function pct(n) {{ return n.toFixed(1) + '%'; }}
function ms(n) {{ return n + 'ms'; }}
function timeAgo(secs) {{
  if (secs < 60) return secs + 's ago';
  if (secs < 3600) return Math.floor(secs / 60) + 'm ago';
  if (secs < 86400) return Math.floor(secs / 3600) + 'h ago';
  return Math.floor(secs / 86400) + 'd ago';
}}
function uptime(secs) {{
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return h + 'h ' + m + 'm';
}}

function makeBar(segments, total) {{
  if (total === 0) return '<div class="bar-segment gray" style="width:100%"></div>';
  return segments.map(s =>
    `<div class="bar-segment ${{s.cls}}" style="width:${{(s.val / total * 100).toFixed(1)}}%" title="${{s.label}}: ${{s.val}}"></div>`
  ).join('');
}}

function resultBadge(r) {{
  const cls = r === 'submitted' ? 'submitted' : r === 'held' ? 'held' : r === 'empty' ? 'empty' : 'error';
  return `<span class="badge ${{cls}}">${{r}}</span>`;
}}

function winRateColor(rate) {{
  if (rate >= 20) return 'green';
  if (rate >= 5) return 'yellow';
  return 'red';
}}

function freshColor(conf) {{
  if (conf >= 0.8) return 'green';
  if (conf >= 0.3) return 'yellow';
  return 'red';
}}

function syncTimeFilter(val) {{
  document.getElementById('timeFilterHeader').value = val;
  document.getElementById('timeFilterTable').value = val;
  refresh();
}}
function getTimeFilterHours() {{
  return parseInt(document.getElementById('timeFilterHeader')?.value ?? '6');
}}

async function refresh() {{
  const indicator = document.getElementById('refreshIndicator');
  indicator.classList.add('active');

  try {{
    const res = await fetch(API_URL);
    if (!res.ok) throw new Error(res.status);
    const d = await res.json();

    // Status
    const dot = document.getElementById('statusDot');
    const label = document.getElementById('statusLabel');
    if (d.overview.last_auction_secs_ago > 0 && d.overview.last_auction_secs_ago < 300) {{
      dot.className = 'status-dot live'; label.textContent = 'Live — ' + timeAgo(d.overview.last_auction_secs_ago);
    }} else if (d.overview.auctions_received > 0) {{
      dot.className = 'status-dot idle'; label.textContent = 'Idle — last ' + timeAgo(d.overview.last_auction_secs_ago);
    }} else {{
      dot.className = 'status-dot idle'; label.textContent = 'Waiting for auctions';
    }}
    document.getElementById('uptimeLabel').textContent = 'Uptime: ' + uptime(d.uptime_seconds);
    document.getElementById('blockLabel').textContent = 'Block: ' + (d.indexer.current_block > 0 ? fmt(d.indexer.current_block) : '--');
    document.getElementById('ethPriceLabel').textContent = d.eth_price_usd_cents > 0 ? 'ETH: $' + (d.eth_price_usd_cents / 100).toFixed(0) : '';
    // Compute estimated Alchemy cost early (also used in System Health section below)
    const cuPerCallEarly = 26;
    const totalCUEarly = d.rpc.total_calls * cuPerCallEarly;
    const baseCUEarly = Math.min(totalCUEarly, 300_000_000);
    const volumeCUEarly = Math.max(0, totalCUEarly - 300_000_000);
    var estimatedRpcCost = (baseCUEarly / 1_000_000 * 0.45) + (volumeCUEarly / 1_000_000 * 0.40);
    // Show estimated Alchemy spend in header
    document.getElementById('rpcCostLabel').textContent = 'RPC: ~$' + estimatedRpcCost.toFixed(2);
    document.getElementById('rpcCostLabel').style.color = estimatedRpcCost > 5 ? 'var(--red)' : estimatedRpcCost > 1 ? 'orange' : 'var(--text2)';

    // ── Sanity checks ─────────────────────────────────────────────────────
    const warnings = [];
    if (d.performance.score_min_gwei > 0 && d.performance.score_max_gwei > 0) {{
      if (d.overview.avg_score_gwei < d.performance.score_min_gwei) {{
        warnings.push('avg score (' + fmt(d.overview.avg_score_gwei) + ') < min score (' + fmt(d.performance.score_min_gwei) + ') — data corruption');
      }}
      if (d.performance.score_min_gwei > d.performance.score_max_gwei) {{
        warnings.push('min score > max score — data corruption');
      }}
    }}
    // Dead strategies warning
    const deadStrategies = d.strategies.filter(s => s.generated > 50 && s.submitted === 0);
    if (deadStrategies.length > 0) {{
      warnings.push(deadStrategies.length + ' dead strategies: ' + deadStrategies.map(s => s.name).join(', '));
    }}

    let warnBanner = document.getElementById('warningBanner');
    if (!warnBanner) {{
      warnBanner = document.createElement('div');
      warnBanner.id = 'warningBanner';
      warnBanner.style.cssText = 'background:rgba(255,100,50,0.15);border:1px solid var(--red);border-radius:8px;padding:10px 16px;margin-bottom:12px;color:var(--red);font-size:13px;display:none;';
      document.querySelector('.container').insertBefore(warnBanner, document.querySelector('.container').children[1]);
    }}
    if (warnings.length > 0) {{
      warnBanner.innerHTML = '<strong>\u26a0 Sanity Warnings:</strong> ' + warnings.join(' | ');
      warnBanner.style.display = 'block';
    }} else {{
      warnBanner.style.display = 'none';
    }}

    // KPIs — compute from time-filtered recent_auctions for accurate window view
    const timeHoursKpi = getTimeFilterHours();
    const nowSecKpi = Math.floor(Date.now() / 1000);
    const cutoffKpi = timeHoursKpi > 0 ? nowSecKpi - (timeHoursKpi * 3600) : 0;
    const windowAuctions = d.recent_auctions.filter(a => timeHoursKpi === 0 || a.received_at >= cutoffKpi);

    const wSubmitted = windowAuctions.filter(a => a.result === 'submitted').length;
    const wHeld = windowAuctions.filter(a => a.result === 'held').length;
    const wTotal = windowAuctions.length;
    const wAvgSolve = wTotal > 0 ? Math.round(windowAuctions.reduce((s,a) => s + a.response_time_ms, 0) / wTotal) : 0;
    const wSpanSec = wTotal > 1 ? (windowAuctions[0].received_at - windowAuctions[windowAuctions.length-1].received_at) : 1;
    const wPerHour = wSpanSec > 0 ? Math.round(wTotal / wSpanSec * 3600) : 0;

    document.getElementById('kpiAuctions').textContent = fmt(wTotal);
    document.getElementById('kpiSubmitted').textContent = fmt(wSubmitted);
    document.getElementById('kpiHeld').textContent = fmt(wHeld);
    document.getElementById('kpiAvgSolve').textContent = ms(wAvgSolve);
    document.getElementById('kpiPerHour').textContent = fmt(wPerHour);

    // Performance — percentiles
    document.getElementById('perfP50').textContent = ms(d.performance.p50_ms);
    const p95el = document.getElementById('perfP95');
    p95el.textContent = ms(d.performance.p95_ms);
    p95el.className = 'value ' + (d.performance.p95_ms > 10000 ? 'red' : d.performance.p95_ms > 5000 ? 'yellow' : 'green');
    document.getElementById('perfP99').textContent = ms(d.performance.p99_ms);

    // Performance — histogram (fine-grained around 200-500ms typical solve range)
    const histBuckets = [
      {{label:'≤50ms',  val: d.performance.hist_50}},
      {{label:'≤100ms', val: d.performance.hist_100}},
      {{label:'≤150ms', val: d.performance.hist_150}},
      {{label:'≤200ms', val: d.performance.hist_200}},
      {{label:'≤250ms', val: d.performance.hist_250}},
      {{label:'≤300ms', val: d.performance.hist_300}},
      {{label:'≤350ms', val: d.performance.hist_350}},
      {{label:'≤400ms', val: d.performance.hist_400}},
      {{label:'≤500ms', val: d.performance.hist_500}},
      {{label:'≤750ms', val: d.performance.hist_750}},
      {{label:'≤1s',    val: d.performance.hist_1000}},
      {{label:'≤2.5s',  val: d.performance.hist_2500}},
      {{label:'≤5s',    val: d.performance.hist_5000}},
      {{label:'≤10s',   val: d.performance.hist_10000}},
      {{label:'≤25s',   val: d.performance.hist_25000}},
      {{label:'>25s',   val: d.performance.hist_over}},
    ];
    const histTotal = histBuckets.reduce((s, b) => s + b.val, 0);
    const histColors = ['green','green','green','green','green','green','green','green','yellow','yellow','yellow','orange','orange','red','red','red'];
    const histChart = document.getElementById('histChart');
    if (histTotal === 0) {{
      histChart.innerHTML = '<div style="color:var(--text2);font-size:12px;padding:8px 0;">No data yet</div>';
    }} else {{
      histChart.innerHTML = histBuckets.map((b, i) => {{
        const w = histTotal > 0 ? (b.val / histTotal * 100).toFixed(1) : 0;
        return `<div style="display:flex;align-items:center;gap:6px;">
          <div style="width:42px;font-size:10px;color:var(--text2);text-align:right">${{b.label}}</div>
          <div style="flex:1;background:var(--border);border-radius:2px;height:12px;">
            <div style="width:${{w}}%;height:100%;border-radius:2px;background:var(--${{histColors[i]}});transition:width 0.3s;"></div>
          </div>
          <div style="width:32px;font-size:10px;color:var(--text2);text-align:right">${{b.val}}</div>
        </div>`;
      }}).join('');
    }}

    // Performance — score distribution
    const ep = d.eth_price_usd_cents;
    document.getElementById('scoreMin').textContent = fmt(d.performance.score_min_gwei) + ' gwei' + gweiToUsd(d.performance.score_min_gwei, ep);
    document.getElementById('scoreAvg').textContent = fmt(d.overview.avg_score_gwei) + ' gwei' + gweiToUsd(d.overview.avg_score_gwei, ep);
    document.getElementById('scoreMax').textContent = fmt(d.performance.score_max_gwei) + ' gwei' + gweiToUsd(d.performance.score_max_gwei, ep);
    document.getElementById('maxScoreHealth').textContent = fmt(d.performance.score_max_gwei) + ' gwei' + gweiToUsd(d.performance.score_max_gwei, ep);
    // Score range bar: fill = avg position between min and max
    if (d.performance.score_max_gwei > 0 && d.performance.score_max_gwei > d.performance.score_min_gwei) {{
      const fillPct = ((d.overview.avg_score_gwei - d.performance.score_min_gwei) /
        (d.performance.score_max_gwei - d.performance.score_min_gwei) * 100).toFixed(1);
      document.getElementById('scoreRangeFill').style.width = fillPct + '%';
    }}

    // Triage
    const triTotal = d.triage.profitable + d.triage.marginal + d.triage.unwinnable + d.triage.skip;
    document.getElementById('triageBar').innerHTML = makeBar([
      {{val: d.triage.profitable, cls: 'green', label: 'Profitable'}},
      {{val: d.triage.marginal, cls: 'yellow', label: 'Marginal'}},
      {{val: d.triage.unwinnable, cls: 'blue', label: 'Unwinnable'}},
      {{val: d.triage.skip, cls: 'gray', label: 'Skip'}},
    ], triTotal);
    document.getElementById('triProfit').textContent = fmt(d.triage.profitable);
    document.getElementById('triMarginal').textContent = fmt(d.triage.marginal);
    document.getElementById('triUnwin').textContent = fmt(d.triage.unwinnable);
    document.getElementById('triSkip').textContent = fmt(d.triage.skip);

    // Submission
    const subTotal = d.submission.submit + d.submission.hold + d.submission.replace;
    document.getElementById('subBar').innerHTML = makeBar([
      {{val: d.submission.submit, cls: 'green', label: 'Submit'}},
      {{val: d.submission.hold, cls: 'yellow', label: 'Hold'}},
      {{val: d.submission.replace, cls: 'blue', label: 'Replace'}},
    ], subTotal);
    document.getElementById('subSubmit').textContent = fmt(d.submission.submit);
    document.getElementById('subHold').textContent = fmt(d.submission.hold);
    document.getElementById('subReplace').textContent = fmt(d.submission.replace);
    document.getElementById('riskLow').textContent = fmt(d.submission.risk_low);
    document.getElementById('riskMed').textContent = fmt(d.submission.risk_medium);
    document.getElementById('riskHigh').textContent = fmt(d.submission.risk_high);

    // Freshness
    const fc = document.getElementById('freshConf');
    fc.textContent = d.freshness.avg_confidence.toFixed(3);
    fc.className = 'value ' + freshColor(d.freshness.avg_confidence);
    const freshTotal = d.freshness.fresh + d.freshness.acceptable + d.freshness.stale;
    document.getElementById('freshBar').innerHTML = makeBar([
      {{val: d.freshness.fresh, cls: 'green', label: 'Fresh'}},
      {{val: d.freshness.acceptable, cls: 'yellow', label: 'Acceptable'}},
      {{val: d.freshness.stale, cls: 'red', label: 'Stale'}},
    ], freshTotal);
    document.getElementById('freshGood').textContent = fmt(d.freshness.fresh);
    document.getElementById('freshOk').textContent = fmt(d.freshness.acceptable);
    document.getElementById('freshBad').textContent = fmt(d.freshness.stale);

    // Strategy table
    const tbody = document.getElementById('strategyTable');
    tbody.innerHTML = d.strategies
      .filter(s => s.generated > 0 || s.submitted > 0)
      .sort((a, b) => b.submitted - a.submitted)
      .map(s => {{
        // Outscored detector: strategy generates but never wins the internal competition
        const isDead = s.generated > 100 && s.submitted === 0;
        const deadBadge = isDead ? ' <span style="background:var(--text2);color:#fff;padding:1px 6px;border-radius:4px;font-size:10px">outscored</span>' : '';
        const rowStyle = isDead ? 'opacity: 0.6;' : '';
        return `<tr style="${{rowStyle}}">
          <td style="font-weight:600">${{s.name}}${{deadBadge}}</td>
          <td>${{fmt(s.generated)}}</td>
          <td style="color:var(--green)">${{fmt(s.sim_passed)}}</td>
          <td style="color:var(--accent)">${{fmt(s.submitted)}}</td>
          <td style="color:var(--blue)">${{fmt(s.won)}}</td>
          <td>${{pct(s.submit_rate * 100)}}</td>
          <td>${{pct(s.win_rate * 100)}}</td>
        </tr>`;
      }}).join('');
    if (tbody.innerHTML === '') {{
      tbody.innerHTML = '<tr><td colspan="7" style="text-align:center;color:var(--text2);padding:16px;">No strategy data yet</td></tr>';
    }}

    // System health
    document.getElementById('simPassed').textContent = fmt(d.simulation.passed);
    document.getElementById('simReverted').textContent = fmt(d.simulation.reverted);
    document.getElementById('simSkipped').textContent = fmt(d.simulation.skipped);
    // RPC calls + estimated Alchemy cost (used in both RPC row and header)
    // Alchemy pay-as-you-go: ~26 CU per eth_call, $0.45 per 1M CU (base), $0.40 over 300M
    // Re-use estimatedRpcCost computed earlier in the header section
    const costStr = ' (~$' + estimatedRpcCost.toFixed(2) + ' est.)';
    document.getElementById('rpcCalls').textContent = fmt(d.rpc.total_calls) + costStr;
    document.getElementById('rpcErrors').textContent = fmt(d.rpc.errors);
    document.getElementById('rpcErrPct').textContent = pct(d.rpc.error_pct);
    const rpcLat = document.getElementById('rpcLatency');
    rpcLat.textContent = ms(d.rpc.avg_latency_ms);
    rpcLat.style.color = d.rpc.avg_latency_ms > 200 ? 'var(--red)' : d.rpc.avg_latency_ms > 50 ? 'var(--yellow)' : 'var(--green)';
    document.getElementById('poolCount').textContent = fmt(d.indexer.pool_cache_size) + ' pools';
    document.getElementById('avgScore').textContent = fmt(d.overview.avg_score_gwei) + ' gwei' + gweiToUsd(d.overview.avg_score_gwei, d.eth_price_usd_cents);
    document.getElementById('fallbackCount').textContent = fmt(d.overview.fallback_used);

    // Recent auctions
    const rt = document.getElementById('recentTable');
    const na = document.getElementById('noAuctions');
    if (d.recent_auctions.length === 0) {{
      rt.innerHTML = '';
      na.style.display = 'block';
    }} else {{
      na.style.display = 'none';
      // Time filter: only show auctions from the last N hours
      const timeHours = getTimeFilterHours();
      const nowSec = Math.floor(Date.now() / 1000);
      const cutoff = timeHours > 0 ? nowSec - (timeHours * 3600) : 0;
      const timeFiltered = d.recent_auctions.filter(a => timeHours === 0 || a.received_at >= cutoff);

      // Settle filter: hide no-settle and pending unless toggled
      const showAll = document.getElementById('showAllAuctions')?.checked ?? false;
      const filteredAuctions = showAll ? timeFiltered : timeFiltered.filter(a => {{
        const noSettle = a.winner_score_gwei !== null && a.winner_score_gwei === 0 && a.winner_solver === 'none';
        const pending = a.winner_score_gwei === null;
        return !noSettle && !pending;
      }});
      rt.innerHTML = filteredAuctions.map(a => {{
        const t = new Date(a.received_at * 1000);
        const timeStr = t.toLocaleTimeString();
        const scoreStr = a.our_score_gwei > 0 ? a.our_score_gwei.toFixed(0) : '-';
        // winner_score_gwei: null = not checked yet, 0 = checked but no settlement, >0 = real winner
        const noSettlement = a.winner_score_gwei !== null && a.winner_score_gwei === 0 && a.winner_solver === 'none';
        const winnerStr = noSettlement
          ? '<span style="color:var(--text2);font-size:10px">no settle</span>'
          : a.winner_score_gwei ? a.winner_score_gwei.toFixed(0) : '<span style="color:var(--text2);font-size:10px">-</span>';
        // Potential earnings: what we'd earn if we won this auction
        // Solver payment ≈ surplus captured (our score in ETH)
        const ethPrice = d.eth_price_usd_cents > 0 ? d.eth_price_usd_cents / 100 : 1850;
        const earnEth = a.winner_score_gwei !== null ? Math.min(a.our_score_gwei, a.winner_score_gwei) / 1e9 : 0;
        const earnUsd = earnEth * ethPrice;
        const earnStr = earnEth > 0
          ? `<span style="color:var(--green);font-size:11px">$${{earnUsd.toFixed(2)}}</span>`
          : '<span style="color:var(--text2);font-size:10px">-</span>';
        const gapStr = noSettlement
          ? '<span style="color:var(--text2);font-size:10px">-</span>'
          : (a.delta_pct !== null && a.delta_pct !== undefined
            ? (a.delta_pct > 200
              ? `<span style="color:orange" title="Score likely inflated">⚠ +${{a.delta_pct.toFixed(0)}}%</span>`
              : `<span style="color:${{a.delta_pct >= 0 ? 'var(--green)' : 'var(--red)'}}">${{a.delta_pct >= 0 ? '+' : ''}}${{a.delta_pct.toFixed(1)}}%</span>`)
            : '<span style="color:var(--text2);font-size:10px">pending</span>');
        const stratStr = a.strategy ? `<span style="color:var(--accent);font-size:11px">${{a.strategy}}</span>` : '-';
        return `<tr>
          <td style="font-family:monospace;font-size:12px">${{a.id}}</td>
          <td style="font-size:12px">${{timeStr}}</td>
          <td>${{stratStr}}</td>
          <td>${{resultBadge(a.result)}}</td>
          <td style="font-size:12px">${{ms(a.response_time_ms)}}</td>
          <td style="text-align:right;font-size:12px">${{scoreStr}}</td>
          <td style="text-align:right;font-size:12px">${{winnerStr}}</td>
          <td style="text-align:right;font-size:12px">${{gapStr}}</td>
          <td style="text-align:right;font-size:12px">${{earnStr}}</td>
        </tr>`;
      }}).join('');

      // Earnings summary below the table
      const ethPrice = d.eth_price_usd_cents > 0 ? d.eth_price_usd_cents / 100 : 1850;
      // Filter to real settled auctions (exclude "no settle" and pending)
      const settled = d.recent_auctions.filter(a =>
        a.winner_score_gwei !== null && a.winner_score_gwei > 0 && a.winner_solver !== 'none'
      );
      const noSettle = d.recent_auctions.filter(a =>
        a.winner_solver === 'none'
      );
      if (settled.length > 0) {{
        // Genuine wins: our score >= winner AND gap < 200% (not inflated)
        const genuineWins = settled.filter(a => a.delta_pct !== null && a.delta_pct >= 0 && a.delta_pct <= 100);
        const genuineLosses = settled.filter(a => a.delta_pct !== null && a.delta_pct < 0);
        const inflatedWins = settled.filter(a => a.delta_pct !== null && a.delta_pct > 100);

        // Earnings from genuine wins only (where we actually found competitive routes)
        const earnedEth = genuineWins.reduce((sum, a) => {{
          return sum + Math.min(a.our_score_gwei, a.winner_score_gwei) / 1e9;
        }}, 0);
        const earnedUsd = earnedEth * ethPrice;

        // Win rate and projection
        const winRate = genuineWins.length / settled.length;
        const uptimeSec = d.uptime_seconds || 1;
        const settledPerHour = settled.length / (uptimeSec / 3600);
        const dailySettled = settledPerHour * 24;
        const dailyWins = dailySettled * winRate;
        const avgEarnPerWin = genuineWins.length > 0 ? earnedEth / genuineWins.length : 0;
        const dailyEth = dailyWins * avgEarnPerWin;
        const dailyUsd = dailyEth * ethPrice;
        const monthlyUsd = dailyUsd * 30;

        let summaryHtml = `<div style="padding:10px 12px;background:var(--surface);border-radius:6px;margin-top:8px;font-size:12px;color:var(--text2);line-height:1.6">`;
        summaryHtml += `<strong>${{settled.length}} settled</strong>`;
        if (noSettle.length > 0) summaryHtml += ` · ${{noSettle.length}} no settlement`;
        summaryHtml += ` · <span style="color:var(--green)">${{genuineWins.length}} genuine wins</span>`;
        summaryHtml += ` · <span style="color:var(--red)">${{genuineLosses.length}} losses</span>`;
        if (inflatedWins.length > 0) summaryHtml += ` · <span style="color:orange">${{inflatedWins.length}} inflated</span>`;
        summaryHtml += `<br><strong>Win rate:</strong> ${{(winRate * 100).toFixed(0)}}%`;
        summaryHtml += ` | <strong>Earned (if live):</strong> ${{earnedEth.toFixed(6)}} ETH ($${{earnedUsd.toFixed(2)}})`;
        if (genuineWins.length > 0 && dailyUsd > 0) {{
          summaryHtml += `<br><strong>Projected:</strong> ~${{dailyWins.toFixed(0)}} wins/day`;
          summaryHtml += ` · $${{dailyUsd.toFixed(2)}}/day · $${{monthlyUsd.toFixed(0)}}/month`;
        }}
        summaryHtml += `</div>`;

        let earningsDiv = document.getElementById('earningsSummary');
        if (!earningsDiv) {{
          earningsDiv = document.createElement('div');
          earningsDiv.id = 'earningsSummary';
          rt.parentElement.appendChild(earningsDiv);
        }}
        earningsDiv.innerHTML = summaryHtml;
      }}
    }}

    // P&L — hide unrealistic values from shadow/testnet (score inflation)
    if (d.pnl && d.pnl.total_auctions > 0) {{
      document.getElementById('pnlSection').style.display = 'block';
      // Cap at 1B gwei (~1 ETH) — anything higher is score inflation, show as "calibrating"
      const MAX_REALISTIC_GWEI = 1_000_000_000;
      const surplus = d.pnl.predicted_surplus_gwei;
      if (surplus > MAX_REALISTIC_GWEI) {{
        document.getElementById('pnlSurplus').textContent = 'Calibrating...';
        document.getElementById('pnlSurplus').style.color = 'var(--text2)';
        document.getElementById('pnlSurplus').style.fontSize = '16px';
      }} else {{
        document.getElementById('pnlSurplus').textContent = surplus.toFixed(2) + gweiToUsd(surplus, d.eth_price_usd_cents);
      }}
      const realized = d.pnl.realized_surplus_gwei;
      document.getElementById('pnlRealized').textContent = realized > 0 ? realized.toFixed(2) + gweiToUsd(realized, d.eth_price_usd_cents) : 'Awaiting settlement data';
      document.getElementById('pnlRealized').style.fontSize = realized > 0 ? '22px' : '14px';
      document.getElementById('pnlRealized').style.color = realized > 0 ? 'var(--green)' : 'var(--text2)';
      document.getElementById('pnlGas').textContent = d.pnl.gas_cost_gwei > 0 ? d.pnl.gas_cost_gwei.toFixed(2) : '--';
      const netEl = document.getElementById('pnlNet');
      if (d.pnl.net_pnl_gwei === 0 && realized === 0) {{
        netEl.textContent = 'Awaiting settlement data';
        netEl.style.fontSize = '14px';
        netEl.className = 'value';
        netEl.style.color = 'var(--text2)';
      }} else {{
        netEl.textContent = d.pnl.net_pnl_gwei.toFixed(2) + gweiToUsd(d.pnl.net_pnl_gwei, d.eth_price_usd_cents);
        netEl.className = 'value ' + (d.pnl.net_pnl_gwei >= 0 ? 'green' : 'red');
      }}
    }}

    // Shadow Competitiveness
    const c = d.competitiveness;
    if (c && c.total_compared > 0) {{
      document.getElementById('compSection').style.display = 'block';
      document.getElementById('compCompared').textContent = fmt(c.total_compared);
      const winsEl = document.getElementById('compWins');
      winsEl.textContent = fmt(c.wins) + ' (' + pct(c.wins / c.total_compared * 100) + ')';
      winsEl.className = 'value ' + (c.wins > 0 ? 'green' : 'red');
      document.getElementById('compWithin10').textContent = fmt(c.within_10pct) + ' (' + pct(c.within_10pct / c.total_compared * 100) + ')';
      document.getElementById('compWithin50').textContent = fmt(c.within_50pct) + ' (' + pct(c.within_50pct / c.total_compared * 100) + ')';
      const deltaEl = document.getElementById('compMedianDelta');
      deltaEl.textContent = c.median_delta_pct.toFixed(1) + '%';
      deltaEl.className = 'value ' + (c.median_delta_pct >= 0 ? 'green' : 'red');
      document.getElementById('compBestRank').textContent = c.best_rank > 0 ? '#' + c.best_rank : '--';
      document.getElementById('compAvgRank').textContent = c.avg_rank > 0 ? '#' + c.avg_rank.toFixed(1) : '--';
      document.getElementById('compTopWinner').textContent = c.top_winner ? c.top_winner.substring(0, 20) + ' (' + fmt(c.top_winner_count) + 'x)' : '--';
      // Theoretical Earnings
      const ethVal = c.theoretical_earnings_eth;
      document.getElementById('compEarningsEth').textContent = ethVal > 0.001 ? ethVal.toFixed(4) + ' ETH' : ethVal.toFixed(6) + ' ETH';
      const ethP = d.eth_price_usd_cents;
      if (ethP > 0) {{
        const usdVal = ethVal * ethP / 100;
        document.getElementById('compEarningsUsd').textContent = usdVal < 1 ? '$' + usdVal.toFixed(4) : '$' + usdVal.toFixed(2);
      }} else {{
        document.getElementById('compEarningsUsd').textContent = '--';
      }}
      if (c.per_win_avg_gwei > 0) {{
        const avgEth = c.per_win_avg_gwei / 1e9;
        let avgStr = avgEth > 0.001 ? avgEth.toFixed(4) + ' ETH' : c.per_win_avg_gwei.toFixed(1) + ' gwei';
        if (ethP > 0) avgStr += ' ($' + (avgEth * ethP / 100).toFixed(3) + ')';
        document.getElementById('compPerWinAvg').textContent = avgStr;
      }}
      const surplusEth = c.total_surplus_gwei / 1e9;
      let surplusStr = surplusEth > 0.001 ? surplusEth.toFixed(4) + ' ETH' : fmt(c.total_surplus_gwei) + ' gwei';
      if (ethP > 0 && surplusEth > 0) surplusStr += ' ($' + (surplusEth * ethP / 100).toFixed(2) + ')';
      document.getElementById('compTotalSurplus').textContent = surplusStr;

      // Closest miss with earnings info
      if (c.closest_miss_pct !== 0 && c.wins === 0) {{
        const missEl = document.getElementById('compClosestMiss');
        const missEth = c.closest_miss_surplus_gwei / 1e9;
        let missStr = 'Closest miss: ' + c.closest_miss_pct.toFixed(1) + '% on auction #' + c.closest_miss_auction;
        if (missEth > 0) {{
          missStr += ' (surplus: ' + missEth.toFixed(6) + ' ETH';
          if (ethP > 0) missStr += ' / $' + (missEth * ethP / 100).toFixed(4);
          missStr += ')';
        }}
        missEl.textContent = missStr;
        missEl.style.display = 'block';
      }}

      // Scoring formula validation
      const valEl = document.getElementById('scoreValidation');
      if (valEl && c.total_compared > 0) {{
        const acc = c.score_accuracy_pct.toFixed(1);
        const inf = c.score_inflated_pct.toFixed(1);
        const color = c.score_inflated_pct > 20 ? '#e74c3c' : c.score_accuracy_pct > 50 ? '#2ecc71' : '#f39c12';
        valEl.innerHTML = '<span style=\"color:' + color + '\">' + acc + '% accurate</span> · ' + inf + '% inflated';
        valEl.style.display = 'block';
      }}
    }}

    // ── Score Anatomy ───────────────────────────────────────────────────
    if (d.score_anatomy && d.score_anatomy.length > 0) {{
      document.getElementById('scoreAnatomyCard').style.display = '';
      document.getElementById('diagnosisBox').textContent = d.diagnosis || '';

      const tbody = document.getElementById('anatomyBody');
      tbody.innerHTML = '';
      for (const a of d.score_anatomy) {{
        const classColor = a.classification === 'accurate' ? 'var(--green)'
          : a.classification === 'inflated' ? '#e74c3c'
          : a.classification === 'high' ? '#f39c12'
          : 'var(--text2)';
        const row = document.createElement('tr');
        row.innerHTML = `<td>${{a.auction_id}}</td>`
          + `<td style="text-align:right">${{a.our_score_gwei.toFixed(0)}}</td>`
          + `<td style="text-align:right">${{a.winner_score_gwei.toFixed(0)}}</td>`
          + `<td style="text-align:right">${{a.ratio.toFixed(1)}}x</td>`
          + `<td style="color:${{classColor}}">${{a.classification}}</td>`;
        tbody.appendChild(row);
      }}
    }}

  }} catch (err) {{
    console.error('Dashboard refresh failed:', err);
    document.getElementById('statusDot').className = 'status-dot dead';
    document.getElementById('statusLabel').textContent = 'Error loading data';
  }}

  indicator.classList.remove('active');
}}

// ── Pool Discovery trigger ──────────────────────────────────────────
async function triggerDiscovery() {{
  const btn = document.getElementById('discoverBtn');
  const status = document.getElementById('discoveryStatus');
  btn.disabled = true;
  btn.textContent = 'Discovering...';
  btn.style.opacity = '0.6';
  status.textContent = 'Running...';
  try {{
    const resp = await fetch('/api/discover?token=' + TOKEN, {{ method: 'POST' }});
    const data = await resp.json();
    if (resp.ok) {{
      status.textContent = 'Started — will complete in ~30s';
      // Poll status every 5s
      const poll = setInterval(async () => {{
        const sr = await fetch('/api/discovery-status?token=' + TOKEN);
        const sd = await sr.json();
        if (!sd.running) {{
          clearInterval(poll);
          btn.disabled = false;
          btn.textContent = 'Discover Pools';
          btn.style.opacity = '1';
          status.textContent = sd.discovered_pools + ' pools discovered (indexed: ' + sd.indexed_pools + ')';
        }}
      }}, 5000);
    }} else {{
      status.textContent = 'Error: ' + (data.message || data.error);
      btn.disabled = false;
      btn.textContent = 'Discover Pools';
      btn.style.opacity = '1';
    }}
  }} catch (e) {{
    status.textContent = 'Request failed';
    btn.disabled = false;
    btn.textContent = 'Discover Pools';
    btn.style.opacity = '1';
  }}
}}

// Also show last discovery info on load
async function loadDiscoveryStatus() {{
  try {{
    const resp = await fetch('/api/discovery-status?token=' + TOKEN);
    if (resp.ok) {{
      const data = await resp.json();
      const el = document.getElementById('discoveryStatus');
      if (data.last_run && data.last_run !== 'never') {{
        el.textContent = data.discovered_pools + ' pools';
      }}
    }}
  }} catch(e) {{}}
}}
loadDiscoveryStatus();

// Initial load + auto-refresh
refresh();
setInterval(refresh, REFRESH_MS);
</script>
</body>
</html>
"##);

    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    (StatusCode::OK, headers, Html(html)).into_response()
}
