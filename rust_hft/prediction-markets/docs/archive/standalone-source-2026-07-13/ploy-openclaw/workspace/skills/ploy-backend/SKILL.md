---
name: ploy-backend
description: Core interface to the Ploy Polymarket trading bot REST API. Provides commands for health checks, positions, risk state, intent submission, governance policy, and system control.
version: 1.0.0
user-invocable: true
metadata: {"openclaw":{"requires":{"bins":["curl","jq"]},"primaryEnv":"PLOY_API_ADMIN_TOKEN","always":true}}
---

# Ploy Trading Backend API

The Ploy trading backend runs at `$PLOY_API_BASE` (default: `http://localhost:8081`).

Use `curl` with `jq` for all API interactions. Always include the appropriate auth header.

## Authentication

Two auth levels:
- **Admin** (system control, governance, config): `-H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN"`
- **Sidecar** (positions, risk, intents, orders): `-H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN"`

Some endpoints accept either token (sidecar OR admin).

---

## Health & System Status

### Health Check (no auth)
```bash
curl -sf $PLOY_API_BASE/health | jq .
```
Returns: `{ "status": "ok"|"degraded", "db": "connected"|"disconnected", "uptime_secs": N }`

### System Status (admin)
```bash
curl -sf $PLOY_API_BASE/api/system/status -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```
Returns: `{ "status", "uptime_seconds", "version", "strategy", "websocket_connected", "database_connected", "error_count_1h" }`

### Platform Capabilities (no auth)
```bash
curl -sf $PLOY_API_BASE/api/capabilities | jq .
```
Returns: supported domains, active domains, deployment counts, system controls.

---

## Positions & Risk

### Get Current Positions (sidecar)
```bash
curl -sf $PLOY_API_BASE/api/sidecar/positions -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" | jq .
```
Returns array of: `{ "id", "market_slug", "token_id", "side", "shares", "avg_price", "current_value", "pnl", "status", "opened_at" }`

### Get Risk State (sidecar)
```bash
curl -sf $PLOY_API_BASE/api/sidecar/risk -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" | jq .
```
Returns: `{ "risk_state", "daily_pnl_usd", "daily_loss_limit_usd", "current_drawdown_usd", "max_drawdown_observed_usd", "drawdown_limit_usd", "queue_depth", "positions": [...], "circuit_breaker_events": [...] }`

---

## Order & Intent Submission

### Submit Intent (sidecar) — Preferred method
```bash
curl -sf -X POST $PLOY_API_BASE/api/sidecar/intents \
  -H "Content-Type: application/json" \
  -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" \
  -d '{
    "deployment_id": "openclaw-meta",
    "agent_id": "openclaw",
    "domain": "crypto",
    "market_slug": "will-btc-reach-100k",
    "token_id": "0x...",
    "side": "UP",
    "order_side": "BUY",
    "size": 10,
    "price_limit": 0.55,
    "reason": "HighVol regime, vol_straddle entry",
    "confidence": 0.75,
    "edge": 0.08,
    "priority": "normal",
    "dry_run": true,
    "metadata": {}
  }' | jq .
```
Returns: `{ "success": true, "intent_id": "uuid", "message": "...", "dry_run": true }`

**IMPORTANT**: Always set `dry_run: true` unless explicitly authorized for live trading.

### Submit Order (sidecar, requires PLOY_SIDECAR_ORDERS_LIVE_ENABLED=true)
```bash
curl -sf -X POST $PLOY_API_BASE/api/sidecar/orders \
  -H "Content-Type: application/json" \
  -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" \
  -d '{
    "strategy": "openclaw",
    "domain": "crypto",
    "market_slug": "...",
    "token_id": "...",
    "side": "UP",
    "shares": 10,
    "price": 0.55,
    "dry_run": true
  }' | jq .
```

---

## Governance Policy

### Get Current Policy (admin)
```bash
curl -sf $PLOY_API_BASE/api/governance/policy -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```
Returns: `{ "block_new_intents", "blocked_domains": [], "max_intent_notional_usd", "max_total_notional_usd", "metadata": {}, "updated_by", "updated_at" }`

### Get Governance Status (admin)
```bash
curl -sf $PLOY_API_BASE/api/governance/status -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

### Update Governance Policy (admin)
```bash
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": false,
    "blocked_domains": [],
    "max_intent_notional_usd": 100.0,
    "max_total_notional_usd": 500.0,
    "updated_by": "openclaw.meta-agent",
    "reason": "Regime change: LowVol → HighVol, reducing limits"
  }' | jq .
```

### Policy History (admin)
```bash
curl -sf "$PLOY_API_BASE/api/governance/policy/history?limit=20" -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

---

## System Control

### Pause System (admin)
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/pause -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```
Optional body: `{ "domain": "crypto" }` to pause only one domain.

### Resume System (admin)
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/resume -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```
Optional body: `{ "domain": "crypto" }` to resume only one domain.

### Emergency Halt (admin)
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/halt -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```
**WARNING**: This force-closes all positions. Use only in emergencies.

---

## Strategy Deployments

### List Deployments (admin)
```bash
curl -sf $PLOY_API_BASE/api/deployments -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

### Enable/Disable Deployment (admin)
```bash
curl -sf -X POST $PLOY_API_BASE/api/deployments/{id}/enable -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
curl -sf -X POST $PLOY_API_BASE/api/deployments/{id}/disable -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

---

## Grok Decision (sidecar)

For NBA comeback and other research-driven decisions:
```bash
curl -sf -X POST $PLOY_API_BASE/api/sidecar/grok/decision \
  -H "Content-Type: application/json" \
  -H "x-ploy-sidecar-token: $PLOY_SIDECAR_AUTH_TOKEN" \
  -d '{
    "game_id": "...",
    "home_team": "Lakers",
    "away_team": "Celtics",
    "trailing_team": "Lakers",
    "trailing_abbrev": "LAL",
    "home_score": 85,
    "away_score": 100,
    "quarter": 3,
    "clock": "5:30",
    "deficit": 15,
    "market_slug": "...",
    "market_price": 0.15
  }' | jq .
```
Returns: `{ "request_id", "decision": "trade"|"pass", "fair_value", "edge", "confidence", "reasoning", "risk_factors": [] }`

---

## Stats

### Today Stats (no auth)
```bash
curl -sf $PLOY_API_BASE/api/stats/today | jq .
```

### PnL History (no auth)
```bash
curl -sf $PLOY_API_BASE/api/stats/pnl | jq .
```
