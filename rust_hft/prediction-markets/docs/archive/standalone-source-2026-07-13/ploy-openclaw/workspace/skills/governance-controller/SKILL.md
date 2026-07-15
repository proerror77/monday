---
name: governance-controller
description: Manages the Ploy governance policy including blocking intents, domain controls, notional limits, metadata updates, and agent pause/resume operations.
version: 1.0.0
user-invocable: true
metadata: {"openclaw":{"requires":{"bins":["curl","jq"]}}}
---

# Governance Controller

Manage the Ploy Coordinator's governance policy to control trading behavior across all agents.

## Governance Model

The governance policy is the primary control mechanism for the meta-agent:

| Field | Type | Description |
|-------|------|-------------|
| `block_new_intents` | bool | When true, no new orders can be submitted |
| `blocked_domains` | string[] | Domains that cannot trade (e.g., ["crypto", "sports"]) |
| `max_intent_notional_usd` | Decimal | Maximum USD value per single order |
| `max_total_notional_usd` | Decimal | Maximum total USD exposure |
| `metadata` | map | Free-form key-value pairs for agent signaling |
| `updated_by` | string | Who made the change (for audit trail) |
| `reason` | string | Why the change was made |

## Common Operations

### View Current Policy
```bash
curl -sf $PLOY_API_BASE/api/governance/policy -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

### View Full Governance Status
```bash
curl -sf $PLOY_API_BASE/api/governance/status -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

### View Policy Change History
```bash
curl -sf "$PLOY_API_BASE/api/governance/policy/history?limit=10" -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq '.[] | {updated_at, updated_by, reason, block_new_intents, max_intent_notional_usd}'
```

---

## Block / Unblock Intents

### Block all new intents (emergency brake)
```bash
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": true,
    "blocked_domains": [],
    "updated_by": "openclaw.governance",
    "reason": "<REASON>"
  }' | jq .
```

### Unblock intents (resume trading)
```bash
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": false,
    "blocked_domains": [],
    "updated_by": "openclaw.governance",
    "reason": "<REASON>"
  }' | jq .
```

### Block specific domains
```bash
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": false,
    "blocked_domains": ["sports", "politics"],
    "updated_by": "openclaw.governance",
    "reason": "Restricting to crypto-only during high vol"
  }' | jq .
```

---

## Adjust Notional Limits

### Tighten limits (reduce risk)
```bash
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": false,
    "blocked_domains": [],
    "max_intent_notional_usd": 50.0,
    "max_total_notional_usd": 200.0,
    "updated_by": "openclaw.governance",
    "reason": "Tightening limits: <REASON>"
  }' | jq .
```

### Loosen limits (increase opportunity)
```bash
curl -sf -X PUT $PLOY_API_BASE/api/governance/policy \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{
    "block_new_intents": false,
    "blocked_domains": [],
    "max_intent_notional_usd": 150.0,
    "max_total_notional_usd": 750.0,
    "updated_by": "openclaw.governance",
    "reason": "Loosening limits: <REASON>"
  }' | jq .
```

---

## Domain Control (Pause/Resume)

### Pause a domain
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/pause \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{"domain": "<crypto|sports|politics>"}' | jq .
```

### Resume a domain
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/resume \
  -H "Content-Type: application/json" \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" \
  -d '{"domain": "<crypto|sports|politics>"}' | jq .
```

### Pause entire system
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/pause \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

### Resume entire system
```bash
curl -sf -X POST $PLOY_API_BASE/api/system/resume \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

---

## Deployment Gate

### Disable a strategy deployment
```bash
curl -sf -X POST $PLOY_API_BASE/api/deployments/{deployment_id}/disable \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

### Enable a strategy deployment
```bash
curl -sf -X POST $PLOY_API_BASE/api/deployments/{deployment_id}/enable \
  -H "x-ploy-admin-token: $PLOY_API_ADMIN_TOKEN" | jq .
```

---

## Best Practices

1. **Always include a reason**: Every policy change should have a clear `reason` field for the audit trail.
2. **Always set `updated_by`**: Use `openclaw.<skill-name>` to identify the source.
3. **Check before changing**: Read current policy first to avoid overwriting changes made by other skills.
4. **Gradual changes**: Prefer reducing limits 50% before blocking entirely.
5. **Log to memory**: Record every governance change with timestamp and rationale.
6. **Never remove all limits**: Always maintain some max_intent and max_total limits.
