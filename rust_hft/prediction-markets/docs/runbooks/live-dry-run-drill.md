# Live Dry-Run Drill

> **Historical standalone PLOY drill.** It is retained for provenance and does not
> define a Monday deployment or promotion path. PLOY live execution remains disabled.

## Goal

Run a repeatable remote-host readiness drill for a future live deployment
without placing real orders.

The drill is intentionally conservative:

- it runs through `ployctl`
- it validates the deployed `ployd` host
- it applies a paper deployment only
- it never submits real live trading or redeem actions

## What The Script Checks

`scripts/drills/live_dry_run.sh` performs:

1. daemon baseline
   - `systemctl is-active ployd`
   - `/health`
   - `ployctl system status`
   - `ployctl system metrics`
   - `ployctl system alerts`
   - `ployctl system audit`
2. config and credential presence
   - `/opt/ploy/.env`
   - operator/admin token presence
   - live signing env presence
   - runtime snapshot files
3. paper deployment drill
   - apply
   - inspect
   - pause
   - resume
   - stop
4. trading readiness projection
   - `ployctl trading status`
   - stale-source and reconcile warning output

## What The Script Does Not Do

The drill does not:

- place live orders
- cancel or replace live venue orders
- run a real redeem / claim loop
- prove venue-side money movement

It is a host readiness drill, not a production trading rehearsal.

## Sample Manifest

The default drill manifest is:

- [`config/deployments/example.live.dry-run.json`](../../config/deployments/example.live.dry-run.json)

It is named after the live-host drill but runs in `paper` mode on purpose.

## Remote Usage

Run on the host:

```bash
/opt/ploy/scripts/drills/live_dry_run.sh
```

Optional overrides:

```bash
/opt/ploy/scripts/drills/live_dry_run.sh \
  --host-root /opt/ploy \
  --addr http://127.0.0.1:8081 \
  --manifest /opt/ploy/config/deployments/example.live.dry-run.json \
  --deployment-id example.live.dry-run
```

## Result Interpretation

- `PASS`
  - the host passed the dry-run and is ready for manual live go/no-go review
- `WARN`
  - the host passed baseline checks, but there are degraded or stale signals to review
- `FAIL`
  - the host is not ready to proceed toward real live enablement

Critical alerts fail the drill immediately.

## Follow-On Action

If the drill passes, return to:

- [`docs/runbooks/live-deployment-checklist.md`](./live-deployment-checklist.md)
- [`docs/runbooks/platform-deploy.md`](./platform-deploy.md)

and complete the remaining operator review before any real live deployment is enabled.
