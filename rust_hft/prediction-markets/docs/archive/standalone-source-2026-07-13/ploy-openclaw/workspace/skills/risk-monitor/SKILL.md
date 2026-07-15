---
name: risk-monitor
description: Monitors risk metrics in real-time, detects dangerous conditions (drawdown spikes, circuit breaker events, position concentration), and triggers emergency actions when needed.
version: 1.0.0
user-invocable: true
metadata: {"openclaw":{"requires":{"bins":["curl","jq"]},"always":true}}
---

# Risk Monitor

Continuously monitor the Ploy trading system for dangerous risk conditions and take protective action when thresholds are breached.

## Risk Thresholds

| Metric | Warning | Critical | Action |
|--------|---------|----------|--------|
| Daily PnL | < -50% of limit | < -80% of limit | Block intents / Halt |
| Drawdown | > 50% of limit | > 80% of limit | Reduce allocation / Halt |
| Circuit Breaker | Any event | 3+ events/hour | Block intents / Pause domain |
| Position Count | > 15 | > 25 | Block new intents |
| Single Position Loss | > -$30 | > -$50 | Alert / Force close |
| Queue Depth | > 10 | > 20 | Block intents (backpressure) |
| Backend Health | degraded | unreachable | Alert immediately |

## Step 1: Check System Health

```bash
HEALTH=$(curl -sf --max-time 5 $PLOY_API_BASE/health 2>/dev/null)
if [ $? -ne 0 ]; then
  echo "CRITICAL: Backend unreachable!"
fi
echo "$HEALTH" | jq .
```

If backend is unreachable, alert immediately and skip other checks.

## Step 2: Check Risk State

```bash
curl -sf $PLOY_API_BASE/api/sidecar/risk -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" | jq '{
  risk_state: .risk_state,
  daily_pnl: .daily_pnl_usd,
  daily_limit: .daily_loss_limit_usd,
  pnl_ratio: (if .daily_loss_limit_usd != 0 then (.daily_pnl_usd / .daily_loss_limit_usd * 100 | round) else 0 end),
  drawdown: .current_drawdown_usd,
  drawdown_limit: .drawdown_limit_usd,
  drawdown_ratio: (if .drawdown_limit_usd != null and .drawdown_limit_usd != 0 then (.current_drawdown_usd / .drawdown_limit_usd * 100 | round) else null end),
  queue_depth: .queue_depth,
  position_count: (.positions | length),
  circuit_breakers: (.circuit_breaker_events | length),
  worst_position: (.positions | min_by(.pnl_usd) | {market: .market, pnl: .pnl_usd})
}'
```

## Step 3: Check Positions

```bash
curl -sf $PLOY_API_BASE/api/sidecar/positions -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" | jq '[.[] | select(.status == "OPEN")] | {
  total_open: length,
  total_pnl: ([.[].pnl // 0] | add),
  worst: (min_by(.pnl) | {market: .market_slug, pnl: .pnl, shares: .shares}),
  stale: [.[] | select((.opened_at | fromdateiso8601) < (now - 172800))] | length
}'
```

## Step 4: Evaluate Risk Level

Based on the gathered data, classify the overall risk level:

### GREEN (Normal)
- Daily PnL within 50% of limit
- Drawdown within 50% of limit
- No circuit breaker events
- Queue depth < 10
- All positions within acceptable loss

**Action**: No changes needed.

### YELLOW (Warning)
- Daily PnL between 50-80% of limit OR
- Drawdown between 50-80% of limit OR
- 1-2 circuit breaker events in last hour OR
- Queue depth 10-20 OR
- Any position losing > $30

**Action**: Alert user. Consider reducing allocation limits.

```bash
# Reduce max intent by 50%
CURRENT_MAX=$(curl -sf $PLOY_API_BASE/api/governance/policy -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq -r '.max_intent_notional_usd')
NEW_MAX=$(echo "$CURRENT_MAX * 0.5" | bc -l)
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d "{
    \"block_new_intents\": false,
    \"blocked_domains\": [],
    \"max_intent_notional_usd\": $NEW_MAX,
    \"updated_by\": \"openclaw.risk-monitor\",
    \"reason\": \"YELLOW risk: reducing allocation 50%\"
  }" | jq .
```

### RED (Critical)
- Daily PnL beyond 80% of limit OR
- Drawdown beyond 80% of limit OR
- 3+ circuit breaker events in last hour OR
- Queue depth > 20 OR
- Risk state is not "normal"

**Action**: Block all new intents immediately. Alert user urgently.

```bash
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": true,
    "blocked_domains": [],
    "updated_by": "openclaw.risk-monitor",
    "reason": "RED risk: blocking all new intents"
  }' | jq .
```

### BLACK (Emergency)
- Backend unreachable for > 2 checks OR
- Daily PnL exceeds 100% of limit OR
- Multiple positions losing > $50

**Action**: Emergency halt — force-close all positions.

```bash
curl -sf -X POST $PLOY_API_BASE/api/system/halt \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

**WARNING**: This is a destructive action. Only use when automated risk controls have failed.

## Step 5: Record & Alert

Write risk assessment to memory:
```
risk_check:
  timestamp: <ISO8601>
  level: GREEN|YELLOW|RED|BLACK
  daily_pnl: $X.XX / $Y.YY limit
  drawdown: $X.XX / $Y.YY limit
  positions: N open
  action: <none|reduced limits|blocked intents|emergency halt>
```

If YELLOW or worse, always send an alert summarizing the condition and action taken.
