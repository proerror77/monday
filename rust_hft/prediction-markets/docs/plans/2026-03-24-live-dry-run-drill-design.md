# Live Dry-Run Drill Design

**Date:** 2026-03-24

## Goal

Add a repeatable remote-host acceptance path for the workspace control-plane runtime so an operator can validate a live host with `ployctl` before enabling real trading.

The flow must prove:

- `ployd` is healthy and reachable on the host
- auth, audit, metrics, and alerts are wired correctly
- a paper deployment can be applied, inspected, paused, resumed, and stopped
- stale-source / reconcile health is visible through the operator surface

The flow must not:

- place real live orders
- trigger real redeem / auto-claim actions
- depend on the retired single-binary runtime

## Constraints

- Target environment is a single remote host with `ployd.service` managed by `systemd`.
- Operator actions run through `/opt/ploy/bin/ployctl`.
- The drill should be safe to repeat on the same host.
- The drill should work with either admin or operator credentials, but it should not require browser login flows.
- Current `ployctl` on `origin/main` does not have `claims ...` commands, so claim readiness has to be inferred from `system status`, `system metrics`, and `system alerts`.

## Approaches Considered

### 1. Docs only

Write a checklist and rely on manual command entry.

Pros:

- smallest code change

Cons:

- too easy to skip steps
- hard to repeat consistently across hosts
- poor fit for pre-live game-day drills

### 2. Checklist plus repeatable drill script

Add a human-facing checklist and a machine-executable remote drill script that drives `ployctl`.

Pros:

- repeatable and operator-friendly
- keeps the live host acceptance path explicit
- does not require new daemon features

Cons:

- script needs careful cleanup to stay idempotent

### 3. Full live rehearsal

Bake real live order / redeem behavior into the acceptance path.

Pros:

- highest fidelity

Cons:

- crosses the user's dry-run boundary
- creates unnecessary financial and operational risk

## Chosen Design

Use approach 2.

The repo will gain a dedicated dry-run acceptance layer with four artifacts:

1. `docs/runbooks/live-deployment-checklist.md`
2. `docs/runbooks/live-dry-run-drill.md`
3. `scripts/drills/live_dry_run.sh`
4. `config/deployments/example.live.dry-run.json`

## Flow

### A. Remote live deployment checklist

This is the operator-facing go/no-go document. It covers:

- host prerequisites and required environment variables
- which credentials are required for remote `ployctl`
- how to distinguish dry-run acceptance from real live enablement
- what a passing host looks like before moving to manual live confirmation

### B. Dry-run drill script

The script runs on the remote host and performs five stages:

1. Baseline service checks
   - `systemctl is-active ployd`
   - `curl /health`
   - `ployctl system status`
   - `ployctl system metrics`
   - `ployctl system alerts`
   - `ployctl system audit`
2. Configuration presence checks
   - ensure `/opt/ploy/.env` exists
   - verify required operator/auth/live env keys are present
   - verify runtime directories and snapshot files exist
3. Paper deployment drill
   - apply a paper-mode manifest intended for live-host dry runs
   - inspect the deployment
   - pause / resume / stop it
4. Trading and claim readiness projection
   - inspect `ployctl trading status`
   - fail on active critical alerts
   - surface stale-source / reconcile issues from `system metrics` and `system alerts`
5. Final result
   - print `PASS`, `WARN`, or `FAIL`
   - state whether the host is ready to proceed to human live confirmation

### C. Manifest strategy

The sample manifest is named `example.live.dry-run.json` to make its purpose obvious, but it remains a `paper` deployment so the drill never touches real funds.

## Error Handling

- Any failed required step exits non-zero.
- Missing required credentials/config files fail immediately with a clear message.
- Active critical alerts fail the drill.
- Non-critical degraded states can produce `WARN` if they do not invalidate the dry-run path.
- Cleanup runs on exit to stop the drill deployment if it was created.

## Validation

Implementation is complete when:

- the script passes `bash -n`
- the script can run locally in a parameter-validation mode without side effects
- runbooks clearly separate deploy, dry-run acceptance, and real live enablement
- README routes operators to the new checklist and drill docs

## Out of Scope

- real live order / cancel / redeem rehearsal
- external alert channels
- new daemon APIs just for the drill
- rewriting `ployctl` to add claim-specific commands
