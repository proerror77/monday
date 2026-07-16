# Platform Deploy Runbook

> **Historical standalone PLOY runbook.** The workflows and host paths below are
> retained as migration evidence only. They are nested under `rust_hft/prediction-markets`, are
> not executed by GitHub, and are not approved Monday deployment entrypoints.
> Monday `rust_hft` remains the only production execution authority.

## Historical Deployment Paths

The former standalone Aliyun host mutation topology was split by role:

- `.github/workflows/deploy-tango-1-1.yml`: data collection, research,
  replay, and dry-run only. The bundle contains no live manifest, live config,
  signing key, or live-resume gate.
- `.github/workflows/deploy-trade.yml`: immutable exact-main-SHA trade control
  plane, installed under `/opt/ploy/releases/<sha>` and staged paused.
- `.github/workflows/approve-live-trade.yml`: the former live resume path, behind
  artifact validation and the protected `ploy-trade-live` human environment.

`.github/workflows/release-platform.yml` was build-only. It packaged and
checksummed the portable platform bundle without remote host credentials or a
deployment job.

## Host Identity Bootstrap

Both SSH deploy paths require alias-keyed pinned host material:
`TANGO_1_1_KNOWN_HOSTS` for `tango-1-1` and
`PLOY_TRADE_1_KNOWN_HOSTS` for `ploy-trade-1`. Never create these secrets from
an unauthenticated `ssh-keyscan` result alone.

When the trade-host secret is missing or the ECS host keys rotate, dispatch
`.github/workflows/bootstrap-aliyun-host-keys.yml` from `main`. The workflow
resolves the host to exactly one ECS instance, reads `/etc/ssh/ssh_host_*_key.pub`
through authenticated Aliyun Cloud Assistant, and requires the public network
keys to match that attestation. Download the resulting
`ploy-trade-1-known-hosts` artifact, inspect its fingerprints in the workflow
log, then set the repository secret:

```bash
gh secret set PLOY_TRADE_1_KNOWN_HOSTS < attested-known-hosts.txt
```

The bootstrap workflow only emits public host keys. It does not deploy, alter
the instance, weaken `StrictHostKeyChecking`, or expose SSH private keys.

The trade path deploys three binaries:

- `/opt/ploy/bin/ployd`
- `/opt/ploy/bin/ployctl`
- `/opt/ploy/bin/ploy-runner`

`ployd` is the only systemd platform process. It supervises the runner;
`ployctl` is the operator client used for post-deploy verification and remote
inspection.

Deployment resources are managed separately from the daemon process. Operators
apply a deployment manifest and then use `ployctl` to inspect or change desired
state.

This runbook covers deploy/install only. Remote host acceptance now has its own
path:

- [`live-deployment-checklist.md`](./live-deployment-checklist.md)
- [`live-dry-run-drill.md`](./live-dry-run-drill.md)

## CI Bundle Contents

The release bundle contains:

- `bin/ployd`
- `bin/ployctl`
- `bin/ploytui`
- `deployment/ployd.service`
- `deployment/ploy-maintenance.service`
- `deployment/ploy-maintenance.timer`
- `deployment/ploy-platform-watchdog.service`
- `deployment/ploy-platform-watchdog.timer`
- `scripts/install-platform-service.sh`
- `scripts/ploy_maintenance.sh`
- `scripts/ploy_platform_watchdog.sh`
- `data/state/deployments.json.sample`

## Trade Host Installation Flow

After the bundle lands on `ploy-trade-1`, the deploy workflow:

1. Verifies the CI bundle checksum and refuses to overwrite an existing SHA
   release with different bytes.
2. Installs the bundle under `/opt/ploy/releases/<sha>` and atomically switches
   `/opt/ploy/current`, with rollback on failed postflight.
3. Installs `deployment/ployd-trade.service` with exact restart, memory, and OOM
   guardrails; no Rust build occurs on-host.
4. Derives and verifies the execution principal, renders the wallet-bound live
   manifest in memory, and applies it with `desired_state=paused`.
5. Verifies the release receipt, health endpoint, operator surfaces, exact
   systemd guardrails, absence of `cargo`/`rustc`, and
   `desired=Paused observed=Paused`.

Before changing a release, the trade workflow pauses the canonical live
deployment, stops `ployd`, and rewrites every persisted live desired state to
paused. Deploy and live approval share the `ploy-trade-host` concurrency group.
Release files and stable config aliases remain root-owned; `ploy` receives write
access only to state/run/log paths. Rollback restores the previous symlink,
service unit, and environment while keeping live paused.

## Required Host Paths

