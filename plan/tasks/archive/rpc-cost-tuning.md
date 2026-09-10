# RPC Cost Tuning Guide

**Provider:** Alchemy Pay-As-You-Go | **Chain:** Arbitrum (42161) | **Last updated:** 2026-03-27

---

## How Alchemy billing works

Alchemy charges per **Compute Unit (CU)**, not per request. Different RPC methods cost different CUs:

| Method | CU cost | We use it for |
|--------|---------|---------------|
| `eth_blockNumber` | 10 | Checking if a new block arrived |
| `eth_call` | 26 | Reading pool reserves (`getReserves`) |
| `eth_call` (batch of N) | 26 × N | Batching doesn't reduce CU cost |

**Pricing:** $0.45/M CU (first 300M/month), $0.40/M CU after that.

---

## What eats our CUs

The pool indexer background task accounts for **99%+ of all RPC spend**. Everything else is noise.

| Component | What it does | Interval | CU/cycle | % of bill |
|-----------|-------------|----------|----------|-----------|
| **Pool indexer (hot)** | Refreshes reserves for pools seen in recent auctions | 30s | ~30 pools × 26 = 780 + 10 (blockNum) = **790** | ~45% |
| **Pool indexer (full)** | Refreshes ALL cached pools | 5 min | ~383 pools × 26 = 9,958 + 10 = **9,968** | ~50% |
| **Gas oracle** | Reads Arbitrum gas prices | 60s | 2 × 26 = **52** | ~3% |
| **Solve handler** | Zero on-chain calls (reads from cache) | per auction | **0** | 0% |
| **Pool discovery** | Queries factory contracts for new pools | manual button | ~1,500 × 26 = **39,000** (one-off) | 0% |
| **Competition tracker** | Uses CoW API, not Alchemy | 5s | **0** | 0% |

---

## The three knobs

### 1. `FAST_REFRESH_SECS` (pool_indexer.rs)

**Current: 30 seconds** | How often hot pools get refreshed.

| Value | Monthly CU (hot) | Monthly cost (hot only) | Trade-off |
|-------|-----------------|------------------------|-----------|
| 10s | ~200M | ~$90 | Freshest data, best for competing |
| **30s** | **~68M** | **~$30** | Good balance - matches auction cadence |
| 60s | ~34M | ~$15 | Reserves can be 1-2 auctions stale |
| 120s | ~17M | ~$8 | Noticeable staleness, weaker scores |

**Rule of thumb:** CoW auctions arrive every ~30s. Refreshing faster than that is waste. Refreshing slower means we're solving on old data.

### 2. `SLOW_REFRESH_SECS` (pool_indexer.rs)

**Current: 300 seconds (5 min)** | How often ALL pools get a full sweep.

| Value | Monthly CU (full) | Monthly cost (full only) | Trade-off |
|-------|-------------------|-------------------------|-----------|
| 120s (2 min) | ~215M | ~$97 | All pools always warm |
| **300s (5 min)** | **~86M** | **~$39** | Reasonable freshness for cold pools |
| 600s (10 min) | ~43M | ~$19 | Cold pools quite stale, but hot ones fine |
| 900s (15 min) | ~29M | ~$13 | Only worth it if budget is very tight |

**When to increase:** If we notice winning auctions use pools that aren't in our "hot" set (the solve handler marks pools from incoming auctions as hot, but if we're missing routes that competitors find, the full sweep frequency matters).

### 3. `MAX_HOT_POOLS` (pool_indexer.rs)

**Current: 30** | How many pools get fast-cycle treatment.

| Value | Effect on cost | Trade-off |
|-------|---------------|-----------|
| 15 | Halves hot refresh cost | May miss relevant pools in diverse auctions |
| **30** | Baseline | Covers typical auction pool diversity |
| 50 | +67% hot cost | Better for auctions with many token pairs |
| 100 | +233% hot cost | Only if we see lots of unique pools per auction |

**How to check:** Look at dashboard logs for "Fast cycle: refreshing hot pools" - if the number is consistently hitting the cap, raise it.

---

## Current config and cost

```
FAST_REFRESH_SECS = 30
SLOW_REFRESH_SECS = 300
MAX_HOT_POOLS = 30
Gas oracle interval = 60s
```

**Estimated monthly cost: ~$68**

| Bucket | CU/month | $/month |
|--------|----------|---------|
| Hot pool refresh (30 pools @ 30s) | ~68M | ~$30 |
| Full pool refresh (383 pools @ 5min) | ~86M | ~$39 |
| Gas oracle (2 calls @ 60s) | ~2.2M | ~$1 |
| Block number polls (@ 30s) | ~0.9M | <$1 |
| **Total** | **~157M** | **~$68** |

---

## Scaling up when revenue justifies it

Once we're winning auctions and earning revenue, tighter refresh = better scores = more wins. Here are two preset profiles:

### Budget mode (~$35/month)
```rust
const FAST_REFRESH_SECS: u64 = 60;
const SLOW_REFRESH_SECS: u64 = 600;
const MAX_HOT_POOLS: usize = 20;
// Gas oracle: 120s
```

### Competitive mode (~$200/month)
```rust
const FAST_REFRESH_SECS: u64 = 10;
const SLOW_REFRESH_SECS: u64 = 120;
const MAX_HOT_POOLS: usize = 50;
// Gas oracle: 30s
```

### Aggressive mode (~$500/month)
```rust
const FAST_REFRESH_SECS: u64 = 5;
const SLOW_REFRESH_SECS: u64 = 60;
const MAX_HOT_POOLS: usize = 100;
// Gas oracle: 15s
```

---

## How to monitor actual spend

1. **Alchemy dashboard:** https://dashboard.alchemy.com - shows real-time CU usage
2. **Solver logs:** grep for "Fast cycle" and "Slow cycle" to see actual pool counts per refresh
3. **Set Alchemy alerts:** Configure a monthly CU budget alert at your comfort level

---

## What DOESN'T cost CUs

- Pool discovery (manual, one-off, ~39K CU per run)
- Competition tracker (hits CoW API, not Alchemy)
- Dashboard/health endpoints (local only)
- Monitoring/alerting (local metrics)
- Solve computation (purely in-memory)

---

## File locations

| Setting | File | Line |
|---------|------|------|
| `FAST_REFRESH_SECS` | `solver-engine/src/pool_indexer.rs` | const near top |
| `SLOW_REFRESH_SECS` | `solver-engine/src/pool_indexer.rs` | const near top |
| `MAX_HOT_POOLS` | `solver-engine/src/pool_indexer.rs` | const near top |
| Gas oracle interval | `solver-engine/src/main.rs` | `run_gas_refresh(..., 60)` |
| Hot pool marking | `solver-engine/src/routes/solve.rs` | `mark_hot()` call in solve handler |
