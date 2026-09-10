# Task: Shadow Competitiveness Panel

**Priority:** Highest remaining item from Truth & Calibration sprint
**Goal:** Answer the only question that matters: "Would we have won?"

---

## What we need

A background task that, after each auction we participate in, fetches the actual winner's score from the CoW API and stores the comparison. This gives us:

- Did we win? Yes/No
- How far were we from the winner? (delta in wei, delta in %)
- What solver won?
- Our hypothetical rank
- Trend over time

---

## Existing infrastructure (already built, just empty)

| What | Where | Status |
|------|-------|--------|
| `replay.db` auction_log table | `replay.rs` | Has `winning_solver`, `winning_score_wei`, `score_delta_wei` columns - all NULL |
| `accounting.db` settlement_log | `accounting.rs` | Has `realized_surplus_wei`, `realized_gas_cost_wei` - all NULL |
| `RevenueTracker` daily ledger | `monitoring/revenue.rs` | Complete implementation, `record_win()` exists but never called |
| Dashboard `PnlStats` card | `routes/dashboard.rs` | Renders the struct, but realized fields are always 0 |

The database schemas and dashboard rendering are ready. Only the data feeder is missing.

---

## Implementation plan

### 1. CoW API client (new file: `solver-engine/src/competition.rs`)

Query endpoint after each auction:
```
GET https://api.cow.fi/arbitrum/api/v1/solver_competition/by_tx_hash/{tx_hash}
```

Alternative (by auction ID, more reliable for shadow):
```
GET https://api.cow.fi/arbitrum/api/v1/solver_competition/{auction_id}
```

Response contains:
```json
{
  "auctionId": 12345,
  "solutions": [
    {
      "solver": "0xabc...",
      "solverName": "Gnosis_1inch",
      "score": "1234567890000000000",
      "ranking": 1
    },
    ...
  ]
}
```

Extract: `winner_solver`, `winner_score`, `our_rank` (find our solver address in the list).

### 2. Background polling task

- Spawn a `tokio::spawn` task at startup
- After each solve, push the auction_id to a channel/queue
- Consumer waits ~60-90 seconds (settlement delay), then queries CoW API
- Retries up to 3 times with backoff (auction may not be settled yet)
- On success: update `replay.db` with winner data, call `accounting::record_realized()`, call `revenue::record_win()` if we won

### 3. Wire into replay.db

Update the existing NULL columns:
```sql
UPDATE auction_log SET
  winning_solver = ?,
  winning_score_wei = ?,
  score_delta_wei = our_score_wei - winning_score_wei
WHERE id = ?
```

### 4. Dashboard panel (new section in dashboard.rs)

Add a `CompetitivenessStats` struct:
```rust
pub struct CompetitivenessStats {
    pub total_compared: u64,        // auctions where we got winner data
    pub wins: u64,                  // our score was highest
    pub within_10pct: u64,          // our score within 10% of winner
    pub within_50pct: u64,          // within 50%
    pub median_delta_pct: f64,      // median % gap to winner
    pub best_rank: u32,             // best rank we achieved
    pub avg_rank: f64,              // average rank
    pub top_winner: String,         // solver that beats us most often
}
```

Display as a card on the dashboard with:
- Win rate badge (green if >0%, target display)
- "Gap to winner" distribution
- Trend sparkline (are we getting closer?)

### 5. Theoretical earnings calculation

For every auction where our score would have been #1:
```rust
theoretical_earning_wei = our_surplus_wei - gas_cost_wei
```

Wire into the existing `RevenueTracker` (`monitoring/revenue.rs`):
- Call `revenue::record_win(surplus_wei, gas_wei)` when our score > winner score
- Daily/weekly aggregation already implemented, just needs to be called
- Convert to ETH: `earning_eth = earning_wei / 1e18`
- Convert to USD: use ETH price from auction's token reference prices (already in payload)

Wire into the existing `PnlStats` card (`routes/dashboard.rs`):
- Fill `realized_surplus_gwei` with winner comparison data
- Fill `net_pnl_gwei` = realized_surplus - gas_cost
- Add new fields: `theoretical_today_eth`, `theoretical_week_eth`, `theoretical_today_usd`, `theoretical_week_usd`
- When win count is 0, show "Closest miss" instead (smallest gap auction)

Call `accounting::record_realized()` with settlement data when CoW API returns results.

### 6. Config

| Env var | Default | Purpose |
|---------|---------|---------|
| `COW_API_BASE` | `https://api.cow.fi/arbitrum` | CoW API base URL |
| `COMPETITION_POLL_DELAY_SECS` | `90` | Delay before querying results |
| `COMPETITION_ENABLED` | `true` | Kill switch |
| `SOLVER_ADDRESS` | (required) | Our solver address to find ourselves in rankings |

---

## Important notes

- The CoW API is public, no auth needed
- Rate limit is generous but add a 1-2 second delay between queries
- Not every auction will have competition data (some may be cancelled)
- In shadow mode, we may not appear in the rankings at all - that's fine, just compare our score vs the winner's score
- Our solver address for Arbitrum should be in `.env` or config

---

## Expected outcome

After this is live for 24 hours, the dashboard should show something like:

```
Shadow Competitiveness (last 24h)
Compared: 847 auctions
Wins: 0 (0.0%)           -- expected in shadow
Within 10%: 23 (2.7%)
Within 50%: 156 (18.4%)
Median gap: -73.2%
Avg rank: 8.4 / 12
Top competitor: Gnosis_1inch (won 312x)
```

That single panel tells you more about solver readiness than every other dashboard metric combined.

### Theoretical earnings estimate

For every auction where our score would have been #1, compute:
```
theoretical_earning = our_surplus_wei - gas_cost_wei
```

The CoW protocol pays solvers the **surplus they generate minus execution cost**. So for each "would-have-won" auction, that's our theoretical payout.

Dashboard should show:
```
Theoretical Earnings (if we had won)
Today: 0.0042 ETH ($12.30)
This week: 0.031 ETH ($91.20)
Per-win avg: 0.0008 ETH ($2.35)
```

Even when win count is 0, show:
```
Closest miss: 0.0003 ETH surplus on auction #48291 (lost by 2.1%)
```

This answers "how much money would we be making?" directly.

To convert to USD, use the ETH price from the auction's token reference prices (already in the payload, no extra API call needed).

---

## Files to create/modify

| File | Action |
|------|--------|
| `solver-engine/src/competition.rs` | **NEW** - CoW API client + polling task |
| `solver-engine/src/main.rs` | Add `tokio::spawn(competition::run_poll_task())` |
| `solver-engine/src/routes/solve.rs` | Push auction_id to competition channel after solve |
| `solver-engine/src/replay.rs` | Add `update_competition_result()` function |
| `solver-engine/src/routes/dashboard.rs` | Add `CompetitivenessStats` section |
| `solver-engine/src/lib.rs` | Add `pub mod competition;` |
| `.env` | Add `SOLVER_ADDRESS` |
