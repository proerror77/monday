# Platform Startup Runbook

## Goal

Start the single-host trading platform runtime with:

- one `ployd` daemon
- one or more managed deployment workers
- one operator client path through `ployctl`
- one optional terminal console through `ploytui`

This runbook covers daemon startup and local/operator smoke checks. Remote
live-host acceptance is documented separately in:

- [`live-deployment-checklist.md`](./live-deployment-checklist.md)
- [`live-dry-run-drill.md`](./live-dry-run-drill.md)

## Default Local Flow

1. Check the daemon boots:

```bash
cargo run -p new-ployd
```

Export `PLOY_ADMIN_TOKEN`, `PLOY_API_ADMIN_TOKEN`, or `PLOY_API_KEY` before
starting the daemon. Protected control-plane routes fail closed when no token is
configured.
Optionally set `PLOY_OPERATOR_TOKEN` for write-capable operator automation, or
`PLOY_SIDECAR_AUTH_TOKEN` for read-only sidecar/agent access. `ployctl`
automatically prefers admin credentials, then operator credentials, then the
sidecar token. The frontend login
flow uses `/auth/login`, which sets an `HttpOnly` same-site signed session
cookie so `/api/events/stream` stays authenticated. Set
`PLOY_API_AUTH_COOKIE_SECRET` too if you want those browser sessions to survive
daemon restarts. Set `PLOY_REQUEST_RATE_LIMIT_PER_MINUTE` if you need a tighter
or looser daemon-side HTTP throttle; `0` disables it. Set
`PLOY_LIVE_RECONCILE_BACKOFF_BASE_MS` and
`PLOY_LIVE_RECONCILE_BACKOFF_MAX_MS` if you need to tune live venue reconcile
retry backoff during exchange/API outages.

2. In a second shell, check the operator client against the control-plane surface:

```bash
cargo run -p ployctl -- system status
cargo run -p ployctl -- system metrics
cargo run -p ployctl -- system alerts
cargo run -p ployctl -- system audit
cargo run -p ployctl -- trading status
cargo run -p ployctl -- deployments list
cargo run -p ploytui
curl -N -H "Authorization: Bearer ${PLOY_ADMIN_TOKEN:-${PLOY_API_ADMIN_TOKEN:-${PLOY_API_KEY}}}" http://127.0.0.1:8081/api/events/stream
```

3. Run the smoke test:

```bash
rtk cargo test --test platform_smoke -- --nocapture
```

## Intended Operator Contract

The current operator flow is:

```bash
new-ployd
ployctl deployments apply config/deployments/example.paper.json
ployctl system status
ployctl system metrics
ployctl system alerts
ployctl system audit
ployctl trading status
ployctl deployments list
ployctl deployments inspect example.paper
ployctl trading cancel example.live <order-id>
ploytui
curl -N -H "Authorization: Bearer ${PLOY_ADMIN_TOKEN:-${PLOY_API_ADMIN_TOKEN:-${PLOY_API_KEY}}}" http://127.0.0.1:8081/api/events/stream
ployctl deployments pause example.paper
ployctl deployments resume example.paper
ployctl deployments stop example.paper
```

This branch now treats `new-ployd` as the default local daemon entrypoint and
`ployctl` as an HTTP-first operator client with snapshot fallback. `ploytui` is
the thin terminal console on top of the same control-plane API. Deployment CRUD
is still simplified, but the daemon/client/runtime contract is no longer a
placeholder. The deployed host binary remains `ployd` and keeps an append-only `run/platform/audit-log.jsonl`
for authenticated control-plane actions, and `ployctl system audit` reads the
latest entries back over `/api/audit/logs`. `ployctl system status` now also
shows `live_reconcile_failures`, `next_live_reconcile_at`, and
`last_live_reconcile_error`, which are the primary live-venue outage signals.

For a remote host that is intended to run future live trading, the default next
step after startup is the dry-run acceptance drill:

```bash
/opt/ploy/scripts/drills/live_dry_run.sh
```
