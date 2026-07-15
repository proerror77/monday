# Platform Hardening Design

**Date:** 2026-03-23

**Goal:** Finish the remaining production-hardening work on the workspace-based control-plane runtime so `ployd` can run 24x7 with clear metrics, actionable alerts, stale-source degradation, and tighter operator permissions.

## Scope

This design is for the current workspace/control-plane mainline, not the retired single-binary monolith. The runtime already has:

- `ployd` as the long-running daemon
- `ployctl` / `ploytui` / frontend on the same control-plane
- audit logging
- request rate limiting
- live reconcile backoff and `degraded / recovering` semantics
- admin token, browser session cookie, and sidecar read-only token

The missing production-hardening work is:

1. platform-internal metrics and alert state
2. feed / venue / claim / worker heartbeat tracking with stale-source auto-degrade
3. finer-grained operator permissions beyond `public / sidecar-readonly / admin`

## Options

### Option 1: Metrics only

Add a few counters to `SystemStatus` and leave stale-source and auth semantics mostly unchanged.

Pros:
- smallest diff

Cons:
- still no clear answer to "what is stale?"
- alerts remain log-shaped instead of operator-shaped
- auth still cannot distinguish read-only from operator-grade write actions

### Option 2: Platform-internal observability loop

Add first-class metrics and alerts to the daemon, track source heartbeats, project stale/degraded state into system status, expose them over HTTP/SSE/CLI/TUI/frontend, and tighten permissions into capability bands.

Pros:
- completes the current platform model instead of papering over it
- gives a usable 24x7 operating surface without external monitoring dependencies
- keeps the semantics inside the platform before exporting them elsewhere

Cons:
- touches contracts, daemon state, operator clients, and frontend

### Option 3: External monitoring first

Add Prometheus/Alertmanager or another external system before stabilizing internal alert semantics.

Pros:
- useful later for real ops integration

Cons:
- wrong order right now
- exports unstable semantics and multiplies cleanup work

## Recommendation

Use **Option 2**.

The platform needs one internal truth for health, stale sources, and operator-visible alerts before any external monitoring integration is worth doing.

## Design

### 1. Metrics and alert model

Add new operator contracts for:

- `PlatformMetrics`
- `ActiveAlert`
- `AlertSeverity`
- `AlertKind`
- `HeartbeatStatus`

The daemon should maintain a small in-memory health projection and publish it to:

- `/api/system/status` for coarse summary
- `/api/system/metrics` for structured metrics
- `/api/system/alerts` for current actionable alerts
- `/api/events/stream` as `metrics_snapshot` and `alert_snapshot`

The first metrics set should stay small and operational:

- total deployment count
- live deployment count
- degraded deployment count
- active alert count by severity
- live reconcile failure count
- pending auto-claim accounts
- stale heartbeat source count
- last successful live reconcile time
- last successful claim time

### 2. Heartbeat and stale-source degradation

Track source-level heartbeat instead of only global daemon health.

The first tracked sources should be:

- deployment worker heartbeat
- live reconcile loop
- venue connectivity
- claim loop

The daemon should evaluate source freshness on every tick and:

- raise/update alerts when a source becomes stale
- mark the system `degraded` while any critical source remains stale
- move through `recovering` when the source resumes

This should not stop the daemon. It should change health projection and operator visibility.

### 3. Auth scopes

Keep the current tokens and browser sessions, but refine required access into capability bands:

- `public`
- `read`
- `operator`
- `admin`

Mapping:

- `public`: `/health`, `/auth/*`
- `read`: system status, deployments, trading state, metrics, alerts, SSE
- `operator`: safe control-plane writes like deployment lifecycle changes and explicit claim controls
- `admin`: high-impact writes such as live trading intent ingress and order cancel/replace if we choose to keep those highest-privilege

The current sidecar token remains read-only. Admin token and signed browser session retain full access.

### 4. Operator surfaces

`ployctl`
- add `system metrics`
- add `system alerts`

`ploytui`
- show alert summary and stale-source summary in the system section

frontend
- render alert state and stale-source signals in `SystemControl`
- preserve EventSource-with-cookie flow; do not require custom headers for SSE

### 5. Documentation

Update startup/deploy runbooks so operators know:

- which env vars govern heartbeat/stale thresholds
- where to read active alerts
- what counts as degraded vs recovering
- which token class can read vs operate vs administer

## Execution shape

Land this in three atomic groups:

1. contracts + daemon health/metrics/alerts state
2. HTTP/SSE/CLI/TUI/frontend operator surfaces
3. auth scope refinement + runbook updates

## Non-goals

- external alert delivery (Telegram, email, Feishu)
- Prometheus exporter integration
- deep live execution model changes
- strategy or research-path changes
