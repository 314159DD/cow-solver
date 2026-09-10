# Solver Audit Report

**Date:** 2026-04-07
**Audited against:** plan/research/cow reference solver/06-bug-analysis-key-differences.md

## 10 Known Bugs - All Fixed

| # | Bug | Status |
|---|-----|--------|
| 1 | executedAmount includes fee | ✅ FIXED - fee always empty |
| 2 | Wrong clearing price formula | ✅ FIXED - prices[sell]=buy_out, prices[buy]=sell_in |
| 3 | Swapped token prices | ✅ FIXED - correctly oriented |
| 4 | Missing buy output cap | ✅ FIXED - min(output, order.buy_amount) |
| 5 | Floor vs ceil division | ✅ FIXED - ceil+1 for amount_in |
| 6 | Fee field on market orders | ✅ FIXED - skip_serializing_if prevents it |
| 7 | ETH vs WETH | ℹ️ Harmless - driver pre-wraps |
| 8 | Non-numeric liquidity IDs | ✅ FIXED - uses auction's numeric IDs |
| 9 | Transitive token prices | ✅ Correct - only sell/buy needed per spec |
| 10 | Slippage double-application | ✅ FIXED - exact amounts only |

## Silent Failures Found & Fixed

| Issue | Status | Impact |
|-------|--------|--------|
| **V3 pool parsing (304 pools dropped)** | ✅ FIXED 2026-04-07 | CRITICAL - was losing all V3 liquidity |
| Token address case sensitivity | ✅ Safe - all lookups use to_lowercase() |
| Serde deserialization drops | ✅ Safe - lenient parsing with skip |
| Overflow in scoring math | ✅ Safe - checked/saturating arithmetic |
| unwrap_or(0) masking | ℹ️ Acceptable - appropriate for missing data |

## Overall: Grade A-
No blocking issues. V3 pool fix was the last major silent failure.
