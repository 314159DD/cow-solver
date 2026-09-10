# CoW Solver - Current Status

**Updated:** 2026-04-07 23:00 UTC
**Phase:** v2.3 - Critical timeout fix deployed. Aggregator sub-timeout prevents Phase 1 solutions from being discarded.

---

## v2.3 Hotfix (April 7 evening)

### The Bug
937 consecutive auctions returned `result=empty, score=0, strategies=["timeout"]`.
Phase 1 (direct routing) was producing valid 867K gwei solutions in 300ms,
but Phase 5 (aggregators) ran 6 providers sequentially at 5s timeout each = 30s.
The outer 20s hard timeout fired mid-Phase 5, **discarding the Phase 1 solution**.

The driver sends duplicate requests for the same auction. Both timed out.

### The Fix
1. **Phase 5 sub-timeout**: Aggregator phase now gets `min(remaining_budget - 1s, 10s)`.
   If it times out, Phase 1-4 solutions are preserved and returned.
2. **Per-aggregator timeout**: Reduced from 5s to 3s (6 × 3s = 18s max, fits in budget).

### Expected Result
Phase 1 always returns. Phase 5 adds aggregator improvements when available,
times out gracefully when rate-limited (0x 429, KyberSwap 403, ParaSwap 429).
Scores should be ~867K-1.2M gwei consistently.

---

## Previous: v2.2 Changes (April 7 morning)

- 8 aggregators (Odos, 0x, Bebop, ParaSwap, KyberSwap, OpenOcean, 1inch, OKX)
- 5 competitive improvements (internalization, CoW filter, pair priority, CIP-67, fee bonus)
- Multi-aggregator racing in Phase 5
- Budget limiter for Odos free tier

---

## Data Collection History (April 7)

| Run | Duration | Auctions | Result | Problem |
|-----|----------|----------|--------|---------|
| Run 1 (morning) | ~2h | ~500 | Mixed | Old scoring baseline, pre-v2.2 |
| Run 2 (afternoon) | ~2h | 937 | ALL empty | v2.2 aggregators caused timeout bug |
| Run 3 (evening) | pending | - | - | v2.3 timeout fix deployed |

---

## Known Issues (Aggregators)

| Aggregator | Status | Issue |
|-----------|--------|-------|
| Odos | Working | Budget limiter protecting free tier |
| 0x | Rate limited | No API key, hitting 429. Set ZEROX_ENABLED=false or get key |
| Bebop | Partial | No liquidity on many pairs (expected) |
| ParaSwap | Rate limited | 429 on free tier |
| KyberSwap | Blocked | Cloudflare 403 from VPS IP |
| OpenOcean | Unknown | No errors but no successful quotes seen |
| 1inch | Working | Key configured, quotes successful |
| OKX | Working | Key configured |

---

## Approach: Iterate Until It Works

This solver WILL work. The architecture is sound - Phase 1 produces valid
solutions in 300ms. The problems have been operational (timeouts, rate limits,
DB issues), not fundamental.

Plan:
1. Deploy v2.3 with timeout fix
2. Collect 2h of clean data
3. Pull replay DB, analyze scores vs winners
4. Fix whatever shows up
5. Repeat until win rate > 0

No giving up. No pivoting away. We fix what's broken and try again.

---

## Key Files

| File | Purpose |
|------|---------|
| `plan/STATUS.md` | This file |
| `plan/SCORING_POSTMORTEM.md` | 9 failed scoring attempts history |
| `plan/research/cow-scoring-deep-dive.md` | Driver formula |
| `plan/research/intelligence-layer-evaluation.md` | Aggregator strategy |
| `plan/tasks/sprint-competitive-edge-v2.2.md` | v2.2 sprint record |
| `solver-engine/src/solver/mod.rs` | Solve orchestration (Phase 1-5) |
| `solver-engine/src/liquidity/aggregator/mod.rs` | Multi-aggregator racing |