The install script ensures:

- `/opt/ploy/.env`
- `/opt/ploy/data/state/deployments.json`
- `/opt/ploy/run/platform/system-status.json`
- `/opt/ploy/run/platform/deployments.json`
- `/opt/ploy/run/platform/trading-state.json`
- `/opt/ploy/run/platform/audit-log.jsonl`

Optional hardening:

- Set `PLOY_ADMIN_TOKEN=...` in `/opt/ploy/.env` to require `Authorization: Bearer ...`
  or `x-ploy-admin-token` on the control-plane API.
- Set `PLOY_OPERATOR_TOKEN=...` in `/opt/ploy/.env` if automation or `ployctl`
  should be allowed to mutate deployments and trading controls without gaining
  full admin access.
- Set `PLOY_SIDECAR_AUTH_TOKEN=...` in `/opt/ploy/.env` if agent/sidecar clients
  only need read-only access to platform snapshots and `/api/events/stream`.
- Set `PLOY_API_AUTH_COOKIE_SECRET=...` in `/opt/ploy/.env` if browser operator
  sessions need to stay valid across daemon restarts or multiple `ployd`
  instances.
- Set `PLOY_REQUEST_RATE_LIMIT_PER_MINUTE=...` in `/opt/ploy/.env` if you want
  to tune daemon-side HTTP throttling. Set `0` to disable it.
- Set `PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS=...` and
  `PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS=...` in `/opt/ploy/.env` if you want to
  tune live venue reconcile retry backoff under exchange/API outages.
- `/opt/ploy/bin/ployctl` will automatically pick up `PLOY_ADMIN_TOKEN`,
  `PLOY_API_ADMIN_TOKEN`, `PLOY_API_KEY`, `PLOY_OPERATOR_TOKEN`,
  `PLOY_API_OPERATOR_TOKEN`, or `PLOY_SIDECAR_AUTH_TOKEN` from the host
  environment.
- Browser operator sessions should authenticate through `/auth/login`; `ployd`
  will issue an `HttpOnly` same-site signed session cookie so the frontend and
  SSE event stream remain authenticated without persisting the raw admin token
  in browser storage.
- Access bands are `public`, `read_only`, `operator`, and `admin`. `sidecar`
  tokens stop at `read_only`; `operator` tokens can drive deployment controls
  and trading mutations; only `admin` can read audit logs or mint browser
  sessions through `/auth/login`.

## Remote Acceptance

After deployment succeeds, do not enable real live trading immediately.

Run:

1. [`live-deployment-checklist.md`](./live-deployment-checklist.md)
2. [`live-dry-run-drill.md`](./live-dry-run-drill.md)

The dry-run drill uses a paper deployment on the live host so you can verify
control-plane and worker behavior without touching real funds.

## Post-Deploy Checks

```bash
sudo systemctl status ployd --no-pager
sudo journalctl -u ployd -n 200 --no-pager
curl -fsS http://127.0.0.1:8081/health
/opt/ploy/bin/ployctl system status
/opt/ploy/bin/ployctl system metrics
/opt/ploy/bin/ployctl system alerts
/opt/ploy/bin/ployctl system audit
/opt/ploy/bin/ployctl trading status
/opt/ploy/bin/ployctl deployments list
/opt/ploy/bin/ploytui
curl -N http://127.0.0.1:8081/api/events/stream
```

When `system status` shows non-zero `live_reconcile_failures`, a future
`next_live_reconcile_at`, or a non-empty `last_live_reconcile_error`, treat the
daemon as live-venue degraded even if the process is still up.

## Deployment Operator Flow

Use a deployment manifest like
[`config/deployments/example.paper.json`](../../config/deployments/example.paper.json)
as the template for remote deployment resources.

For remote live-host readiness checks, use the paper-mode drill manifest
[`config/deployments/example.live.dry-run.json`](../../config/deployments/example.live.dry-run.json)
and the drill script from [`live-dry-run-drill.md`](./live-dry-run-drill.md).

```bash
/opt/ploy/bin/ployctl deployments apply /opt/ploy/config/deployments/example.paper.json
/opt/ploy/bin/ployctl deployments inspect example.paper
/opt/ploy/bin/ployctl trading cancel example.live <order-id>
/opt/ploy/bin/ployctl deployments pause example.paper
/opt/ploy/bin/ployctl deployments resume example.paper
/opt/ploy/bin/ployctl deployments stop example.paper
```

## Legacy Workflows

Older workflows that still build or deploy the single-binary `ploy` path are
legacy only. They are not the default release path for the trading-platform
workspace.
