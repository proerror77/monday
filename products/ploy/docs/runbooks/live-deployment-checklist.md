# Live Deployment Checklist

> **Historical standalone PLOY checklist.** Do not use this document to deploy or
> resume trading from Monday. The referenced nested workflows and former hosts are
> inactive migration evidence; PLOY live execution remains disabled.

## Goal

Validate a remote `ployd` host before enabling real live trading.

This checklist is the operator-facing go/no-go gate. It assumes:

- `ployd.service` is already installed on the host
- validation happens remotely through `/opt/ploy/bin/ployctl`
- the default acceptance path is a dry-run drill using a paper deployment

## Before You Start

- Confirm the trade host was installed from the named protected path:
  - `.github/workflows/deploy-trade.yml`
  - `deployment/ployd-trade.service`
  - an immutable `/opt/ploy/releases/<main-sha>` receipt
- Confirm the host has:
  - `/opt/ploy/bin/ployd`
  - `/opt/ploy/bin/ployctl`
  - `/opt/ploy/.env`
- Confirm `ployd` is the only long-running platform process.
- Confirm host-support timers are installed:
  - `ploy-platform-watchdog.timer`
  - `ploy-maintenance.timer`

## Required Host Inputs

At minimum, `/opt/ploy/.env` should define:

- `PLOY_ADMIN_TOKEN`
- `POLYMARKET_PRIVATE_KEY`

Recommended:

- `PLOY_OPERATOR_TOKEN` for remote `ployctl` mutation without full admin access
- `PLOY_API_AUTH_COOKIE_SECRET` if browser sessions are used
- `POLY_SIGNATURE_TYPE=proxy` with `POLY_WALLET_TYPE=PROXY`, or
  `POLY_SIGNATURE_TYPE=gnosis_safe` with `POLY_WALLET_TYPE=SAFE`
- `POLY_FUNDER` matching the signer-derived Proxy/Safe address

`poly1271`/`DEPOSIT` order signing is understood by the Rust adapter, but the
current custody/redemption relayer does not support that wallet route. The
deploy and live gates therefore reject it instead of presenting an incomplete
wallet lifecycle as live-ready.

## Baseline Service Checks

Run on the remote host:

```bash
systemctl is-active ployd
curl -fsS http://127.0.0.1:8081/health
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl system metrics
/opt/ploy/bin/ployctl system alerts
/opt/ploy/bin/ployctl system audit
```

Go only if:

- `ployd` is active
- `/health` returns success
- `ployctl system status` is `running`, `recovering`, or `degraded`
- no critical alerts are active

Stop if:

- `/health` fails
- `system status` is unexpected
- `system alerts` contains critical entries

## Dry-Run Acceptance Path

The default host acceptance path is:

1. Run the dry-run drill script:

```bash
/opt/ploy/scripts/drills/live_dry_run.sh
```

2. Confirm the script reports `PASS` or an understood `WARN`.
3. Review any warnings before considering real live enablement.

The drill uses a paper deployment and does not touch real funds.

## Manual Review Before Real Live Enablement

Only consider real live enablement after the dry-run drill passes and you have
manually reviewed:

- `ployctl system status`
- `ployctl system metrics`
- `ployctl system alerts`
- `ployctl trading status`
- `ployctl trading readiness 5`
- the most recent audit log entries

## PM5D ThreeLayer Live Gate

For the current PM5D ThreeLayer setup, stage the live deployment in paused mode:

```bash
/opt/ploy/scripts/drills/pm5d_threelayer_live_gate.sh
```

This verifies `02-pm5d-threelayer.live.toml` against the dry-run config, checks
the authenticated Polymarket account for geoblock, closed-only status, pUSD
balance, and both V2 allowances, applies `pm5d.threelayer.live` with
`desired_state=paused`, and stops before any live orders can be placed.
The readiness command derives an already-existing L2 API key with the read-only
endpoint, never creates one, and stops its temporary SDK heartbeat task before
the first venue probe. Missing credentials or a 15-second end-to-end timeout
fails the gate closed.

The staging script cannot resume live. Real live enablement is available only
through `.github/workflows/approve-live-trade.yml`, which requires successful
exact-SHA runtime replay, recorded replay/dry-run strict parity, positive
dry-run economics, bounded drawdown, no open dry-run positions, the protected
`ploy-trade-live` environment approval, and the explicit 5 USD risk
acknowledgement. A failed observed-running or fresh-venue postflight pauses the
deployment automatically.

The trade daemon also enforces this boundary: when
`PLOY_LIVE_APPROVAL_FILE` is configured, both a live `apply` with
`desired_state=running` and a later `resume` are rejected unless the protected
workflow has installed a short-lived root-owned receipt matching the deployment
wallet, 5 USD exposure cap, exact release SHA, and immutable live-config hash.
The workflow removes the pending receipt after the transition, so a later
pause requires a new evidence review and human approval.

## Not Covered By This Checklist

Manual execution of this checklist does not:

- place live orders
- cancel live venue orders
- trigger real redeem / claim flows
- replace a separate game-day rehearsal for production funds
