---
name: capital-allocator
description: Makes capital allocation decisions based on market regime and agent performance. Adjusts governance policy to control entry modes, kelly fractions, and per-agent allocation limits.
version: 1.0.0
user-invocable: true
metadata: {"openclaw":{"requires":{"bins":["curl","jq"]}}}
---

# Capital Allocator

Make capital allocation decisions by combining market regime analysis with per-agent performance metrics, then execute adjustments via governance policy updates.

## Allocation Policy Table

| Regime | Crypto Entry Mode | Kelly Fraction | Max Intent % of Capital |
|--------|------------------|----------------|------------------------|
| HighVol | vol_straddle | 0.15 | 50% |
| LowVol | arb_only | 0.30 | 100% |
| Trending | directional | 0.25 | 100% |
| Ranging | arb_only | 0.20 | 75% |

## Step 1: Gather Current State

### Get risk state and positions
```bash
RISK=$(curl -sf $PLOY_API_BASE/api/sidecar/risk -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN")
echo "$RISK" | jq '{risk_state: .risk_state, daily_pnl: .daily_pnl_usd, drawdown: .current_drawdown_usd, positions: (.positions | length), queue: .queue_depth}'
```

### Get current governance policy
```bash
POLICY=$(curl -sf $PLOY_API_BASE/api/governance/policy -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN")
echo "$POLICY" | jq '{block_new_intents: .block_new_intents, blocked_domains: .blocked_domains, max_intent: .max_intent_notional_usd, max_total: .max_total_notional_usd, metadata: .metadata}'
```

### Get governance status (includes agent states)
```bash
STATUS=$(curl -sf $PLOY_API_BASE/api/governance/status -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN")
echo "$STATUS" | jq .
```

### Get system capabilities (active agents/domains)
```bash
CAPS=$(curl -sf $PLOY_API_BASE/api/capabilities)
echo "$CAPS" | jq '{active_domains: .active_domains, coordinator: .coordinator_running, deployments: .scoped_enabled_deployments}'
```

### Get PnL history for performance scoring
```bash
PNL=$(curl -sf $PLOY_API_BASE/api/stats/pnl)
echo "$PNL" | jq '.'
```

## Step 2: Detect Current Regime

Use the regime-detector skill OR read the last confirmed regime from memory.

If no regime is recorded, run the regime-detector skill first.

## Step 3: Score Agent Performance

For each active agent/domain, compute a performance score:

**Composite Score Formula:**
```
score = 0.4 * sharpe_normalized + 0.3 * win_rate + 0.3 * (1 - drawdown_ratio)
```

Where:
- `sharpe_normalized` = clamp(sharpe / 2.0, 0, 1) — Sharpe of 2.0+ gets perfect score
- `win_rate` = wins / total_trades (from PnL data)
- `drawdown_ratio` = current_drawdown / max_allowed_drawdown

Score interpretation:
- **> 0.70**: Excellent — increase allocation
- **0.40 - 0.70**: Normal — maintain allocation
- **< 0.40**: Underperforming — consider pausing

## Step 4: Make Allocation Decisions

For each agent:

1. **If score < 0.30 AND not paused AND more than 1 agent active**:
   - Pause the agent via domain control
   - Record pause timestamp in memory for cooldown tracking

2. **If paused AND score >= 0.50 AND paused > 15 minutes ago**:
   - Resume the agent via domain control

3. **Adjust per-agent allocation**:
   - `agent_max_alloc = score * regime_max_intent_pct`
   - Clamp to max 40% of total capital per single agent

4. **SAFETY GUARD**: Never pause ALL agents. If pausing would leave 0 active agents, skip.

## Step 5: Execute Decisions

### Pause a domain
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/pause \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{"domain": "crypto"}' | jq .
```

### Resume a domain
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/resume \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{"domain": "crypto"}' | jq .
```

### Update governance with new allocation
```bash
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": false,
    "blocked_domains": [],
    "max_intent_notional_usd": <COMPUTED>,
    "max_total_notional_usd": <COMPUTED>,
    "updated_by": "openclaw.capital-allocator",
    "reason": "Allocation update: regime=<REGIME>, scores={crypto: X.XX, sports: Y.YY}"
  }' | jq .
```

## Step 6: Record & Report

Write allocation decision to memory:
```
allocation_decision:
  timestamp: <ISO8601>
  regime: <regime>
  scores: {crypto: X.XX, sports: Y.YY, politics: Z.ZZ}
  actions: [pause crypto | resume sports | adjust limits]
  new_max_intent: $XX.XX
  new_max_total: $XXX.XX
```

Report format:
```
Capital Allocation Update
─────────────────────────
Regime: <REGIME>
| Agent    | Score | Status  | Max Alloc |
|----------|-------|---------|-----------|
| crypto   | 0.72  | active  | 25%       |
| sports   | 0.45  | active  | 15%       |
| politics | 0.28  | PAUSED  | 0%        |
─────────────────────────
Action: Paused politics (score 0.28 < 0.30 threshold)
New limits: intent=$75, total=$300
```
