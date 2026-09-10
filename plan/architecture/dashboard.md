# Dashboard - Command Center

**What this is:** A real-time web dashboard showing solver performance, competition data, and revenue tracking.

**URL:** `http://<server_ip>:8000/dashboard`

## Sections

### Header Bar
- **Status indicator:** Live / Idle / Waiting for auctions
- **Uptime, Block number, ETH price, RPC cost**

### KPI Cards
- **Auctions Received:** Total POST /solve calls
- **Submitted:** Solutions actually sent to driver
- **Held:** Solutions found but not submitted (sim revert, risk policy)
- **Avg Solve Time:** Wall-clock time per auction

### Score Distribution
- Min, average, max scores across all auctions this session
- Indicates whether scores are constant (bad) or variable (good)

### Strategy Attribution
Shows which solving strategy wins the internal competition:
- **direct:** Single-pool routing (Phase 1)
- **graph:** Multi-hop graph search (Phase 4)
- **cow:** Coincidence of Wants matching
- **combined/multihop/split:** Phase 2-3 strategies
- Strategies marked "outscored" generate solutions but never beat the winning strategy

### Shadow Competitiveness
- **Auctions Compared:** How many settled auctions we've compared against
- **Within 10%/50%:** Percentage of auctions where our score is close to winner
- **Median Gap:** Our typical distance from the winner (0% = tied)
- **Top Competitor:** Who beats us most often

### Recent Auctions Table
Each row shows one auction with:
- **Our Score:** What we computed (gwei)
- **Winner:** Real winner's score (from CoW API)
- **Gap:** Percentage difference (green = we beat them, red = they beat us, orange ⚠ = inflated)
- **Earnings:** Estimated USD if we had won

**"No settle"** = nobody won that auction (80% of batches).
**"Pending"** = competition data not yet fetched (30-90s delay).

Toggle "Show all (incl. no-settle)" to see everything. Default view filters to only settled auctions with real competition data.

### Earnings Summary
Below the auction table:
- Genuine wins vs losses vs inflated
- Win rate (only genuine wins count)
- Earned amount (if we were live)
- Projected daily/monthly revenue

## Key Files

| File | Role |
|------|------|
| `routes/dashboard.rs` | Full HTML/CSS/JS dashboard (single file, ~1300 lines) |
| `routes/solve.rs` | Records metrics after each auction |
| `monitoring/mod.rs` | In-memory counters (auctions, scores, solve times) |
| `competition.rs` | Fetches real winner data for comparison |
| `replay.rs` | SQLite DB storing all auction history |
