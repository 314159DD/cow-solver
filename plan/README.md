# CoW Solver - Plan Directory

**Last Updated:** 2026-03-31
**Status:** Shadow mode on Arbitrum. Awaiting V3 liquidity activation from CoW team.

## Folder Structure

```
plan/
├── README.md                   ← You are here
├── PRODUCT_VISION.md           ← What we're building and why
├── ROADMAP.md                  ← Phase-level progress tracker
├── architecture/               ← How each system works (PM-readable)
│   ├── README.md               ← System overview + data flow diagram
│   ├── solving-pipeline.md     ← The 5-phase solving engine
│   ├── pool-data.md            ← How we source and refresh pool data
│   ├── scoring.md              ← How scores are computed and why they matter
│   ├── competition.md          ← Shadow comparison + earnings tracking
│   ├── aggregators.md          ← External price sources (0x, Bebop, Paraswap, OKX)
│   └── dashboard.md            ← Command center dashboard explained
├── decisions/                  ← Why we made specific technical choices
│   └── techstack.md            ← Rust, Axum, Alchemy, etc.
├── tasks/                      ← Sprint task breakdowns (historical)
│   └── sprint-win-rate.md      ← Current active sprint
└── research/                   ← External research findings
    ├── research-handoff.md     ← Brief we gave to researchers
    ├── research-result.md      ← Round 1 findings
    └── research-correction.md  ← Round 2 audit + corrections
```

## Quick Links

| Question | Read this |
|----------|-----------|
| What does this project do? | [PRODUCT_VISION.md](PRODUCT_VISION.md) |
| What's our current progress? | [ROADMAP.md](ROADMAP.md) |
| How does the solver work? | [architecture/solving-pipeline.md](architecture/solving-pipeline.md) |
| Where does pool data come from? | [architecture/pool-data.md](architecture/pool-data.md) |
| How do we compare against competitors? | [architecture/competition.md](architecture/competition.md) |
| What's the revenue potential? | [architecture/scoring.md](architecture/scoring.md) |
| What external APIs do we use? | [architecture/aggregators.md](architecture/aggregators.md) |
| What's the current sprint? | [tasks/sprint-win-rate.md](tasks/sprint-win-rate.md) |
