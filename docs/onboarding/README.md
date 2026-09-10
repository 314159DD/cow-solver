# CoW Protocol Solver Onboarding

This document tracks the onboarding process for this solver to enter CoW Protocol production competition.

## Process Overview

```
Shadow Competition -> Onboarding Call -> KYC -> Barn/Staging -> Production
     (test env)         (with CoW team)                (1+ week)       (Tuesdays)
```

## Status Tracker

| Step | Status | Notes |
|------|--------|-------|
| 1. Shadow competition | ⏳ Pending | Need Telegram registration + public endpoint |
| 2. Onboarding call | ⏳ Pending | Schedule after shadow >50% validity |
| 3. KYC documentation | ⏳ Pending | Docs required before barn |
| 4. Staging (barn) | ⏳ Pending | After KYC approval |
| 5. Production | ⏳ Pending | Promoted on Tuesday releases |

## Step 1: Shadow Competition

The shadow competition is a test environment where solutions are scored but not settled on-chain.

**Prerequisites:**
- Solver passing local tests (`bash scripts/local_test.sh`)
- Public HTTPS endpoint (ngrok or a public host)
- Arbitrum RPC endpoint

**How to connect:**
```bash
# Start ngrok tunnel
ngrok http 8000
# Note your URL: https://xxxx.ngrok-free.app

# Start solver with Arbitrum config
export RPC_URL=https://arb-mainnet.g.alchemy.com/v2/YOUR_KEY
export CHAIN_ID=42161
docker-compose up -d

# Run shadow test script
export PUBLIC_URL=https://xxxx.ngrok-free.app
bash scripts/shadow_test.sh
```

**Registration:**
1. Join CoW Solvers Telegram: https://t.me/cowprotocolsolver
2. Message: "Hi, I'd like to register for the shadow competition. Solver name: cow-solver, endpoint: `<YOUR_URL>/solve`, chain: Arbitrum"
3. Wait for confirmation and first auction

**Acceptance criteria for proceeding:**
- At least 50% of submitted solutions pass CoW validation
- Score within 2× of winning score
- Running for at least 48 hours

## Step 2: Onboarding Call

**Schedule via Telegram** after meeting shadow competition criteria.

**Agenda:**
- Demonstrate solver capabilities (show shadow competition metrics)
- Discuss bonding pool terms (amount, lock period)
- Review EBBO compliance and solution quality
- Get assigned submission addresses for barn competition

**Prepare:**
- Screenshot of shadow competition dashboard showing validity %
- Technical overview of solving strategies (direct, multi-hop, CoW matching)
- Architecture doc: `plan/architecture/README.md`

## Step 3: KYC Documentation

Required before barn access:

| Document | Format | Notes |
|----------|--------|-------|
| Company/entity registration | PDF | Or individual identity for solo developers |
| Developer passport(s) | PDF/JPG | All developers who will sign transactions |
| Rewards wallet on Arbitrum | Address | Where surplus payments go |
| Rewards wallet on Mainnet | Address | For mainnet surplus (future) |

**Submit to:** CoW Protocol team (instructions provided on onboarding call)

**Save copies in:** `docs/onboarding/kyc/` (gitignored, do not commit PII)

## Step 4: Barn / Staging Competition

Barn is a public staging environment: real CoW Protocol infrastructure but not production.

**Connection details** (provided by CoW team after KYC):
- Submission address on Arbitrum: `<TBD>`
- Submission address on Mainnet: `<TBD>`
- Barn API: `https://barn.api.cow.fi/arbitrum_one/api/v1/`

**Run for at least 1 week** with:
- Solution validity > 80%
- No EBBO violations
- Positive revenue trending (surplus > gas)

**Monitor with:**
```bash
# Check metrics
curl http://localhost:8000/metrics

# Check revenue
cat data/revenue.json | jq .

# Shadow test logs
tail -f /tmp/shadow-metrics.log
```

## Step 5: Production

New solvers are promoted on **Tuesday releases** (CoW Protocol deployment schedule).

**Before first auction:**
1. Verify `data/revenue.json` tracking is working
2. Confirm alerting is configured (Discord/PagerDuty webhook)
3. Ensure solver restarts automatically (`restart: always` in docker-compose)
4. Set up log rotation and monitoring

**First 24 hours:**
- Watch for auction_received events in logs
- Verify at least one solution submitted
- Check `/metrics` for win_rate > 0
- Review revenue.json daily summary

## Key Contacts & Resources

- CoW Solvers Telegram: https://t.me/cowprotocolsolver
- Solver docs: https://docs.cow.fi/cow-protocol/concepts/introduction/solvers
- Solver API spec: https://docs.cow.fi/cow-protocol/reference/apis/solver
- Competition rules: https://docs.cow.fi/cow-protocol/reference/core/auctions/competition-rules
- Reference implementation: https://github.com/cowprotocol/services

## Appendix: Solver Technical Summary

For the onboarding call, here is a brief technical summary:

**Strategies implemented:**
1. **CoW matching** - detects opposite-direction orders and settles directly (no DEX)
2. **Direct routing** - single-pool execution on best available pool
3. **Multi-hop routing** - 2-3 hop routes via WETH/USDC intermediaries
4. **Split routing** - large orders split across multiple pools

**DEX integrations:**
- Uniswap V2 + Sushiswap (V2 math with 0.3% fee)
- Uniswap V3 (concentrated liquidity, all 4 fee tiers: 0.01%, 0.05%, 0.3%, 1%)

**Chains:** Mainnet (chain 1), Arbitrum One (chain 42161)

**EBBO compliance:** Implemented - solutions must beat or match reference price from UniV3

**UDCP compliance:** Enforced - all orders in same direction receive uniform clearing price
