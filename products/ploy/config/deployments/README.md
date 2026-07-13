# Deployment Manifests

`config/deployments/` is reserved for deployment manifests.

Each manifest should describe one deployment resource.
The current minimal operator path uses JSON manifests such as
`config/deployments/example.paper.json`.

Current examples:

- `example.paper.json`
  - local or remote paper deployment smoke path
- `example.live.dry-run.json`
  - remote live-host readiness drill
  - still uses `runtime_mode: "paper"` so the drill never touches real funds
- `pm5d.threelayer.settlement-probability-btc-eth.dryrun.json`
  - BTC/ETH-only PM5D/PM15D settlement-probability dry-run handoff candidate
  - paper runtime only; not live approval
  - records its own MarketUpdate stream at
    `/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson`
    so recorded replay parity can use the same deployed strategy and time
    window

Each manifest includes:

- `deployment_id`
- `bundle_id`
- `runtime_mode`
- `desired_state`

Current bundle resolution rule:

- if `bundle_id` ends with `.toml` or contains a path separator, the deployment
  worker treats it as a config path (relative paths resolve from the platform
  working directory)
- otherwise the deployment worker resolves it as
  `config/strategies/<bundle_id>.toml`

Example:

- `bundle_id: "02-pm5d.v3-dryrun"` resolves to
  `config/strategies/02-pm5d.v3-dryrun.toml`

The operator manages deployment resources. The platform manages the worker processes behind them.

Do not point the dry-run drill at a real live manifest. The drill is meant to
prove host readiness, not to enable production trading.
