# Liquorice

## What It Is
A private market maker (PMM) aggregator built specifically for CoW Protocol solvers. Instead of integrating Bebop, Hashflow, and 10 other market makers individually, Liquorice gives you all of them through one API.

When you query Liquorice, it fans out to multiple professional market makers simultaneously and returns the best quote. These market makers have deep off-chain inventory and often beat on-chain DEX pools by 0.1-0.5%, especially on large trades ($10K+).

## Why It Matters
The research team found that Liquorice is **literally wired into the CoW driver codebase** - the `example.toml` has a `[liquidity-sources-notifier.liquorice]` section. Top-performing solvers like helixbox-solve and rizzolver likely use Liquorice or a similar service to access private liquidity at scale.

From their website:
> *"With just one integration, gain seamless access to multiple private market makers. Liquorice's offchain service collects quotes from all PMMs and selects the best one for your needs."*

It provides unified access to CoW Swap, Uniswap X, 1inch Fusion, and Bebop ecosystems.

## How It Fits In
Liquorice operates as a "liquidity sources notifier" - the driver notifies Liquorice when it settles a trade, allowing Liquorice's market makers to provide inventory. It's tighter than a simple RFQ:

```
Auction arrives → Solver queries Liquorice for PMM quotes
   → Combine best PMM price with on-chain routing
   → Submit solution → Win auction
   → Driver notifies Liquorice of settlement
   → Market maker provides inventory
```

## API
- **Base URL:** `https://api.liquorice.tech/`
- **Auth:** API key required
- **Dashboard:** `https://app.liquorice.tech/`

## Configuration
```
# .env (when we have the key)
LIQUORICE_API_KEY=your_key_here
```

## Cost
Unknown - contact-based. Historically free for early adopters.

## Current Status
**Pending.** Account created at `app.liquorice.tech`. Discord ticket open for secret code / API key activation. No response yet.

## Expected Impact
+3-5% win rate on large orders. This is the single integration most likely to close the gap between our 0% win rate and genuine competitiveness.

## Key References
- Website: liquorice.tech
- App: app.liquorice.tech
- CoW driver config: `[liquidity-sources-notifier.liquorice]` in `example.toml`
- Research finding: `plan/research/research-correction.md` → Critical Finding #2
