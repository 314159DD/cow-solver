# Competition Tracking - Shadow Comparison System

**What this is:** How we compare our solutions against real auction winners to measure our competitiveness without being registered as a live solver.

## How It Works

```
1. We solve an auction → produce score (e.g., 5.5M gwei)
2. Wait 30 seconds for settlement
3. Query CoW API: api.cow.fi/arbitrum_one/api/v1/solver_competition/{auction_id}
4. API returns: winner solver name, winner score, all solver rankings
5. Compare: our score vs winner score → gap percentage
6. Display on dashboard
```

## What the API Returns

For each settled auction, the CoW API publishes:
- Which solver won (e.g., "helixbox-solve")
- Their score
- All participating solvers with scores and rankings
- The orders that were filled

~80% of auctions DON'T settle (nobody finds a profitable route). For these, the API returns 404 and we mark them as "no settle" on the dashboard.

## Competition Classifications

| Classification | Criteria | What it means |
|---------------|----------|---------------|
| **Competitive** | Gap ≤ ±100% | We're in the game |
| **Inflated** | Gap > +100% | Our constant score exceeds a tiny auction |
| **Close loss** | Gap -10% to -80% | Winnable with better data |
| **Outclassed** | Gap < -80% | Big auction we can't compete on (yet) |

## Top Competitors (From Shadow Data)

| Solver | Wins | Notes |
|--------|------|-------|
| helixbox-solve | 37% | Most frequent winner |
| rizzolver | 23% | Wins big auctions |
| zeroex-solve | 13% | Our closest competitor (-19.9% gap) |
| extquasimodo-solve | 7% | |
| sector-solve | 7% | |

## Earnings Display

The dashboard shows per-auction potential earnings:
- **Earnings column:** `min(our_score, winner_score)` converted to USD
- **Genuine wins:** Gap ≤ 100% and we scored higher
- **Win rate:** Only counts genuine wins (excludes inflated)
- **Projected revenue:** Extrapolated daily/monthly from genuine win rate

## Key Files

| File | Role |
|------|------|
| `competition.rs` | Background poller, CoW API client, result storage |
| `routes/dashboard.rs` | Dashboard rendering with earnings + gap display |
| `replay.rs` | Persistent storage of auction results |

## API Details

- **Endpoint:** `GET /api/v1/solver_competition/{auction_id}`
- **Base URL:** `https://api.cow.fi/arbitrum_one`
- **Poll delay:** 30 seconds after auction (settlements take time)
- **Retry:** Once after 60 seconds if first attempt returns 404
- **Rate limit:** 500ms between API calls
