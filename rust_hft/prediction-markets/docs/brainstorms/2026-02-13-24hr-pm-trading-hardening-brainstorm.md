---
date: 2026-02-13
topic: 24hr-pm-trading-hardening
---

# 24hr PM Trading Hardening

## What We're Building
Harden the existing Polymarket trading runtime so it can operate as a 24/7 service with safer startup behavior, clearer deployment contracts, and a stable integration surface for OpenClaw-driven automation.

This work focuses on reliability and operability, not strategy alpha changes.

## Why This Approach
The fastest path to production safety is to fix failure semantics and deployment mismatches first:
- Fail fast when live mode cannot persist state.
- Remove ambiguous or half-wired runtime paths.
- Ensure infrastructure health checks reflect real endpoints.
- Keep OpenClaw control-plane hooks explicit and deterministic.

## Key Decisions
- DB fail-fast in live mode: do not silently downgrade to simple mode without persistence.
- Keep simple mode only for dry-run fallback.
- Wire sports agent DB dependency in coordinator bootstrap so enabled sports mode is actually runnable.
- Use a stable non-empty `market_slug` key for EventEdge decisions (`event_id`) to avoid blank routing/risk keys.
- Restrict `platform` action to `start` only so CLI contract matches implementation.
- Align deployment profile with real runtime endpoints:
  - Install `curl` in runtime image for healthchecks.
  - Use `/readyz` in compose healthcheck.
  - Mark `/api` and `/ws` as intentionally disabled in this profile.
- Force-enable EventEdge via daemon env (`PLOY_EVENT_EDGE_AGENT__ENABLED=true`) to avoid no-op daemon starts.

## Open Questions
- Whether to run a dedicated API service profile (feature-enabled + separate port/process) instead of the current health-only backend profile.
- Whether sports-mode should fail startup when DB is unavailable (current behavior) or auto-disable sports while keeping other agents running.
- Whether EventEdge should carry true market slug (if available) instead of event-level key.

## Next Steps
1. Run 24h soak test (network blips, DB restart, process restart).
2. Define a dedicated API deployment profile if frontend `/api` is required.
3. Add OpenClaw RPC idempotency + audit trail for write methods.
